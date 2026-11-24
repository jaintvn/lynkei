//! The setup pages a phone opens to get connected: `http://<mac>.local:8888/setup`.
//!
//! - `/setup`            a short guide (the QR code on the Mac points here)
//! - `/android`          the same for an Android phone (the Android QR code points here)
//! - `/lynkei.mobileconfig`  iOS profile that installs the Lynkei CA
//! - `/proxy.pac`        proxy auto-config, so the phone survives a changed Mac IP
//! - `/ca.pem`, `/ca.crt`  the CA as PEM / DER (Android, macOS, Linux)

use base64::Engine as _;

pub struct Page {
    pub content_type: &'static str,
    pub filename: Option<&'static str>,
    pub body: Vec<u8>,
}

/// `host` is the Host header the phone used to reach us (e.g. `jains-mbp.local:8888`).
pub fn route(path: &str, host: &str, ca_pem: &str, ca_der: &[u8]) -> Option<Page> {
    let path = path.split('?').next().unwrap_or("/");
    Some(match path {
        "/" | "/setup" => Page {
            content_type: "text/html; charset=utf-8",
            filename: None,
            body: guide_html(host).into_bytes(),
        },
        "/android" => Page {
            content_type: "text/html; charset=utf-8",
            filename: None,
            body: android_html(host).into_bytes(),
        },
        "/ca.pem" | "/lynkei-ca.pem" => Page {
            content_type: "application/x-x509-ca-cert",
            filename: Some("lynkei-ca.pem"),
            body: ca_pem.as_bytes().to_vec(),
        },
        "/ca.crt" | "/ca.cer" => Page {
            content_type: "application/x-x509-ca-cert",
            filename: Some("lynkei-ca.crt"),
            body: ca_der.to_vec(),
        },
        "/lynkei.mobileconfig" => Page {
            content_type: "application/x-apple-aspen-config",
            filename: Some("lynkei.mobileconfig"),
            body: mobileconfig(ca_der).into_bytes(),
        },
        "/proxy.pac" => Page {
            content_type: "application/x-ns-proxy-autoconfig",
            filename: None,
            body: pac(host).into_bytes(),
        },
        _ => return None,
    })
}

pub fn pac(host: &str) -> String {
    // Fall back to DIRECT if Lynkei is not running, so the phone keeps its internet.
    format!("function FindProxyForURL(url, host) {{\n  return \"PROXY {host}; DIRECT\";\n}}\n")
}

/// A stable id derived from the certificate, so reinstalling replaces rather than duplicates.
fn uuid_from(der: &[u8], salt: u8) -> String {
    let fnv = |seed: u64| {
        let mut h = seed;
        for b in der.iter().chain(&[salt]) {
            h = (h ^ *b as u64).wrapping_mul(0x100000001b3);
        }
        h
    };
    let (a, b) = (fnv(0xcbf29ce484222325), fnv(0x84222325cbf29ce4));
    format!(
        "{:08X}-{:04X}-{:04X}-{:04X}-{:012X}",
        (a >> 32) as u32,
        (a >> 16) as u16,
        0x4000 | (a as u16 & 0x0fff),
        0x8000 | (b >> 48) as u16 & 0x3fff,
        b & 0xffff_ffff_ffff
    )
}

pub fn mobileconfig(ca_der: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(ca_der);
    let (inner, outer) = (uuid_from(ca_der, 1), uuid_from(ca_der, 2));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>PayloadContent</key>
  <array>
    <dict>
      <key>PayloadCertificateFileName</key><string>lynkei-ca.cer</string>
      <key>PayloadContent</key><data>{b64}</data>
      <key>PayloadDescription</key><string>Lets Lynkei read HTTPS traffic from this device while you debug.</string>
      <key>PayloadDisplayName</key><string>Lynkei CA</string>
      <key>PayloadIdentifier</key><string>dev.lynkei.ca.cert</string>
      <key>PayloadType</key><string>com.apple.security.root</string>
      <key>PayloadUUID</key><string>{inner}</string>
      <key>PayloadVersion</key><integer>1</integer>
    </dict>
  </array>
  <key>PayloadDescription</key><string>Installs the Lynkei certificate authority.</string>
  <key>PayloadDisplayName</key><string>Lynkei CA</string>
  <key>PayloadIdentifier</key><string>dev.lynkei.ca</string>
  <key>PayloadRemovalDisallowed</key><false/>
  <key>PayloadType</key><string>Configuration</string>
  <key>PayloadUUID</key><string>{outer}</string>
  <key>PayloadVersion</key><integer>1</integer>
</dict>
</plist>
"#
    )
}

