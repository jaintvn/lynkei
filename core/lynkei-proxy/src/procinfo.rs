//! Which program on this Mac opened a connection to Lynkei ("client process", like Charles).
//!
//! Connections from this Mac — browsers, desktop apps, iOS Simulator apps — arrive from
//! `127.0.0.1:<ephemeral port>`. `lsof` lists the sockets on Lynkei's port together with their owning
//! programs, so the client port maps to a name. One listing resolves every open connection, and
//! lookups are batched, so a busy browser costs one `lsof` per burst rather than one per connection.

use std::collections::HashMap;

/// Parses `lsof -nP +c0 -iTCP:<port> -sTCP:ESTABLISHED -Fcn` output into client port → program.
/// Only the client side of each connection counts (`127.0.0.1:<client>-><lynkei port>`).
pub fn parse_lsof(output: &str, lynkei_port: u16) -> HashMap<u16, String> {
    let mut out = HashMap::new();
    let mut command: Option<String> = None;
    for line in output.lines() {
        let Some(rest) = line.get(1..) else { continue };
        match line.as_bytes().first() {
            Some(b'p') => command = None, // a new process begins
            Some(b'c') => command = Some(tidy_name(rest)),
            Some(b'n') => {
                // "127.0.0.1:53946->127.0.0.1:8888" (client side) or the reverse (Lynkei's side).
                let Some((local, remote)) = rest.split_once("->") else {
                    continue;
                };
                let remote_port = remote
                    .rsplit(':')
                    .next()
                    .and_then(|p| p.parse::<u16>().ok());
                let local_port = local.rsplit(':').next().and_then(|p| p.parse::<u16>().ok());
                if let (Some(cmd), Some(lp), Some(rp)) = (&command, local_port, remote_port)
                    && rp == lynkei_port
                    && lp != lynkei_port
                {
                    out.insert(lp, cmd.clone());
                }
            }
            _ => {}
        }
    }
    out
}

/// "Google Chrome Helper (Renderer)" → "Google Chrome"; "com.apple.WebKit.Networking" stays.
fn tidy_name(raw: &str) -> String {
    let name = raw.split(" Helper").next().unwrap_or(raw).trim();
    name.replace("\\x20", " ")
}

/// Runs `lsof` once and returns every client port → program currently connected to `lynkei_port`.
#[cfg(target_os = "macos")]
pub fn snapshot(lynkei_port: u16) -> HashMap<u16, String> {
    let out = std::process::Command::new("/usr/sbin/lsof")
        .args([
            "-nP",
            "+c0",
            &format!("-iTCP:{lynkei_port}"),
            "-sTCP:ESTABLISHED",
            "-Fcn",
        ])
        .stdin(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) => parse_lsof(&String::from_utf8_lossy(&o.stdout), lynkei_port),
        Err(_) => HashMap::new(),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn snapshot(_lynkei_port: u16) -> HashMap<u16, String> {
    HashMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "p501\ncGoogle Chrome Helper\nn127.0.0.1:53946->127.0.0.1:8888\nn127.0.0.1:53947->127.0.0.1:8888\n\
                          p502\ncAcmeShop\nn127.0.0.1:60001->127.0.0.1:8888\n\
                          p9\ncLynkei\nn127.0.0.1:8888->127.0.0.1:53946\nn127.0.0.1:8888->127.0.0.1:60001\nn*:8888\n\
                          p77\ncSimulator Helper\nn[::1]:61000->[::1]:8888\n";

    #[test]
    fn maps_client_ports_to_programs_and_ignores_relays_own_side() {
        let m = parse_lsof(SAMPLE, 8888);
        assert_eq!(
            m.get(&53946).map(String::as_str),
            Some("Google Chrome"),
            "Helper suffix is tidied"
        );
        assert_eq!(m.get(&53947).map(String::as_str), Some("Google Chrome"));
        assert_eq!(m.get(&60001).map(String::as_str), Some("AcmeShop"));
        assert_eq!(
            m.get(&61000).map(String::as_str),
            Some("Simulator"),
            "IPv6 loopback works too"
        );
        assert!(
            !m.values().any(|v| v == "Lynkei"),
            "Lynkei's own sockets are not clients"
        );
        assert_eq!(m.len(), 4);
    }

    #[test]
    fn other_lynkei_ports_are_not_mixed_in() {
        assert!(parse_lsof(SAMPLE, 9999).is_empty());
        assert!(parse_lsof("", 8888).is_empty());
    }
}
