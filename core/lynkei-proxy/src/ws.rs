//! WebSocket message capture (RFC 6455). Bytes are relayed untouched; this only observes them.

use std::time::Instant;

use lynkei_model::{WsDirection, WsKind, WsMessage};

/// Messages kept per connection, and preview size per message.
const MAX_MESSAGES: usize = 500;
const PREVIEW_BYTES: usize = 4096;

/// Incremental frame parser for one direction of a WebSocket connection.
#[derive(Debug)]
struct FrameParser {
    dir: WsDirection,
    buf: Vec<u8>,
    /// Bytes of the current frame's payload still to skip after we stopped buffering it.
    skip: u64,
}

impl FrameParser {
    fn new(dir: WsDirection) -> Self {
        Self {
            dir,
            buf: Vec::new(),
            skip: 0,
        }
    }
}

/// Records the messages of one WebSocket connection, both directions.
#[derive(Debug)]
pub struct WsRecorder {
    opened: Instant,
    send: FrameParser,
    receive: FrameParser,
    pub messages: Vec<WsMessage>,
    pub dropped: u64,
}

impl Default for WsRecorder {
    fn default() -> Self {
        Self {
            opened: Instant::now(),
            send: FrameParser::new(WsDirection::Send),
            receive: FrameParser::new(WsDirection::Receive),
            messages: Vec::new(),
            dropped: 0,
        }
    }
}

impl WsRecorder {
    /// Feeds bytes that passed in `dir` (Send = app → server).
    pub fn feed(&mut self, dir: WsDirection, bytes: &[u8]) {
        let at_ms = self.opened.elapsed().as_millis() as u64;
        let parser = match dir {
            WsDirection::Send => &mut self.send,
            WsDirection::Receive => &mut self.receive,
        };
        let mut out = Vec::new();
        parse(parser, bytes, at_ms, &mut out);
        for m in out {
            if self.messages.len() < MAX_MESSAGES {
                self.messages.push(m);
            } else {
                self.dropped += 1;
            }
        }
    }
}

fn parse(p: &mut FrameParser, mut bytes: &[u8], at_ms: u64, out: &mut Vec<WsMessage>) {
    // Skip the tail of a huge frame we already recorded.
    if p.skip > 0 {
        let n = (p.skip as usize).min(bytes.len());
        p.skip -= n as u64;
        bytes = &bytes[n..];
    }
    p.buf.extend_from_slice(bytes);
    loop {
        let Some(h) = header(&p.buf) else { return };
        let total = h.header_len as u64 + h.len;
        let have = p.buf.len() as u64;
        // Big frame: record from what we have once the preview is covered, skip the rest.
        let preview_ready = have >= h.header_len as u64 + (h.len.min(PREVIEW_BYTES as u64));
        if have < total && !(h.len > PREVIEW_BYTES as u64 && preview_ready) {
            return;
        }
        let avail = (have.min(total) as usize) - h.header_len;
        let mut payload = p.buf[h.header_len..h.header_len + avail.min(PREVIEW_BYTES)].to_vec();
        if let Some(key) = h.mask {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= key[i % 4];
            }
        }
        out.push(message(p.dir, &h, payload, at_ms));
        if have >= total {
            p.buf.drain(..total as usize);
        } else {
            p.skip = total - have;
            p.buf.clear();
            return;
        }
    }
}

struct Header {
    opcode: u8,
    compressed: bool,
    mask: Option<[u8; 4]>,
    len: u64,
    header_len: usize,
}

fn header(b: &[u8]) -> Option<Header> {
    if b.len() < 2 {
        return None;
    }
    let opcode = b[0] & 0x0f;
    let compressed = b[0] & 0x40 != 0; // RSV1 = permessage-deflate
    let masked = b[1] & 0x80 != 0;
    let (len, mut i) = match b[1] & 0x7f {
        126 => (u16::from_be_bytes([*b.get(2)?, *b.get(3)?]) as u64, 4),
        127 => (u64::from_be_bytes(b.get(2..10)?.try_into().ok()?), 10),
        n => (n as u64, 2),
    };
    let mask = if masked {
        let k: [u8; 4] = b.get(i..i + 4)?.try_into().ok()?;
        i += 4;
        Some(k)
    } else {
        None
    };
    Some(Header {
        opcode,
        compressed,
        mask,
        len,
        header_len: i,
    })
}

