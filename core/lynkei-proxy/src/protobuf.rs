//! Schema-less Protocol Buffers and gRPC previews.
//!
//! Without a `.proto` file the field names are unknown, so a message is shown as JSON keyed by field
//! number: `{"1": "hello", "2": 42, "3": {"1": "nested"}}`. Strings and nested messages are
//! recognised by their shape; anything else is shown as hex. Good enough to read a gRPC call.

use std::fmt::Write;

enum Node {
    Int(u64),
    Fixed64(u64),
    Fixed32(u32),
    Text(String),
    Bytes(Vec<u8>),
    Message(Vec<(u32, Node)>),
}

const MAX_DEPTH: usize = 8;

fn varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..70).step_by(7) {
        let byte = *b.get(*i)?;
        *i += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// Parses `b` as one whole message, or `None` if it is not one.
fn message(b: &[u8], depth: usize) -> Option<Vec<(u32, Node)>> {
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let key = varint(b, &mut i)?;
        let num = u32::try_from(key >> 3)
            .ok()
            .filter(|n| (1..1 << 29).contains(n))?;
        let node = match key & 7 {
            0 => Node::Int(varint(b, &mut i)?),
            1 => {
                let s = b.get(i..i + 8)?;
                i += 8;
                Node::Fixed64(u64::from_le_bytes(s.try_into().ok()?))
            }
            5 => {
                let s = b.get(i..i + 4)?;
                i += 4;
                Node::Fixed32(u32::from_le_bytes(s.try_into().ok()?))
            }
            2 => {
                let len = usize::try_from(varint(b, &mut i)?).ok()?;
                let s = b.get(i..i.checked_add(len)?)?;
                i += len;
                bytes_node(s, depth)
            }
            _ => return None,
        };
        out.push((num, node));
    }
    Some(out)
}

fn bytes_node(s: &[u8], depth: usize) -> Node {
    if s.is_empty() {
        return Node::Text(String::new());
    }
    if let Ok(t) = std::str::from_utf8(s)
        && t.chars()
            .all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
    {
        return Node::Text(t.to_string());
    }
    if depth < MAX_DEPTH
        && let Some(m) = message(s, depth + 1)
        && !m.is_empty()
    {
        return Node::Message(m);
    }
    Node::Bytes(s.to_vec())
}

fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

fn pad(out: &mut String, n: usize) {
    for _ in 0..n {
        out.push_str("  ");
    }
}

fn scalar(out: &mut String, n: &Node) {
    match n {
        Node::Int(v) => {
            let _ = write!(out, "{v}");
        }
        Node::Fixed64(v) => {
            let _ = write!(out, "\"fixed64:{v}\"");
        }
        Node::Fixed32(v) => {
            let _ = write!(out, "\"fixed32:{v}\"");
        }
        Node::Text(t) => out.push_str(&quote(t)),
        Node::Bytes(b) => {
            let hex: String = b.iter().take(64).map(|x| format!("{x:02x}")).collect();
            let more = if b.len() > 64 { "…" } else { "" };
            let _ = write!(out, "\"0x{hex}{more} ({} bytes)\"", b.len());
        }
        Node::Message(_) => {}
    }
}

fn emit(out: &mut String, fields: &[(u32, Node)], depth: usize) {
    if fields.is_empty() {
        out.push_str("{}");
        return;
    }
    let mut nums: Vec<u32> = fields.iter().map(|f| f.0).collect();
    nums.sort_unstable();
    nums.dedup();
    out.push_str("{\n");
    for (k, num) in nums.iter().enumerate() {
        let vals: Vec<&Node> = fields
            .iter()
            .filter(|f| f.0 == *num)
            .map(|f| &f.1)
            .collect();
        pad(out, depth + 1);
        let _ = write!(out, "\"{num}\": ");
        let one = |out: &mut String, n: &Node, d: usize| match n {
            Node::Message(m) => emit(out, m, d),
            other => scalar(out, other),
        };
        if vals.len() == 1 {
            one(out, vals[0], depth + 1);
        } else {
            out.push_str("[\n");
            for (j, v) in vals.iter().enumerate() {
                pad(out, depth + 2);
                one(out, v, depth + 2);
                out.push_str(if j + 1 < vals.len() { ",\n" } else { "\n" });
            }
            pad(out, depth + 1);
            out.push(']');
        }
        out.push_str(if k + 1 < nums.len() { ",\n" } else { "\n" });
    }
    pad(out, depth);
    out.push('}');
}

/// A protobuf message as JSON keyed by field number.
pub fn decode_message(b: &[u8]) -> Option<String> {
    let m = message(b, 0)?;
    if m.is_empty() {
        return None;
    }
    let mut out = String::new();
    emit(&mut out, &m, 0);
    Some(out)
}

/// A gRPC (or gRPC-web) body: its length-prefixed messages, decoded. `inflate` handles a gzip
/// compressed message. One message is shown alone; several as a list.
pub fn decode_grpc(b: &[u8], inflate: impl Fn(&[u8]) -> Option<Vec<u8>>) -> Option<String> {
    let mut i = 0;
    let mut shown: Vec<String> = Vec::new();
    while i + 5 <= b.len() {
        let flag = b[i];
        let len = u32::from_be_bytes(b[i + 1..i + 5].try_into().ok()?) as usize;
        let end = (i + 5).checked_add(len)?;
        let frame = b.get(i + 5..end)?;
        i = end;
        if flag & 0x80 != 0 {
            continue; // gRPC-web trailers
        }
        let text = if flag & 1 == 1 {
            match inflate(frame) {
                Some(raw) => decode_message(&raw),
                None => Some("\"[compressed message]\"".to_string()),
            }
        } else if frame.is_empty() {
            Some("{}".to_string())
        } else {
            decode_message(frame)
        };
        shown.push(text?);
    }
    match shown.len() {
        0 => None,
        1 => shown.pop(),
        _ => Some(format!("[\n{}\n]", shown.join(",\n"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // {1: "hi", 2: 150, 3: {1: "x"}}
    const MSG: &[u8] = &[
        0x0a, 2, b'h', b'i', 0x10, 0x96, 0x01, 0x1a, 3, 0x0a, 1, b'x',
    ];

    #[test]
    fn decodes_without_a_schema() {
        let t = decode_message(MSG).unwrap();
        let v: serde_json::Value = serde_json::from_str(&t).unwrap();
        assert_eq!(v["1"], "hi");
        assert_eq!(v["2"], 150);
        assert_eq!(v["3"]["1"], "x");
    }

    #[test]
    fn repeated_fields_become_lists() {
        let m = [0x0a, 1, b'a', 0x0a, 1, b'b'];
        let v: serde_json::Value = serde_json::from_str(&decode_message(&m).unwrap()).unwrap();
        assert_eq!(v["1"], serde_json::json!(["a", "b"]));
    }

    #[test]
    fn unframes_grpc() {
        let mut body = vec![0, 0, 0, 0, MSG.len() as u8];
        body.extend_from_slice(MSG);
        let t = decode_grpc(&body, |_| None).unwrap();
        assert!(t.contains("\"hi\""));
        assert!(decode_grpc(b"not grpc", |_| None).is_none());
    }

    #[test]
    fn garbage_is_not_a_message() {
        assert!(decode_message(&[0xff, 0xff, 0xff]).is_none());
    }
}
