//! Protocol sniffing on the first bytes of a connection: TLS ClientHello (SNI) and plain HTTP.
//!
//! Needed for transparent/VPN traffic, where the destination is an IP address and the hostname
//! only appears inside the ClientHello or the HTTP `Host` header.

/// TLS record header length.
const RECORD_HEADER: usize = 5;

/// Total bytes of the first TLS record (header + body) if `buf` starts with a handshake record.
pub fn tls_record_len(buf: &[u8]) -> Option<usize> {
    if buf.len() < RECORD_HEADER || buf[0] != 0x16 || buf[1] != 0x03 {
        return None;
    }
    Some(RECORD_HEADER + u16::from_be_bytes([buf[3], buf[4]]) as usize)
}

/// Extracts the server name (SNI) from a TLS ClientHello. Returns `None` if absent or malformed.
pub fn parse_sni(buf: &[u8]) -> Option<String> {
    let rec_len = tls_record_len(buf)?;
    let rec = buf.get(RECORD_HEADER..rec_len.min(buf.len()))?;
    let mut r = Reader(rec);
    if r.u8()? != 0x01 {
        return None; // not a ClientHello
    }
    let _hs_len = r.u24()?;
    r.skip(2 + 32)?; // legacy_version + random
    let sid = r.u8()? as usize;
    r.skip(sid)?;
    let suites = r.u16()? as usize;
    r.skip(suites)?;
    let comp = r.u8()? as usize;
    r.skip(comp)?;
    let ext_total = r.u16()? as usize;
    let mut exts = Reader(r.take(ext_total)?);
    while !exts.0.is_empty() {
        let ty = exts.u16()?;
        let len = exts.u16()? as usize;
        let body = exts.take(len)?;
        if ty == 0x0000 {
            let mut sn = Reader(body);
            let list_len = sn.u16()? as usize;
            let mut list = Reader(sn.take(list_len)?);
            while !list.0.is_empty() {
                let name_type = list.u8()?;
                let name_len = list.u16()? as usize;
                let name = list.take(name_len)?;
                if name_type == 0 {
                    let s = std::str::from_utf8(name).ok()?;
                    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_graphic()) {
                        return Some(s.to_ascii_lowercase());
                    }
                }
            }
        }
    }
    None
}

/// True if `buf` starts with an HTTP/1.x request line method.
pub fn looks_like_http(buf: &[u8]) -> bool {
    const METHODS: &[&[u8]] = &[
        b"GET ", b"POST ", b"PUT ", b"HEAD ", b"DELETE ", b"PATCH ", b"OPTIONS ", b"TRACE ",
    ];
    METHODS.iter().any(|m| buf.starts_with(m))
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(|_| ())
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
    fn u24(&mut self) -> Option<u32> {
        self.take(3).map(|b| u32::from_be_bytes([0, b[0], b[1], b[2]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A real ClientHello produced by rustls for `host`.
    fn client_hello(host: &str) -> Vec<u8> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from(host.to_string()).unwrap();
        let mut conn = rustls::ClientConnection::new(Arc::new(cfg), name).unwrap();
        let mut out = Vec::new();
        conn.write_tls(&mut out).unwrap();
        out
    }

    #[test]
    fn extracts_sni_from_real_client_hello() {
        let hello = client_hello("API.AcmeShop.com");
        assert_eq!(tls_record_len(&hello), Some(hello.len()));
        assert_eq!(parse_sni(&hello).as_deref(), Some("api.acmeshop.com"));
    }

    #[test]
    fn rejects_truncated_and_non_tls_input() {
        let hello = client_hello("example.com");
        assert_eq!(parse_sni(&hello[..20]), None);
        assert_eq!(parse_sni(b"GET / HTTP/1.1\r\n"), None);
        assert_eq!(tls_record_len(b"\x16\x03"), None);
    }

    #[test]
    fn detects_plain_http() {
        assert!(looks_like_http(b"GET /v2/products HTTP/1.1\r\n"));
        assert!(looks_like_http(b"POST /graphql HTTP/1.1\r\n"));
        assert!(!looks_like_http(b"\x16\x03\x01\x02\x00"));
        assert!(!looks_like_http(b"SSH-2.0-OpenSSH"));
    }
}