fn message(dir: WsDirection, h: &Header, payload: Vec<u8>, at_ms: u64) -> WsMessage {
    let kind = match h.opcode {
        0x0 => WsKind::Continuation,
        0x1 => WsKind::Text,
        0x2 => WsKind::Binary,
        0x8 => WsKind::Close,
        0x9 => WsKind::Ping,
        0xA => WsKind::Pong,
        _ => WsKind::Other,
    };
    let preview = if h.compressed {
        Some("[compressed with permessage-deflate]".to_string())
    } else {
        match kind {
            WsKind::Text | WsKind::Continuation => {
                Some(String::from_utf8_lossy(&payload).into_owned())
            }
            WsKind::Close if payload.len() >= 2 => {
                let code = u16::from_be_bytes([payload[0], payload[1]]);
                Some(
                    format!("{code} {}", String::from_utf8_lossy(&payload[2..]))
                        .trim()
                        .to_string(),
                )
            }
            _ => None,
        }
    };
    WsMessage {
        direction: dir,
        kind,
        size: h.len,
        preview,
        at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(opcode: u8, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
        let mut f = vec![0x80 | opcode];
        let m = if mask.is_some() { 0x80 } else { 0 };
        match payload.len() {
            n if n < 126 => f.push(m | n as u8),
            n if n <= u16::MAX as usize => {
                f.push(m | 126);
                f.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                f.push(m | 127);
                f.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        match mask {
            Some(k) => {
                f.extend_from_slice(&k);
                f.extend(payload.iter().enumerate().map(|(i, b)| b ^ k[i % 4]));
            }
            None => f.extend_from_slice(payload),
        }
        f
    }

    #[test]
    fn records_masked_client_and_plain_server_text() {
        let mut r = WsRecorder::default();
        r.feed(WsDirection::Send, &frame(0x1, b"hello", Some([1, 2, 3, 4])));
        r.feed(WsDirection::Receive, &frame(0x1, b"echo: hello", None));
        assert_eq!(r.messages.len(), 2);
        assert_eq!(
            (r.messages[0].direction, r.messages[0].kind),
            (WsDirection::Send, WsKind::Text)
        );
        assert_eq!(
            r.messages[0].preview.as_deref(),
            Some("hello"),
            "client frames are unmasked"
        );
        assert_eq!(r.messages[1].preview.as_deref(), Some("echo: hello"));
    }

    #[test]
    fn handles_frames_split_across_reads_and_several_per_read() {
        let mut r = WsRecorder::default();
        let mut bytes = frame(0x1, b"one", None);
        bytes.extend(frame(0x2, &[0, 1, 2], None));
        bytes.extend(frame(0x8, &[0x03, 0xE8, b'b', b'y', b'e'], None));
        let (a, b) = bytes.split_at(4);
        r.feed(WsDirection::Receive, a);
        r.feed(WsDirection::Receive, b);
        let kinds: Vec<_> = r.messages.iter().map(|m| m.kind).collect();
        assert_eq!(kinds, vec![WsKind::Text, WsKind::Binary, WsKind::Close]);
        assert_eq!(r.messages[2].preview.as_deref(), Some("1000 bye"));
    }

    #[test]
    fn big_frames_are_recorded_without_buffering_them_whole() {
        let mut r = WsRecorder::default();
        let big = vec![b'x'; 200_000];
        let bytes = frame(0x1, &big, None);
        for chunk in bytes.chunks(8192) {
            r.feed(WsDirection::Receive, chunk);
        }
        r.feed(WsDirection::Receive, &frame(0x1, b"after", None));
        assert_eq!(r.messages.len(), 2);
        assert_eq!(r.messages[0].size, 200_000);
        assert_eq!(r.messages[0].preview.as_ref().unwrap().len(), PREVIEW_BYTES);
        assert_eq!(
            r.messages[1].preview.as_deref(),
            Some("after"),
            "parser resyncs after the skipped tail"
        );
    }
}
