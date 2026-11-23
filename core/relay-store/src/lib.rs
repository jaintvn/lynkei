//! relay-store: durable, searchable traffic history (SQLite + FTS5 trigram index).
//!
//! Same schema on macOS and Android. Every transaction is stored as JSON plus a few indexed
//! columns; a trigram full-text index over URL, headers, body previews and errors narrows text
//! searches, and the shared [`Filter`] decides the exact match.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use relay_model::filter::Filter;
use relay_model::{Transaction, TransactionState};
use relay_proxy::EventSink;
use rusqlite::{Connection, OptionalExtension, params};

pub const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone)]
pub struct StoreConfig {
    /// Oldest transactions beyond this count are deleted.
    pub max_rows: u64,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self { max_rows: 500_000 }
    }
}

/// A stored transaction: `key` is unique across engine runs (transaction ids restart per run).
#[derive(Debug, Clone)]
pub struct Stored {
    pub key: i64,
    pub session: String,
    pub tx: Transaction,
}

pub struct Store {
    conn: Mutex<Connection>,
    cfg: StoreConfig,
}

impl Store {
    pub fn open(path: &Path, cfg: StoreConfig) -> Result<Arc<Store>> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::init(conn, cfg)
    }

    pub fn open_in_memory(cfg: StoreConfig) -> Result<Arc<Store>> {
        Self::init(Connection::open_in_memory()?, cfg)
    }

    fn init(conn: Connection, cfg: StoreConfig) -> Result<Arc<Store>> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS tx (
                 key INTEGER PRIMARY KEY,
                 session TEXT NOT NULL,
                 id INTEGER NOT NULL,
                 started_at_ms INTEGER NOT NULL,
                 host TEXT NOT NULL,
                 method TEXT NOT NULL,
                 status INTEGER,
                 state TEXT NOT NULL,
                 duration_ms INTEGER NOT NULL,
                 response_size INTEGER NOT NULL,
                 json TEXT NOT NULL,
                 UNIQUE (session, id)
             );
             CREATE INDEX IF NOT EXISTS tx_host ON tx (host);
             CREATE VIRTUAL TABLE IF NOT EXISTS tx_fts USING fts5 (doc, tokenize = 'trigram');",
        )?;
        let version: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        match version.as_deref() {
            None => {
                conn.execute(
                    "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
                    [SCHEMA_VERSION.to_string()],
                )?;
            }
            Some(v) if v.parse::<i64>().unwrap_or(0) > SCHEMA_VERSION => {
                anyhow::bail!(
                    "traffic store was written by a newer Relay (schema {v}); refusing to downgrade it"
                );
            }
            Some(_) => {}
        }
        Ok(Arc::new(Store {
            conn: Mutex::new(conn),
            cfg,
        }))
    }

    /// Inserts or replaces (same session + id) a batch in one SQLite transaction.
    pub fn upsert(&self, session: &str, batch: &[Transaction]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let db = conn.transaction()?;
        {
            let mut up = db.prepare_cached(
                "INSERT INTO tx (session, id, started_at_ms, host, method, status, state, duration_ms, response_size, json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT (session, id) DO UPDATE SET
                    started_at_ms = excluded.started_at_ms, host = excluded.host, method = excluded.method,
                    status = excluded.status, state = excluded.state, duration_ms = excluded.duration_ms,
                    response_size = excluded.response_size, json = excluded.json
                 RETURNING key",
            )?;
            let mut del = db.prepare_cached("DELETE FROM tx_fts WHERE rowid = ?1")?;
            let mut ins = db.prepare_cached("INSERT INTO tx_fts (rowid, doc) VALUES (?1, ?2)")?;
            for t in batch {
                let key: i64 = up.query_row(
                    params![
                        session,
                        t.id as i64,
                        t.started_at_ms as i64,
                        t.host,
                        t.method,
                        t.status.map(|s| s as i64),
                        format!("{:?}", t.state),
                        t.duration_ms as i64,
                        t.response_body_size as i64,
                        serde_json::to_string(t)?,
                    ],
                    |r| r.get(0),
                )?;
                del.execute([key])?;
                ins.execute(params![key, search_doc(t)])?;
            }
        }
        db.commit()?;
        Ok(())
    }

    /// Newest first. `filter` decides the match; the text index narrows the scan for words of 3+ characters.
    pub fn search(&self, filter: &Filter, limit: usize) -> Result<Vec<Stored>> {
        const PAGE: usize = 500;
        const MAX_SCAN: usize = 200_000;
        let needles: Vec<String> = filter
            .text_needles()
            .iter()
            .filter(|n| n.chars().count() >= 3)
            .map(|n| fts_phrase(n))
            .collect();
        let fts = needles.join(" AND ");
        let conn = self.conn.lock().unwrap();
        let mut out = Vec::new();
        let mut offset = 0;
        while out.len() < limit && offset < MAX_SCAN {
            let rows: Vec<(i64, String, String)> = if fts.is_empty() {
                let mut q = conn.prepare_cached(
                    "SELECT key, session, json FROM tx ORDER BY key DESC LIMIT ?1 OFFSET ?2",
                )?;
                q.query_map(params![PAGE as i64, offset as i64], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?
                .collect::<Result<_, _>>()?
            } else {
                let mut q = conn.prepare_cached(
                    "SELECT tx.key, tx.session, tx.json FROM tx_fts JOIN tx ON tx.key = tx_fts.rowid
                     WHERE tx_fts MATCH ?1 ORDER BY tx.key DESC LIMIT ?2 OFFSET ?3",
                )?;
                q.query_map(params![fts, PAGE as i64, offset as i64], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?
                .collect::<Result<_, _>>()?
            };
            let n = rows.len();
            for (key, session, json) in rows {
                let tx: Transaction = match serde_json::from_str(&json) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!(key, error = %e, "skipping unreadable stored transaction");
                        continue;
                    }
                };
                if filter.matches(&tx) {
                    out.push(Stored { key, session, tx });
                    if out.len() == limit {
                        break;
                    }
                }
            }
            if n < PAGE {
                break;
            }
            offset += PAGE;
        }
        Ok(out)
    }

    pub fn get(&self, key: i64) -> Result<Option<Stored>> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(String, String)> = conn
            .query_row("SELECT session, json FROM tx WHERE key = ?1", [key], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        Ok(match row {
            Some((session, json)) => Some(Stored {
                key,
                session,
                tx: serde_json::from_str(&json)?,
            }),
            None => None,
        })
    }

    pub fn count(&self) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row("SELECT COUNT(*) FROM tx", [], |r| r.get::<_, i64>(0))? as u64)
    }

    /// Deletes the oldest rows beyond `max_rows`. Returns how many were removed.
    pub fn prune(&self) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let cutoff: Option<i64> = conn
            .query_row(
                "SELECT key FROM tx ORDER BY key DESC LIMIT 1 OFFSET ?1",
                [self.cfg.max_rows as i64],
                |r| r.get(0),
            )
            .optional()?;
        let Some(cutoff) = cutoff else { return Ok(0) };
        conn.execute("DELETE FROM tx_fts WHERE rowid <= ?1", [cutoff])?;
        Ok(conn.execute("DELETE FROM tx WHERE key <= ?1", [cutoff])? as u64)
    }

    pub fn clear(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch("DELETE FROM tx; DELETE FROM tx_fts;")?;
        Ok(())
    }
}