fn guide_html(host: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Connect to Lynkei</title>
<style>
  :root {{ color-scheme: dark light; }}
  body {{ font: 16px/1.5 -apple-system, system-ui, sans-serif; margin: 0; padding: 24px 20px 48px; background: #0D0E11; color: #E9EBF0; }}
  main {{ max-width: 520px; margin: 0 auto; }}
  h1 {{ font-size: 24px; margin: 0 0 4px; letter-spacing: -0.02em; }}
  p.sub {{ color: #A1A7B4; margin: 0 0 24px; }}
  ol {{ padding-left: 0; list-style: none; counter-reset: s; }}
  li {{ counter-increment: s; background: #13151A; border: 1px solid rgba(255,255,255,.1); border-radius: 14px; padding: 16px; margin: 0 0 12px; }}
  li::before {{ content: counter(s); display: inline-grid; place-items: center; width: 24px; height: 24px; border-radius: 12px; background: #7C8CFF; color: #0D0E11; font-weight: 700; font-size: 13px; margin-right: 10px; }}
  a.btn {{ display: block; margin-top: 12px; text-align: center; background: #7C8CFF; color: #0D0E11; font-weight: 650; text-decoration: none; padding: 12px; border-radius: 10px; }}
  code {{ background: rgba(255,255,255,.08); padding: 1px 6px; border-radius: 5px; font-size: 14px; }}
  small {{ color: #8A91A0; }}
</style></head>
<body><main>
<h1>Connect to Lynkei</h1>
<p class="sub">Three steps. Takes about a minute.</p>
<ol>
  <li><b>Install the certificate</b><br>Tap the button, allow the download, then open <b>Settings → Profile Downloaded → Install</b>.
    <a class="btn" href="/lynkei.mobileconfig">Download profile</a></li>
  <li><b>Trust it</b><br>Settings → General → About → Certificate Trust Settings → turn on <b>Lynkei CA</b>.<br><small>This is what lets Lynkei read HTTPS. Remove the profile any time to undo it.</small></li>
  <li><b>Use the proxy</b><br>Settings → Wi-Fi → ⓘ next to your network → Configure Proxy → <b>Automatic</b>, URL:<br><code>http://{host}/proxy.pac</code></li>
</ol>
<p><small>Android: download <a href="/ca.crt" style="color:#7C8CFF">the certificate</a>, install it from Settings → Security → Encryption &amp; credentials, and set the Wi-Fi proxy to <code>{host}</code>. Apps that ignore user certificates need <code>lynkei-debug-lib</code> in their debug build.</small></p>
</main></body></html>
"#
    )
}

/// The Android version of the guide: certificate, then the Wi-Fi proxy. `host` is `ip:port`.
fn android_html(host: &str) -> String {
    let (name, port) = host.rsplit_once(':').unwrap_or((host, "8888"));
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Connect an Android phone to Lynkei</title>
<style>
  :root {{ color-scheme: dark light; }}
  body {{ font: 16px/1.5 system-ui, sans-serif; margin: 0; padding: 24px 20px 48px; background: #0D0E11; color: #E9EBF0; }}
  main {{ max-width: 520px; margin: 0 auto; }}
  h1 {{ font-size: 24px; margin: 0 0 4px; letter-spacing: -0.02em; }}
  p.sub {{ color: #A1A7B4; margin: 0 0 24px; }}
  ol {{ padding-left: 0; list-style: none; counter-reset: s; }}
  li {{ counter-increment: s; background: #13151A; border: 1px solid rgba(255,255,255,.1); border-radius: 14px; padding: 16px; margin: 0 0 12px; }}
  li::before {{ content: counter(s); display: inline-grid; place-items: center; width: 24px; height: 24px; border-radius: 12px; background: #7C8CFF; color: #0D0E11; font-weight: 700; font-size: 13px; margin-right: 10px; }}
  a.btn {{ display: block; margin-top: 12px; text-align: center; background: #7C8CFF; color: #0D0E11; font-weight: 650; text-decoration: none; padding: 12px; border-radius: 10px; }}
  code {{ background: rgba(255,255,255,.08); padding: 1px 6px; border-radius: 5px; font-size: 14px; }}
  small {{ color: #8A91A0; }}
</style></head>
<body><main>
<h1>Connect an Android phone</h1>
<p class="sub">Two steps. Your phone and the Mac must be on the same Wi-Fi.</p>
<ol>
  <li><b>Install the certificate</b><br>Tap the button to download it, then install it from Settings:<br>
    <small>Pixel and most phones: Settings → Security → Encryption &amp; credentials → Install a certificate → CA certificate → Install anyway.<br>
    Samsung: Settings → Security and privacy → More security settings → Install from device storage → CA certificate.</small><br>
    <small>Pick <b>lynkei-ca.crt</b> from Downloads. A screen lock is required.</small>
    <a class="btn" href="/ca.crt">Download certificate</a></li>
  <li><b>Use the proxy</b><br>Settings → Wi-Fi → tap your network (gear icon) → Advanced → Proxy → <b>Manual</b>:<br>
    Host <code>{name}</code> · Port <code>{port}</code><br>
    <small>Or choose <b>Auto-config</b> with <code>http://{host}/proxy.pac</code>. To undo, set Proxy back to None.</small></li>
</ol>
<p><small>Android apps built for Android 7 or newer ignore certificates you install yourself unless the app is a debug build that trusts them, so most store apps stay unreadable. Browsers and your own debug builds work. Lynkei shows which hosts it could not read.</small></p>
</main></body></html>
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_every_page_and_rejects_unknown_paths() {
        let der = [0x30, 0x82, 0x01, 0x0a, 1, 2, 3];
        for (p, ct) in [
            ("/setup", "text/html"),
            ("/android", "text/html"),
            ("/", "text/html"),
            ("/lynkei.mobileconfig", "application/x-apple-aspen-config"),
            ("/proxy.pac", "application/x-ns-proxy-autoconfig"),
            ("/ca.pem", "application/x-x509-ca-cert"),
            ("/ca.crt?x=1", "application/x-x509-ca-cert"),
        ] {
            let page = route(p, "mac.local:8888", "PEM", &der).unwrap_or_else(|| panic!("{p}"));
            assert!(page.content_type.starts_with(ct), "{p}");
        }
        assert!(route("/nope", "mac.local:8888", "PEM", &der).is_none());
    }

    #[test]
    fn pac_names_the_host_the_phone_used_and_falls_back_to_direct() {
        let pac = pac("jains-mbp.local:8888");
        assert!(pac.contains("PROXY jains-mbp.local:8888; DIRECT"));
    }

    #[test]
    fn mobileconfig_embeds_the_certificate_with_stable_ids() {
        let der = b"fake-der-bytes".to_vec();
        let a = mobileconfig(&der);
        assert!(a.contains(&base64::engine::general_purpose::STANDARD.encode(&der)));
        assert!(a.contains("com.apple.security.root"));
        assert_eq!(
            a,
            mobileconfig(&der),
            "same certificate, same profile (reinstall replaces)"
        );
        assert_ne!(a, mobileconfig(b"another"));
        let uuid = uuid_from(&der, 1);
        assert_eq!(uuid.len(), 36);
        assert!(uuid.chars().filter(|c| *c == '-').count() == 4 && a.contains(&uuid));
        assert_ne!(
            uuid_from(&der, 1),
            uuid_from(&der, 2),
            "inner and outer payloads differ"
        );
    }

    #[test]
    fn android_guide_names_the_proxy_host_and_port() {
        let h = android_html("192.168.1.5:8888");
        assert!(h.contains("<code>192.168.1.5</code>") && h.contains("<code>8888</code>"));
        assert!(h.contains("/ca.crt") && h.contains("http://192.168.1.5:8888/proxy.pac"));
    }

    #[test]
    fn guide_points_at_the_pac_and_profile() {
        let h = guide_html("mac.local:8888");
        assert!(
            h.contains("http://mac.local:8888/proxy.pac") && h.contains("/lynkei.mobileconfig")
        );
    }
}
