//! Applying rules (relay-model::rules) to live traffic: block, map local, map remote, rewrite and
//! throttle. Breakpoints need an editor UI and are not applied here yet.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use bytes::Bytes;
use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use relay_model::Transaction;
use relay_model::filter::Filter;
use relay_model::rules::{
    Phase, RULES_VERSION, RewriteAction, Rule, RuleAction, RuleSet, ThrottleProfile,
};
use serde_json::Value;

use crate::body::decode_body;

/// Largest body Relay will hold in memory to rewrite it.
pub const MAX_REWRITE_BODY: usize = 32 * 1024 * 1024;

/// A rule set ready to apply: filters parsed once, and the folder `map_local` files are read from.
pub struct ActiveRules {
    set: RuleSet,
    base: PathBuf,
    filters: Vec<Option<Filter>>,
}

impl ActiveRules {
    pub fn new(set: RuleSet, base: impl Into<PathBuf>) -> Self {
        let filters = set
            .rules
            .iter()
            .map(|r| {
                r.matcher
                    .filter
                    .as_deref()
                    .and_then(|f| Filter::parse(f).ok())
            })
            .collect();
        Self {
            set,
            base: base.into(),
            filters,
        }
    }

    pub fn empty() -> Self {
        Self::new(
            RuleSet {
                version: RULES_VERSION,
                rules: vec![],
            },
            ".",
        )
    }

    /// Enabled rules that match this exchange, in file order.
    pub fn matching(&self, t: &Transaction) -> Vec<&Rule> {
        self.set
            .rules
            .iter()
            .enumerate()
            .filter(|(i, r)| {
                r.enabled
                    && r.matcher
                        .matches_request(&t.method, &t.scheme, &t.host, &t.path)
                    && match (&r.matcher.filter, &self.filters[*i]) {
                        (None, _) => true,
                        (Some(_), Some(f)) => f.matches(t),
                        (Some(_), None) => false,
                    }
            })
            .map(|(_, r)| r)
            .collect()
    }