/// Lowercased text the trigram index searches: URL, headers, previews, error.
fn search_doc(t: &Transaction) -> String {
    let mut doc = String::with_capacity(512);
    doc.push_str(&t.url());
    for h in t.request_headers.iter().chain(&t.response_headers) {
        doc.push('\n');
        doc.push_str(&h.name);
        doc.push_str(": ");
        doc.push_str(&h.value);
    }
    for part in [&t.request_body_preview, &t.response_body_preview, &t.error]
        .into_iter()
        .flatten()
    {
        doc.push('\n');
        doc.push_str(part);
    }
    doc.to_lowercase()
}

fn fts_phrase(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

enum Msg {
    Tx(Box<Transaction>),
    Flush(SyncSender<()>),
}

/// An [`EventSink`] that persists every event (batched on a background thread) and forwards it.
pub struct StoreSink {
    queue: SyncSender<Msg>,
    forward: Option<Arc<dyn EventSink>>,
    dropped: std::sync::atomic::AtomicU64,
}

impl StoreSink {
    /// `session` identifies this engine run; `forward` (e.g. the UI) also receives every event.
    pub fn new(
        store: Arc<Store>,
        session: String,
        forward: Option<Arc<dyn EventSink>>,
    ) -> Arc<StoreSink> {
        Self::with_capacity(store, session, forward, 20_000)
    }

    /// `capacity` events may wait for the disk; beyond that, history is dropped (and counted)
    /// rather than ever blocking the proxy — connectivity comes first.
    pub fn with_capacity(
        store: Arc<Store>,
        session: String,
        forward: Option<Arc<dyn EventSink>>,
        capacity: usize,
    ) -> Arc<StoreSink> {
        let (queue, rx) = mpsc::sync_channel::<Msg>(capacity);
        std::thread::Builder::new()
            .name("relay-store".into())
            .spawn(move || writer(store, session, rx))
            .expect("spawning store writer");
        Arc::new(StoreSink {
            queue,
            forward,
            dropped: Default::default(),
        })
    }

    /// Events not stored because the disk could not keep up.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Blocks until everything sent so far is on disk.
    pub fn flush(&self) {
        let (tx, rx) = mpsc::sync_channel(1);
        if self.queue.send(Msg::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(10));
        }
    }
}

