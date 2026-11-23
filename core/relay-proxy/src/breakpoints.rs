//! Breakpoints: a request or response that matches a `breakpoint` rule is held until someone
//! (the Mac app, the CLI, an agent) resumes it, optionally with edits, or aborts it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use relay_model::Header;
use tokio::sync::oneshot;

/// A held request is released unchanged after this long, so a forgotten breakpoint cannot hang an app forever.
pub const PAUSE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BreakPhase {
    Request,
    Response,
}

/// What is being held, as shown in the editor.
#[derive(Debug, Clone)]
pub struct Paused {
    pub tx_id: u64,
    pub phase: BreakPhase,
    pub rule: String,
    pub method: String,
    pub url: String,
    /// Response status (response phase only).
    pub status: Option<u16>,
    pub headers: Vec<Header>,
    pub body: Vec<u8>,
}

/// The edited message. Headers and body replace the held ones.
#[derive(Debug, Clone)]
pub struct Edit {
    pub status: Option<u16>,
    pub headers: Vec<Header>,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub enum Decision {
    Resume(Option<Edit>),
    Abort,
}

type Held = HashMap<(u64, BreakPhase), (Paused, oneshot::Sender<Decision>)>;

#[derive(Default)]
pub struct Breakpoints {
    held: Mutex<Held>,
}

impl Breakpoints {
    /// Holds until a decision arrives (or the timeout passes, which resumes unchanged).
    pub async fn pause(&self, item: Paused) -> Decision {
        let (tx, rx) = oneshot::channel();
        let key = (item.tx_id, item.phase);
        self.held.lock().unwrap().insert(key, (item, tx));
        let decision = match tokio::time::timeout(PAUSE_TIMEOUT, rx).await {
            Ok(Ok(d)) => d,
            _ => Decision::Resume(None),
        };
        self.held.lock().unwrap().remove(&key);
        decision
    }

    pub fn list(&self) -> Vec<Paused> {
        let mut v: Vec<Paused> = self
            .held
            .lock()
            .unwrap()
            .values()
            .map(|(p, _)| p.clone())
            .collect();
        v.sort_by_key(|p| (p.tx_id, p.phase == BreakPhase::Response));
        v
    }

    /// Releases a held message. Returns false if it was not (or no longer) held.
    pub fn decide(&self, tx_id: u64, phase: BreakPhase, decision: Decision) -> bool {
        match self.held.lock().unwrap().remove(&(tx_id, phase)) {
            Some((_, tx)) => tx.send(decision).is_ok(),
            None => false,
        }
    }
}