    /// An answer that replaces the server's (block, map local, throttle drop), if a rule gives one.
    pub async fn early(&self, matched: &[&Rule]) -> Option<Early> {
        for r in matched {
            match &r.action {
                RuleAction::Block { status } => {
                    return Some(Early::Respond {
                        rule: r.id.clone(),
                        status: *status,
                        headers: vec![],
                        body: Bytes::from(format!("Blocked by Relay rule `{}`\n", r.id)),
                    });
                }
                RuleAction::MapLocal {
                    file,
                    status,
                    headers,
                } => {
                    let path = self.base.join(file);
                    return Some(match tokio::fs::read(&path).await {
                        Ok(bytes) => {
                            let mut h: Vec<(String, String)> = headers
                                .iter()
                                .map(|(k, v)| (k.clone(), v.clone()))
                                .collect();
                            if !h
                                .iter()
                                .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                            {
                                h.push(("content-type".into(), guess_type(file).into()));
                            }
                            Early::Respond {
                                rule: r.id.clone(),
                                status: *status,
                                headers: h,
                                body: Bytes::from(bytes),
                            }
                        }
                        Err(e) => Early::Drop {
                            rule: r.id.clone(),
                            reason: format!("map local file {}: {e}", path.display()),
                        },
                    });
                }
                RuleAction::Throttle { profile } => {
                    let p = Throttle::from(profile);
                    if p.offline || (p.loss_pct > 0.0 && rand::random::<f32>() * 100.0 < p.loss_pct)
                    {
                        return Some(Early::Drop {
                            rule: r.id.clone(),
                            reason: if p.offline {
                                "offline (throttle rule)".into()
                            } else {
                                "request dropped (throttle rule)".into()
                            },
                        });
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Rewrite actions for a phase (`Both` rules apply to each), with the rule ids that carry them.
    pub fn edits<'a>(
        &self,
        matched: &[&'a Rule],
        phase: Phase,
    ) -> Vec<(&'a str, &'a RewriteAction)> {
        let mut out = vec![];
        for r in matched {
            if let RuleAction::Rewrite { phase: p, actions } = &r.action
                && (*p == phase || *p == Phase::Both)
            {
                out.extend(actions.iter().map(|a| (r.id.as_str(), a)));
            }
        }
        out
    }

    /// The server a request is redirected to by a `map_remote` rule: (rule, scheme, authority, path prefix, keep host).
    pub fn remote(&self, matched: &[&Rule]) -> Option<Remote> {
        matched.iter().find_map(|r| match &r.action {
            RuleAction::MapRemote { to, preserve_host } => {
                let uri: http::Uri = to.parse().ok()?;
                Some(Remote {
                    rule: r.id.clone(),
                    scheme: uri.scheme_str()?.to_owned(),
                    authority: uri.authority()?.to_string(),
                    path_prefix: uri.path().trim_end_matches('/').to_owned(),
                    preserve_host: *preserve_host,
                })
            }
            _ => None,
        })
    }

    /// Network conditions to impose on the response of the first matching throttle rule.
    pub fn throttle(&self, matched: &[&Rule]) -> Option<(String, Throttle)> {
        matched.iter().find_map(|r| match &r.action {
            RuleAction::Throttle { profile } => Some((r.id.clone(), Throttle::from(profile))),
            _ => None,
        })
    }
}

pub enum Early {
    Respond {
        rule: String,
        status: u16,
        headers: Vec<(String, String)>,
        body: Bytes,
    },
    Drop {
        rule: String,
        reason: String,
    },
}

pub struct Remote {
    pub rule: String,
    pub scheme: String,
    pub authority: String,
    pub path_prefix: String,
    pub preserve_host: bool,
}

/// Simulated network conditions.
#[derive(Debug, Clone, PartialEq)]
pub struct Throttle {
    pub latency_ms: u64,
    pub down_kbps: Option<u32>,
    pub loss_pct: f32,
    pub offline: bool,
}

impl From<&ThrottleProfile> for Throttle {
    fn from(p: &ThrottleProfile) -> Self {
        let t = |latency_ms, down_kbps, loss_pct| Throttle {
            latency_ms,
            down_kbps,
            loss_pct,
            offline: false,
        };
        match p {
            ThrottleProfile::Preset(name) => match name.as_str() {
                "3g" => t(300, Some(780), 0.0),
                "edge" => t(400, Some(240), 0.0),
                "lte" => t(50, Some(12_000), 0.0),
                "lte-lossy" => t(70, Some(8_000), 3.0),
                "high-latency" => t(800, None, 0.0),
                _ => Throttle {
                    offline: true,
                    ..t(0, None, 0.0)
                },
            },
            ThrottleProfile::Custom {
                latency_ms,
                down_kbps,
                loss_pct,
                ..
            } => t(*latency_ms as u64, *down_kbps, *loss_pct),
        }
    }
}

impl Throttle {
    /// How long a response of `bytes` takes under these conditions (latency + transfer time).
    pub fn delay_for(&self, bytes: Option<u64>) -> Duration {
        let transfer = match (self.down_kbps, bytes) {
            (Some(kbps), Some(b)) if kbps > 0 => b * 8 / kbps as u64,
            _ => 0,
        };
        Duration::from_millis(self.latency_ms + transfer)
    }
}

/// Holder the proxy reads on every request; replaced atomically when rules change.
pub struct RulesHook(RwLock<Arc<ActiveRules>>);

impl Default for RulesHook {
    fn default() -> Self {
        Self(RwLock::new(Arc::new(ActiveRules::empty())))
    }
}

impl RulesHook {
    pub fn get(&self) -> Arc<ActiveRules> {
        self.0.read().unwrap().clone()
    }
    pub fn set(&self, rules: ActiveRules) {
        *self.0.write().unwrap() = Arc::new(rules);
    }
}

pub fn has_body_edit(edits: &[(&str, &RewriteAction)]) -> bool {
    edits.iter().any(|(_, a)| {
        matches!(
            a,
            RewriteAction::SetJson { .. }
                | RewriteAction::RemoveJson { .. }
                | RewriteAction::ReplaceBody { .. }
        )
    })
}

pub fn edit_delay(edits: &[(&str, &RewriteAction)]) -> Duration {
    Duration::from_millis(
        edits
            .iter()
            .filter_map(|(_, a)| match a {
                RewriteAction::DelayMs(ms) => Some(*ms),
                _ => None,
            })
            .sum(),
    )
}

/// Applies header edits (and `set_status`, when given a place to put it).
pub fn apply_headers(
    edits: &[(&str, &RewriteAction)],
    headers: &mut HeaderMap,
    status: Option<&mut http::StatusCode>,
) {
    let mut status = status;
    for (_, a) in edits {
        match a {
            RewriteAction::SetHeader { name, value } => {
                if let (Ok(n), Ok(v)) = (
                    HeaderName::from_bytes(name.as_bytes()),
                    HeaderValue::from_str(value),
                ) {
                    headers.insert(n, v);
                }
            }
            RewriteAction::RemoveHeader { name } => {
                if let Ok(n) = HeaderName::from_bytes(name.as_bytes()) {
                    headers.remove(n);
                }
            }
            RewriteAction::SetStatus(code) => {
                if let (Some(s), Ok(c)) = (status.as_deref_mut(), http::StatusCode::from_u16(*code))
                {
                    *s = c;
                }
            }
            _ => {}
        }
    }
}

/// Applies the body edits. Gzip/deflate bodies are decoded first (the result is sent uncompressed).
/// A body that cannot be edited (unknown encoding, not JSON for a JSON edit) is passed unchanged.
pub fn rewrite_body(
    edits: &[(&str, &RewriteAction)],
    headers: &mut HeaderMap,
    body: Bytes,
) -> Bytes {
    if !has_body_edit(edits) {
        return body;
    }
    let Some(plain) = decode_body(headers, &body, MAX_REWRITE_BODY) else {
        return body;
    };
    let mut text = plain;
    for (_, a) in edits {
        match a {
            RewriteAction::SetJson { path, value } => {
                if let Some(out) = edit_json(&text, |v| set_path(v, path, value.clone())) {
                    text = out;
                }
            }
            RewriteAction::RemoveJson { path } => {
                if let Some(out) = edit_json(&text, |v| remove_path(v, path)) {
                    text = out;
                }
            }
            RewriteAction::ReplaceBody {
                find,
                replace,
                regex,
            } => {
                let s = String::from_utf8_lossy(&text).into_owned();
                let replaced = if *regex {
                    match regex::Regex::new(find) {
                        Ok(re) => re.replace_all(&s, replace.as_str()).into_owned(),
                        Err(_) => s,
                    }
                } else {
                    s.replace(find.as_str(), replace)
                };
                text = replaced.into_bytes();
            }
            _ => {}
        }
    }
    headers.remove(header::CONTENT_ENCODING);
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(text.len()));
    Bytes::from(text)
}

fn edit_json(text: &[u8], f: impl FnOnce(&mut Value)) -> Option<Vec<u8>> {
    let mut v: Value = serde_json::from_slice(text).ok()?;
    f(&mut v);
    serde_json::to_vec(&v).ok()
}

enum Seg {
    Key(String),
    Index(usize),
}

/// Parses `$.a.b[0].c` (a leading `$` and `.` are optional).
fn parse_path(path: &str) -> Vec<Seg> {
    let p = path.trim().trim_start_matches('$');
    let mut out = vec![];
    let mut key = String::new();
    let mut chars = p.chars().peekable();
    let flush = |key: &mut String, out: &mut Vec<Seg>| {
        if !key.is_empty() {
            out.push(Seg::Key(std::mem::take(key)));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '.' => flush(&mut key, &mut out),
            '[' => {
                flush(&mut key, &mut out);
                let mut inner = String::new();
                for c in chars.by_ref() {
                    if c == ']' {
                        break;
                    }
                    inner.push(c);
                }
                match inner.parse::<usize>() {
                    Ok(i) => out.push(Seg::Index(i)),
                    Err(_) => out.push(Seg::Key(inner.trim_matches(['"', '\'']).to_owned())),
                }
            }
            c => key.push(c),
        }
    }
    flush(&mut key, &mut out);
    out
}

fn set_path(root: &mut Value, path: &str, value: Value) {
    let segs = parse_path(path);
    if segs.is_empty() {
        *root = value;
        return;
    }
    let mut cur = root;
    for (i, seg) in segs.iter().enumerate() {
        let last = i + 1 == segs.len();
        match seg {
            Seg::Key(k) => {
                if !cur.is_object() {
                    *cur = Value::Object(Default::default());
                }
                let map = cur.as_object_mut().unwrap();
                if last {
                    map.insert(k.clone(), value);
                    return;
                }
                cur = map.entry(k.clone()).or_insert(Value::Null);
            }
            Seg::Index(n) => {
                if !cur.is_array() {
                    *cur = Value::Array(vec![]);
                }
                let arr = cur.as_array_mut().unwrap();
                while arr.len() <= *n {
                    arr.push(Value::Null);
                }
                if last {
                    arr[*n] = value;
                    return;
                }
                cur = &mut arr[*n];
            }
        }
    }
}

fn remove_path(root: &mut Value, path: &str) {
    let segs = parse_path(path);
    let Some((last, parents)) = segs.split_last() else {
        return;
    };
    let mut cur = root;
    for seg in parents {
        let next = match seg {
            Seg::Key(k) => cur.get_mut(k.as_str()),
            Seg::Index(n) => cur.get_mut(*n),
        };
        match next {
            Some(n) => cur = n,
            None => return,
        }
    }
    match last {
        Seg::Key(k) => {
            if let Some(m) = cur.as_object_mut() {
                m.remove(k);
            }
        }
        Seg::Index(n) => {
            if let Some(a) = cur.as_array_mut()
                && *n < a.len()
            {
                a.remove(*n);
            }
        }
    }
}

fn guess_type(file: &str) -> &'static str {
    match file
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "json" => "application/json",
        "html" | "htm" => "text/html; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "js" => "text/javascript",
        "css" => "text/css",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edits(a: &[RewriteAction]) -> Vec<(&'static str, &RewriteAction)> {
        a.iter().map(|x| ("r", x)).collect()
    }

    #[test]
    fn json_paths_set_and_remove() {
        let a = [
            RewriteAction::SetJson {
                path: "$.flags.new_checkout".into(),
                value: Value::Bool(true),
            },
            RewriteAction::SetJson {
                path: "$.items[1].qty".into(),
                value: 3.into(),
            },
            RewriteAction::RemoveJson {
                path: "$.secret".into(),
            },
        ];
        let mut h = HeaderMap::new();
        let out = rewrite_body(
            &edits(&a),
            &mut h,
            Bytes::from_static(br#"{"secret":1,"items":[{"qty":1},{"qty":2}]}"#),
        );
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["flags"]["new_checkout"], true);
        assert_eq!(v["items"][1]["qty"], 3);
        assert!(v.get("secret").is_none());
        assert_eq!(h.get("content-length").unwrap(), &out.len().to_string());
    }

    #[test]
    fn replace_body_literal_and_regex() {
        let a = [
            RewriteAction::ReplaceBody {
                find: "cat".into(),
                replace: "dog".into(),
                regex: false,
            },
            RewriteAction::ReplaceBody {
                find: r"d\w+".into(),
                replace: "bird".into(),
                regex: true,
            },
        ];
        let mut h = HeaderMap::new();
        let out = rewrite_body(&edits(&a), &mut h, Bytes::from_static(b"a cat sat"));
        assert_eq!(&out[..], b"a bird sat");
    }

    #[test]
    fn non_json_bodies_are_left_alone_by_json_edits() {
        let a = [RewriteAction::SetJson {
            path: "$.a".into(),
            value: 1.into(),
        }];
        let mut h = HeaderMap::new();
        let out = rewrite_body(&edits(&a), &mut h, Bytes::from_static(b"<html>"));
        assert_eq!(&out[..], b"<html>");
    }

    #[test]
    fn presets_map_to_conditions() {
        let t = Throttle::from(&ThrottleProfile::Preset("3g".into()));
        assert_eq!(t.latency_ms, 300);
        assert!(Throttle::from(&ThrottleProfile::Preset("offline".into())).offline);
        // 78 KB at 780 kbps takes 800 ms on top of the latency.
        assert_eq!(t.delay_for(Some(78_000)), Duration::from_millis(300 + 800));
    }
}