impl EventSink for StoreSink {
    fn on_transaction(&self, t: Transaction) {
        if let Some(f) = &self.forward {
            f.on_transaction(t.clone());
        }
        // Never block an engine thread on the disk.
        if let Err(mpsc::TrySendError::Full(_)) = self.queue.try_send(Msg::Tx(Box::new(t))) {
            let n = self
                .dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1;
            if n == 1 || n.is_power_of_two() {
                tracing::warn!(
                    dropped = n,
                    "traffic store is behind; some history is not being saved"
                );
            }
        }
    }
}

fn writer(store: Arc<Store>, session: String, rx: mpsc::Receiver<Msg>) {
    const BATCH: usize = 500;
    let mut since_prune = 0usize;
    loop {
        let first = match rx.recv() {
            Ok(m) => m,
            Err(_) => return,
        };
        let mut batch = Vec::new();
        let mut flushes = Vec::new();
        let push = |m: Msg, batch: &mut Vec<Transaction>, flushes: &mut Vec<SyncSender<()>>| match m
        {
            Msg::Tx(t) => {
                let t = *t;
                // A later event for the same id replaces an earlier one in the same batch.
                if let Some(e) = batch.iter_mut().find(|e: &&mut Transaction| e.id == t.id) {
                    *e = t;
                } else {
                    batch.push(t);
                }
            }
            Msg::Flush(f) => flushes.push(f),
        };
        push(first, &mut batch, &mut flushes);
        while batch.len() < BATCH {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(m) => push(m, &mut batch, &mut flushes),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        if !batch.is_empty() {
            if let Err(e) = store.upsert(&session, &batch) {
                tracing::error!(error = %e, "writing traffic to the store failed");
            }
            since_prune += batch.len();
            if since_prune >= 1_000 {
                since_prune = 0;
                let _ = store.prune();
            }
        }
        for f in flushes {
            let _ = f.send(());
        }
    }
}

/// Count of transactions by state, for status views.
pub fn summarize(rows: &[Stored]) -> (usize, usize) {
    let failed = rows
        .iter()
        .filter(|r| r.tx.state == TransactionState::Failed || r.tx.status.unwrap_or(0) >= 500)
        .count();
    (rows.len(), failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_model::Header;

    fn tx(id: u64, method: &str, path: &str, status: u16, ms: u64, body: &str) -> Transaction {
        Transaction {
            id,
            started_at_ms: 1_000 + id,
            duration_ms: ms,
            method: method.into(),
            scheme: "https".into(),
            host: "api.acmeshop.com".into(),
            port: 443,
            path: path.into(),
            status: Some(status),
            state: TransactionState::Complete,
            error: None,
            decrypted: true,
            http_version: "HTTP/2".into(),
            upstream_http_version: Some("HTTP/2".into()),
            request_headers: vec![Header {
                name: "x-app-version".into(),
                value: "3.4.1".into(),
            }],
            response_headers: vec![],
            request_body_size: 0,
            response_body_size: body.len() as u64,
            request_body_preview: None,
            response_body_preview: Some(body.into()),
            client_addr: "10.0.0.2:5000".into(),
            websocket_messages: vec![],
            client_process: None,
            applied_rules: vec![],
        }
    }

    fn q(store: &Store, f: &str) -> Vec<u64> {
        store
            .search(&Filter::parse(f).unwrap(), 100)
            .unwrap()
            .into_iter()
            .map(|s| s.tx.id)
            .collect()
    }

    fn sample() -> Arc<Store> {
        let s = Store::open_in_memory(StoreConfig::default()).unwrap();
        s.upsert(
            "run1",
            &[
                tx(
                    1,
                    "GET",
                    "/v2/products?page=2",
                    200,
                    142,
                    r#"{"items":[{"name":"Runner Pro 2"}]}"#,
                ),
                tx(
                    2,
                    "POST",
                    "/v2/checkout",
                    500,
                    1840,
                    r#"{"error":"PAYMENT_PROVIDER_TIMEOUT"}"#,
                ),
                tx(
                    3,
                    "POST",
                    "/oauth/token",
                    401,
                    64,
                    r#"{"error":"invalid_grant"}"#,
                ),
            ],
        )
        .unwrap();
        s
    }

    #[test]
    fn filters_and_full_text_search() {
        let s = sample();
        assert_eq!(q(&s, ""), vec![3, 2, 1], "newest first");
        assert_eq!(q(&s, "status:5xx duration:>800ms"), vec![2]);
        assert_eq!(q(&s, "method:POST -status:401"), vec![2]);
        assert_eq!(
            q(&s, "provider_timeout"),
            vec![2],
            "body text through the trigram index"
        );
        assert_eq!(q(&s, "RUNNER PRO"), vec![1], "case-insensitive");
        assert_eq!(q(&s, r#"body:"invalid_grant""#), vec![3]);
        assert_eq!(q(&s, "checkout status:200"), Vec::<u64>::new());
        assert_eq!(q(&s, "header:x-app-version=3.4"), vec![3, 2, 1]);
    }

    #[test]
    fn in_progress_rows_are_replaced_by_their_final_version() {
        let s = sample();
        let mut open = tx(4, "GET", "/v2/recommendations", 200, 5, "");
        open.state = TransactionState::InProgress;
        s.upsert("run1", &[open.clone()]).unwrap();
        assert_eq!(q(&s, "state:open"), vec![4]);
        let mut done = open;
        done.state = TransactionState::Complete;
        done.response_body_preview = Some(r#"{"recommended":[]}"#.into());
        s.upsert("run1", &[done]).unwrap();
        assert_eq!(s.count().unwrap(), 4, "same session + id is one row");
        assert_eq!(q(&s, "state:open"), Vec::<u64>::new());
        assert_eq!(
            q(&s, "recommended"),
            vec![4],
            "the index follows the update"
        );
    }

    #[test]
    fn ids_restart_per_session_without_clashing() {
        let s = sample();
        s.upsert("run2", &[tx(1, "GET", "/v2/categories", 304, 40, "")])
            .unwrap();
        assert_eq!(s.count().unwrap(), 4);
        let rows = s
            .search(&Filter::parse("path:/v2/categories").unwrap(), 10)
            .unwrap();
        assert_eq!((rows.len(), rows[0].session.as_str()), (1, "run2"));
        assert_eq!(
            s.get(rows[0].key).unwrap().unwrap().tx.path,
            "/v2/categories"
        );
    }

    #[test]
    fn prunes_oldest_beyond_the_cap() {
        let s = Store::open_in_memory(StoreConfig { max_rows: 2 }).unwrap();
        s.upsert(
            "r",
            &[
                tx(1, "GET", "/a", 200, 1, "one"),
                tx(2, "GET", "/b", 200, 1, "two"),
                tx(3, "GET", "/c", 200, 1, "three"),
            ],
        )
        .unwrap();
        assert_eq!(s.prune().unwrap(), 1);
        assert_eq!(q(&s, ""), vec![3, 2]);
        assert_eq!(
            q(&s, "one"),
            Vec::<u64>::new(),
            "pruned rows leave the index too"
        );
    }

    #[test]
    fn persists_across_reopen_and_refuses_newer_schema() {
        let dir = std::env::temp_dir().join(format!("relay-store-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("relay.db");
        {
            let s = Store::open(&path, StoreConfig::default()).unwrap();
            s.upsert("r", &[tx(1, "GET", "/persisted", 200, 1, "kept")])
                .unwrap();
        }
        let s = Store::open(&path, StoreConfig::default()).unwrap();
        assert_eq!(q(&s, "persisted"), vec![1]);
        drop(s);
        let c = Connection::open(&path).unwrap();
        c.execute(
            "UPDATE meta SET value = '99' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
        drop(c);
        assert!(
            Store::open(&path, StoreConfig::default())
                .err()
                .unwrap()
                .to_string()
                .contains("newer Relay")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sink_batches_writes_and_forwards_events() {
        let s = Store::open_in_memory(StoreConfig::default()).unwrap();
        let seen = Arc::new(Mutex::new(0usize));
        let seen2 = seen.clone();
        let fwd: Arc<dyn EventSink> = Arc::new(move |_t: Transaction| *seen2.lock().unwrap() += 1);
        let sink = StoreSink::new(s.clone(), "run".into(), Some(fwd));
        for i in 0..2_000 {
            let mut t = tx(i, "GET", "/v2/products/{id}", 200, 10, "x");
            t.state = TransactionState::InProgress;
            sink.on_transaction(t.clone());
            t.state = TransactionState::Complete;
            sink.on_transaction(t);
        }
        sink.flush();
        assert_eq!(*seen.lock().unwrap(), 4_000, "every event reaches the UI");
        assert_eq!(s.count().unwrap(), 2_000, "upserted, not duplicated");
        assert!(q(&s, "state:open").is_empty());
    }

    #[test]
    fn a_full_queue_drops_history_instead_of_blocking_the_proxy() {
        let s = Store::open_in_memory(StoreConfig::default()).unwrap();
        let sink = StoreSink::with_capacity(s.clone(), "run".into(), None, 1);
        let t0 = std::time::Instant::now();
        for i in 0..20_000 {
            sink.on_transaction(tx(i, "GET", "/x", 200, 1, "x"));
        }
        let took = t0.elapsed();
        sink.flush();
        assert!(
            took < Duration::from_millis(500),
            "sending never waits for the disk ({took:?})"
        );
        assert!(
            sink.dropped() > 0,
            "with a 1-slot queue some history must be dropped"
        );
        assert_eq!(
            s.count().unwrap() + sink.dropped(),
            20_000,
            "every event is either stored or counted as dropped"
        );
    }

    #[test]
    fn search_stays_fast_on_a_large_history() {
        let s = Store::open_in_memory(StoreConfig::default()).unwrap();
        let batch: Vec<_> = (0..50_000)
            .map(|i| {
                tx(
                    i,
                    "GET",
                    &format!("/v2/products/{i}"),
                    if i % 97 == 0 { 500 } else { 200 },
                    i % 2000,
                    "{}",
                )
            })
            .collect();
        for chunk in batch.chunks(5_000) {
            s.upsert("big", chunk).unwrap();
        }
        let t0 = std::time::Instant::now();
        let hits = s
            .search(&Filter::parse("status:5xx").unwrap(), 100)
            .unwrap();
        let t1 = t0.elapsed();
        let text = s
            .search(&Filter::parse("products/4242").unwrap(), 100)
            .unwrap();
        let t2 = t0.elapsed() - t1;
        assert_eq!(hits.len(), 100);
        assert!(text.iter().any(|r| r.tx.path == "/v2/products/4242"));
        eprintln!("50k rows: status:5xx (100 hits) {t1:?}, text search {t2:?}");
        assert!(
            t1 < Duration::from_secs(2) && t2 < Duration::from_millis(500),
            "status {t1:?}, text {t2:?}"
        );
    }
}
