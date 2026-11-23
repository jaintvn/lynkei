//! Relay rules: a versioned YAML format (commit it next to your app) for rewrite, map local,
//! map remote, breakpoints, blocking and throttling. The engine applies them from P3; this module
//! owns the format, validation and matching so every platform reads rules identically.
//!
//! ```yaml
//! version: 1
//! rules:
//!   - id: new-checkout
//!     name: Force feature flag new_checkout = true
//!     kind: rewrite
//!     match: { method: GET, host: api.acmeshop.com, path: /v2/feature-flags }
//!     phase: response
//!     actions:
//!       - set_json: { path: $.flags.new_checkout, value: true }
//!       - set_header: { name: cache-control, value: no-store }
//! ```

use std::collections::{BTreeMap, HashSet};
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::filter::{Filter, Glob};

pub const RULES_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleSet {
    /// Format version (currently 1).
    pub version: u32,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Rule {
    /// Stable identifier, unique in the file (letters, digits, `-`, `_`).
    pub id: String,
    /// Human-readable name shown in the UI.
    #[serde(default)]
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Which requests the rule applies to.
    #[serde(rename = "match")]
    pub matcher: Matcher,
    #[serde(flatten)]
    pub action: RuleAction,
}

fn yes() -> bool {
    true
}

/// Request matching. Every field that is set must match; `host` and `path` accept `*`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Matcher {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
    /// e.g. `api.acmeshop.com`, `*.acmeshop.com` (a bare domain also matches its subdomains).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// e.g. `/v2/checkout`, `/v2/products/*` (without `*`, a prefix match).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Any extra condition in the search language, e.g. `status:5xx header:x-env=staging`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Request,
    #[default]
    Response,
    Both,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleAction {
    /// Edit requests or responses in flight.
    Rewrite {
        #[serde(default)]
        phase: Phase,
        actions: Vec<RewriteAction>,
    },
    /// Answer from a local file instead of the server.
    MapLocal {
        /// Path to the body file, relative to the rules file.
        file: String,
        #[serde(default = "ok_status")]
        status: u16,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
    /// Send the request to another server (e.g. production → staging).
    MapRemote {
        /// Base URL, e.g. `https://staging.acmeshop.com`; the original path and query are kept.
        to: String,
        /// Keep the original `Host`/`:authority` (for servers that route by host).
        #[serde(default)]
        preserve_host: bool,
    },
    /// Pause matching requests/responses for editing.
    Breakpoint {
        #[serde(default)]
        phase: Phase,
    },
    /// Refuse matching requests without contacting the server.
    Block {
        #[serde(default = "forbidden")]
        status: u16,
    },
    /// Simulate a slow or lossy network.
    Throttle { profile: ThrottleProfile },
    /// Connect to a different address for a host (like an `/etc/hosts` entry, but only for traffic
    /// through Relay). The app still sees the original host name: `Host`, TLS name and certificate
    /// checks are unchanged. Only `match.host` is used.
    DnsSpoof {
        /// An IP address (`127.0.0.1`, `10.0.0.7`) or another host name to look up instead.
        to: String,
    },
}

fn ok_status() -> u16 {
    200
}
fn forbidden() -> u16 {
    403
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RewriteAction {
    /// Set a value at a JSON path in the body (`$.a.b[0].c`).
    SetJson {
        path: String,
        value: serde_json::Value,
    },
    /// Remove a key at a JSON path.
    RemoveJson {
        path: String,
    },
    SetHeader {
        name: String,
        value: String,
    },
    RemoveHeader {
        name: String,
    },
    SetStatus(u16),
    /// Replace text in the body (literal unless `regex: true`).
    ReplaceBody {
        find: String,
        replace: String,
        #[serde(default)]
        regex: bool,
    },
    /// Delay the response by this many milliseconds.
    DelayMs(u64),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ThrottleProfile {
    /// `3g`, `edge`, `lte`, `lte-lossy`, `offline`, `high-latency`.
    Preset(String),
    Custom {
        #[serde(default)]
        latency_ms: u32,
        #[serde(default)]
        down_kbps: Option<u32>,
        #[serde(default)]
        up_kbps: Option<u32>,
        /// Percentage of packets/requests to drop (0–100).
        #[serde(default)]
        loss_pct: f32,
    },
}

pub const THROTTLE_PRESETS: &[&str] =
    &["3g", "edge", "lte", "lte-lossy", "offline", "high-latency"];

/// A problem found by [`RuleSet::validate`], pointing at the rule it concerns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub rule: Option<String>,
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.rule {
            Some(id) => write!(f, "rule `{id}`: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl RuleSet {
    pub fn from_yaml(text: &str) -> Result<RuleSet, String> {
        let set: RuleSet = serde_norway::from_str(text).map_err(|e| e.to_string())?;
        if set.version > RULES_VERSION {
            return Err(format!(
                "rules file is version {}, this Relay reads up to {RULES_VERSION}; please update Relay",
                set.version
            ));
        }
        Ok(set)
    }

    pub fn to_yaml(&self) -> String {
        // Through a JSON value so enum variants come out as plain maps (`- set_json: {…}`) rather
        // than YAML tags (`!set_json`), which the flattened `kind` layout cannot read back.
        let value = serde_json::to_value(self).expect("rules always serialize");
        serde_norway::to_string(&value).expect("rules always serialize")
    }

    /// Every problem in the file, so the UI can show them all at once. Empty means valid.
    pub fn validate(&self) -> Vec<Problem> {
        let mut out = Vec::new();
        let mut ids = HashSet::new();
        for r in &self.rules {
            let mut p = |m: String| {
                out.push(Problem {
                    rule: Some(r.id.clone()),
                    message: m,
                })
            };
            if r.id.is_empty()
                || !r
                    .id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                p("id must be letters, digits, `-` or `_`".into());
            }
            if !ids.insert(r.id.clone()) {
                p("duplicate id".into());
            }
            let m = &r.matcher;
            if m.method.is_none()
                && m.host.is_none()
                && m.path.is_none()
                && m.filter.is_none()
                && m.scheme.is_none()
            {
                p("`match` is empty and would apply to every request; add a host, path, method or filter".into());
            }
            if let Some(f) = &m.filter
                && let Err(e) = Filter::parse(f)
            {
                p(format!("match.filter: {e}"));
            }
            if let Some(method) = &m.method
                && !method.chars().all(|c| c.is_ascii_alphabetic())
            {
                p(format!("match.method `{method}` is not an HTTP method"));
            }
            let status_ok = |s: u16| (100..=599).contains(&s);
            match &r.action {
                RuleAction::Rewrite { actions, .. } => {
                    if actions.is_empty() {
                        p("rewrite has no actions".into());
                    }
                    for a in actions {
                        match a {
                            RewriteAction::SetJson { path, .. }
                            | RewriteAction::RemoveJson { path }
                                if !path.starts_with('$') =>
                            {
                                p(format!(
                                    "JSON path `{path}` must start with `$` (e.g. `$.flags.new_checkout`)"
                                ));
                            }
                            RewriteAction::SetHeader { name, .. }
                            | RewriteAction::RemoveHeader { name }
                                if name.is_empty()
                                    || name.contains(|c: char| c.is_whitespace() || c == ':') =>
                            {
                                p(format!("header name `{name}` is invalid"));
                            }
                            RewriteAction::SetStatus(s) if !status_ok(*s) => {
                                p(format!("status {s} is not between 100 and 599"))
                            }
                            RewriteAction::ReplaceBody { find, .. } if find.is_empty() => {
                                p("replace_body.find is empty".into())
                            }
                            _ => {}
                        }
                    }
                }
                RuleAction::MapLocal { file, status, .. } => {
                    if file.is_empty() {
                        p("map_local.file is empty".into());
                    }
                    if !status_ok(*status) {
                        p(format!("status {status} is not between 100 and 599"));
                    }
                }
                RuleAction::MapRemote { to, .. } => {
                    let ok = (to.starts_with("http://") || to.starts_with("https://"))
                        && to.split("://").nth(1).is_some_and(|h| !h.is_empty());
                    if !ok {
                        p(format!(
                            "map_remote.to `{to}` must be an http:// or https:// URL"
                        ));
                    }
                }
                RuleAction::DnsSpoof { to } => {
                    if m.host.as_deref().is_none_or(|h| h.trim().is_empty()) {
                        p("dns_spoof needs `match.host`".into());
                    }
                    if m.method.is_some()
                        || m.scheme.is_some()
                        || m.path.is_some()
                        || m.filter.is_some()
                    {
                        p("dns_spoof only uses `match.host` (DNS happens before any request is read)".into());
                    }
                    let target = to.trim();
                    if target.is_empty()
                        || target.contains(|c: char| c.is_whitespace() || c == '/' || c == '*')
                        || target.contains("://")
                    {
                        p(format!(
                            "dns_spoof.to `{to}` must be an IP address or a host name, like 127.0.0.1 or staging.internal"
                        ));
                    }
                }
                RuleAction::Block { status } if !status_ok(*status) => {
                    p(format!("status {status} is not between 100 and 599"))
                }
                RuleAction::Throttle { profile } => match profile {
                    ThrottleProfile::Preset(name) if !THROTTLE_PRESETS.contains(&name.as_str()) => {
                        p(format!(
                            "unknown throttle preset `{name}` (use one of {})",
                            THROTTLE_PRESETS.join(", ")
                        ));
                    }
                    ThrottleProfile::Custom { loss_pct, .. }
                        if !(0.0..=100.0).contains(loss_pct) =>
                    {
                        p("loss_pct must be between 0 and 100".into());
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        if self.version == 0 {
            out.push(Problem {
                rule: None,
                message: "version must be 1".into(),
            });
        }
        out
    }

    /// Enabled rules matching a request, in file order.
    pub fn matching<'a>(
        &'a self,
        method: &'a str,
        scheme: &'a str,
        host: &'a str,
        path: &'a str,
    ) -> impl Iterator<Item = &'a Rule> {
        self.rules
            .iter()
            .filter(move |r| r.enabled && r.matcher.matches_request(method, scheme, host, path))
    }

    /// The JSON Schema for this format (checked into `shared/rules-schema/`).
    pub fn json_schema() -> String {
        serde_json::to_string_pretty(&schemars::schema_for!(RuleSet)).expect("schema serializes")
    }
}

impl Matcher {
    /// Whether `host` satisfies the `host` pattern (a rule without one matches every host).
    pub fn matches_host(&self, host: &str) -> bool {
        let Some(h) = &self.host else { return true };
        let sub = !h.contains('*')
            && host
                .to_lowercase()
                .ends_with(&format!(".{}", h.to_lowercase()));
        Glob::new(h).matches(host) || sub
    }

    /// Request-level match (method, scheme, host, path). `filter` needs a response and is
    /// evaluated by the engine with the full transaction.
    pub fn matches_request(&self, method: &str, scheme: &str, host: &str, path: &str) -> bool {
        if self
            .method
            .as_deref()
            .is_some_and(|m| !m.eq_ignore_ascii_case(method))
        {
            return false;
        }
        if self
            .scheme
            .as_deref()
            .is_some_and(|s| !s.eq_ignore_ascii_case(scheme))
        {
            return false;
        }
        if !self.matches_host(host) {
            return false;
        }
        if let Some(p) = &self.path {
            let g = Glob::new(p);
            let prefix = !p.contains('*') && path.to_lowercase().starts_with(&p.to_lowercase());
            if !(g.matches(path) || prefix) {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
version: 1
rules:
  - id: new-checkout
    name: Force feature flag new_checkout = true
    kind: rewrite
    match: { method: GET, host: api.acmeshop.com, path: /v2/feature-flags }
    phase: response
    actions:
      - set_json: { path: $.flags.new_checkout, value: true }
      - set_header: { name: cache-control, value: no-store }
      - set_status: 200
  - id: empty-recs
    name: Empty recommendations
    kind: map_local
    match: { host: acmeshop.com, path: /v2/recommendations }
    file: mocks/recommendations_empty.json
    headers: { content-type: application/json }
  - id: staging
    kind: map_remote
    match: { host: api.acmeshop.com }
    to: https://staging.acmeshop.com
    enabled: false
  - id: pause-recs
    kind: breakpoint
    match: { path: /v2/recommendations, filter: "method:GET" }
    phase: request
  - id: no-ads
    kind: block
    match: { host: "*.doubleclick.net" }
  - id: slow-pixel
    kind: throttle
    match: { host: api.acmeshop.com }
    profile: lte-lossy
  - id: custom-slow
    kind: throttle
    match: { path: /v2/search }
    profile: { latency_ms: 800, down_kbps: 256, loss_pct: 2.5 }
"#;

    #[test]
    fn parses_every_kind_and_round_trips() {
        let set = RuleSet::from_yaml(EXAMPLE).unwrap();
        assert_eq!(set.rules.len(), 7);
        assert!(set.validate().is_empty(), "{:?}", set.validate());
        assert!(
            matches!(&set.rules[0].action, RuleAction::Rewrite { phase: Phase::Response, actions } if actions.len() == 3)
        );
        assert!(
            matches!(
                &set.rules[1].action,
                RuleAction::MapLocal { status: 200, .. }
            ),
            "default status"
        );
        assert!(!set.rules[2].enabled);
        assert!(
            matches!(&set.rules[4].action, RuleAction::Block { status: 403 }),
            "default block status"
        );
        assert!(matches!(
            &set.rules[6].action,
            RuleAction::Throttle {
                profile: ThrottleProfile::Custom {
                    latency_ms: 800,
                    ..
                }
            }
        ));
        let again = RuleSet::from_yaml(&set.to_yaml()).unwrap();
        assert_eq!(again, set, "YAML round trip is lossless");
    }

    #[test]
    fn matching_uses_hosts_paths_and_enabled() {
        let set = RuleSet::from_yaml(EXAMPLE).unwrap();
        let ids = |m, h, p| {
            set.matching(m, "https", h, p)
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids("GET", "api.acmeshop.com", "/v2/feature-flags?x=1"),
            vec!["new-checkout", "slow-pixel"]
        );
        assert_eq!(
            ids("GET", "api.acmeshop.com", "/v2/recommendations"),
            vec!["empty-recs", "pause-recs", "slow-pixel"]
        );
        assert_eq!(
            ids("GET", "ads.g.doubleclick.net", "/pixel"),
            vec!["no-ads"]
        );
        assert!(
            !ids("GET", "api.acmeshop.com", "/v2/x").contains(&"staging"),
            "disabled rules never match"
        );
    }

    #[test]
    fn validation_reports_every_problem_with_its_rule() {
        let bad = r#"
version: 1
rules:
  - id: dup
    kind: block
    match: { host: a.com }
  - id: dup
    kind: rewrite
    match: {}
    actions:
      - set_json: { path: flags.x, value: 1 }
      - set_status: 999
  - id: "bad id!"
    kind: map_remote
    match: { host: a.com, filter: "stauts:500" }
    to: staging.acmeshop.com
  - id: t
    kind: throttle
    match: { host: a.com }
    profile: warp-speed
"#;
        let probs: Vec<String> = RuleSet::from_yaml(bad)
            .unwrap()
            .validate()
            .iter()
            .map(|p| p.to_string())
            .collect();
        let has = |s: &str| probs.iter().any(|p| p.contains(s));
        assert!(has("rule `dup`: duplicate id"));
        assert!(has("`match` is empty"));
        assert!(has("must start with `$`"));
        assert!(has("status 999"));
        assert!(has("rule `bad id!`: id must be"));
        assert!(has("match.filter: unknown filter"));
        assert!(has("must be an http:// or https:// URL"));
        assert!(has("unknown throttle preset `warp-speed`"));
    }

    #[test]
    fn unknown_fields_and_newer_versions_are_rejected_clearly() {
        assert!(
            RuleSet::from_yaml("version: 1\nrulez: []")
                .unwrap_err()
                .contains("rulez")
        );
        assert!(
            RuleSet::from_yaml(
                "version: 1\nrules:\n  - id: x\n    kind: teleport\n    match: {host: a}"
            )
            .is_err()
        );
        assert!(
            RuleSet::from_yaml("version: 9\nrules: []")
                .unwrap_err()
                .contains("update Relay")
        );
    }

    #[test]
    fn shipped_example_rules_file_is_valid() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../shared/rules-schema/example.rules.yaml"
        );
        let set = RuleSet::from_yaml(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert!(set.validate().is_empty(), "{:?}", set.validate());
        assert_eq!(set.rules.len(), 6);
    }

    #[test]
    fn checked_in_json_schema_is_current() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../shared/rules-schema/rules.schema.json"
        );
        let generated = RuleSet::json_schema();
        if std::env::var("UPDATE_SCHEMA").is_ok() {
            std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
            std::fs::write(path, &generated).unwrap();
        }
        let on_disk = std::fs::read_to_string(path).unwrap_or_default();
        assert_eq!(
            on_disk, generated,
            "run `UPDATE_SCHEMA=1 cargo test -p relay-model` to refresh shared/rules-schema"
        );
    }
}
