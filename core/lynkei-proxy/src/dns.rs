//! DNS spoofing: `dns_spoof` rules make Lynkei connect to another address for a host, while the
//! app (and TLS) still see the original name. Used for upstream connections only, so nothing on
//! the app's side changes.

use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};

use hyper_util::client::legacy::connect::dns::{GaiResolver, Name};
use lynkei_model::rules::{Matcher, RuleAction, RuleSet};
use tower_service::Service;

/// The enabled `dns_spoof` rules, shared by every upstream connection and replaced on a rules change.
#[derive(Clone, Default)]
pub struct Spoofs(Arc<RwLock<Vec<(String, Matcher, String)>>>);

impl Spoofs {
    /// Replaces the rules; returns whether anything changed.
    pub fn set(&self, rules: &RuleSet) -> bool {
        let list: Vec<_> = rules
            .rules
            .iter()
            .filter(|r| r.enabled)
            .filter_map(|r| match &r.action {
                RuleAction::DnsSpoof { to } => {
                    Some((r.id.clone(), r.matcher.clone(), to.trim().to_owned()))
                }
                _ => None,
            })
            .collect();
        let mut current = self.0.write().unwrap();
        let changed = *current != list;
        *current = list;
        changed
    }

    /// `(rule id, replacement)` for the first rule that matches `host`.
    pub fn lookup(&self, host: &str) -> Option<(String, String)> {
        self.0
            .read()
            .unwrap()
            .iter()
            .find(|(_, m, _)| m.matches_host(host))
            .map(|(id, _, to)| (id.clone(), to.clone()))
    }
}

/// System DNS, except for hosts a rule spoofs.
#[derive(Clone)]
pub struct SpoofResolver {
    spoofs: Spoofs,
    system: GaiResolver,
}

impl SpoofResolver {
    pub fn new(spoofs: Spoofs) -> Self {
        Self {
            spoofs,
            system: GaiResolver::new(),
        }
    }
}

type Addrs = std::vec::IntoIter<SocketAddr>;

impl Service<Name> for SpoofResolver {
    type Response = Addrs;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Addrs, io::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.system.poll_ready(cx)
    }

    fn call(&mut self, name: Name) -> Self::Future {
        let target = self.spoofs.lookup(name.as_str()).map(|(_, to)| to);
        let mut system = self.system.clone();
        Box::pin(async move {
            let name = match target {
                // An IP needs no lookup (the port is filled in by the connector).
                Some(to) => match IpAddr::from_str(&to) {
                    Ok(ip) => return Ok(vec![SocketAddr::new(ip, 0)].into_iter()),
                    Err(_) => Name::from_str(&to)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?,
                },
                None => name,
            };
            let addrs = system.call(name).await?;
            Ok(addrs.collect::<Vec<_>>().into_iter())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(yaml: &str) -> RuleSet {
        RuleSet::from_yaml(yaml).unwrap()
    }

    #[test]
    fn matches_hosts_and_subdomains_and_respects_enabled() {
        let s = Spoofs::default();
        s.set(&rules(
            "version: 1\nrules:\n  - { id: api, kind: dns_spoof, match: { host: api.acme.com }, to: 127.0.0.1 }\n  - { id: off, enabled: false, kind: dns_spoof, match: { host: other.com }, to: 10.0.0.1 }\n",
        ));
        assert_eq!(s.lookup("api.acme.com").unwrap().1, "127.0.0.1");
        assert_eq!(s.lookup("v2.api.acme.com").unwrap().0, "api");
        assert!(s.lookup("acme.com").is_none());
        assert!(s.lookup("other.com").is_none());
    }

    #[tokio::test]
    async fn ip_targets_skip_the_system_resolver() {
        let s = Spoofs::default();
        s.set(&rules(
            "version: 1\nrules:\n  - { id: a, kind: dns_spoof, match: { host: shop.test }, to: 192.0.2.7 }\n",
        ));
        let mut r = SpoofResolver::new(s);
        let got: Vec<_> = r
            .call(Name::from_str("shop.test").unwrap())
            .await
            .unwrap()
            .collect();
        assert_eq!(got, vec!["192.0.2.7:0".parse::<SocketAddr>().unwrap()]);
        let local: Vec<_> = r
            .call(Name::from_str("localhost").unwrap())
            .await
            .unwrap()
            .collect();
        assert!(local.iter().any(|a| a.ip().is_loopback()));
    }
}
