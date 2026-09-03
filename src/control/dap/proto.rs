// SPDX-License-Identifier: GPL-3.0-or-later

//! The DAP base protocol: `Content-Length: N\r\n\r\n<json>` framed
//! messages, each a request, a response or an event, with a per-side
//! sequence number. Hand-rolled over `serde_json::Value`, like the
//! control protocol's own wire code, so the adapter stays std-only.

use serde_json::{json, Map, Value};
use std::io::{self, BufRead, Write};

/// Largest message body accepted (a client sending more is broken).
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// Largest header line and most header lines per message.
const MAX_HEADER_LINE: u64 = 8 * 1024;
const MAX_HEADERS: usize = 32;

/// A request from the client.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub seq: i64,
    pub command: String,
    pub arguments: Value,
}

/// Read one framed message; `None` at a clean end of stream.
pub fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    let mut headers = 0usize;
    loop {
        let mut line = String::new();
        let n = std::io::Read::take(&mut *reader, MAX_HEADER_LINE).read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        if !line.ends_with('\n') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "header line too long",
            ));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        headers += 1;
        if headers > MAX_HEADERS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "too many headers",
            ));
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                let len = value.trim().parse::<usize>().map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("bad Content-Length: {e}"),
                    )
                })?;
                if len > MAX_BODY_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("message of {len} bytes exceeds the limit"),
                    ));
                }
                content_length = Some(len);
            }
            // Other headers (Content-Type) are ignored, per the spec.
        }
    }
    let Some(len) = content_length else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message without a Content-Length header",
        ));
    };
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad message JSON: {e}")))
}

/// Frame and write one message.
pub fn write_message<W: Write>(writer: &mut W, message: &Value) -> io::Result<()> {
    let body = message.to_string();
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body.as_bytes())?;
    writer.flush()
}

/// Interpret a decoded message as a request.
pub fn as_request(message: &Value) -> Option<Request> {
    if message.get("type").and_then(Value::as_str) != Some("request") {
        return None;
    }
    Some(Request {
        seq: message.get("seq").and_then(Value::as_i64).unwrap_or(0),
        command: message.get("command")?.as_str()?.to_string(),
        arguments: message.get("arguments").cloned().unwrap_or(Value::Null),
    })
}

/// The outgoing side: numbers responses and events.
#[derive(Debug, Default)]
pub struct Outgoing {
    next_seq: i64,
}

impl Outgoing {
    fn seq(&mut self) -> i64 {
        self.next_seq += 1;
        self.next_seq
    }

    pub fn response(&mut self, req: &Request, body: Value) -> Value {
        let mut msg = Map::new();
        msg.insert("seq".into(), json!(self.seq()));
        msg.insert("type".into(), json!("response"));
        msg.insert("request_seq".into(), json!(req.seq));
        msg.insert("success".into(), json!(true));
        msg.insert("command".into(), json!(req.command));
        if !body.is_null() {
            msg.insert("body".into(), body);
        }
        Value::Object(msg)
    }

    /// An error response. `message` is the short form shown to the
    /// user; the same text goes into `body.error` for clients that read
    /// the structured form.
    pub fn error(&mut self, req: &Request, message: impl Into<String>) -> Value {
        let message = message.into();
        json!({
            "seq": self.seq(),
            "type": "response",
            "request_seq": req.seq,
            "success": false,
            "command": req.command,
            "message": message,
            "body": {"error": {"id": 1, "format": message, "showUser": true}},
        })
    }

    pub fn event(&mut self, name: &str, body: Value) -> Value {
        let mut msg = Map::new();
        msg.insert("seq".into(), json!(self.seq()));
        msg.insert("type".into(), json!("event"));
        msg.insert("event".into(), json!(name));
        if !body.is_null() {
            msg.insert("body".into(), body);
        }
        Value::Object(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frames_round_trip_and_split_reads_reassemble() {
        let mut buf = Vec::new();
        write_message(
            &mut buf,
            &json!({"seq": 1, "type": "request", "command": "initialize"}),
        )
        .unwrap();
        write_message(
            &mut buf,
            &json!({"seq": 2, "type": "request", "command": "launch"}),
        )
        .unwrap();
        let text = String::from_utf8(buf.clone()).unwrap();
        assert!(text.starts_with("Content-Length: "), "{text}");
        let mut reader = Cursor::new(buf);
        let first = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(first["command"], "initialize");
        let second = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(second["command"], "launch");
        assert!(read_message(&mut reader).unwrap().is_none(), "clean EOF");
    }

    #[test]
    fn other_headers_are_tolerated_and_missing_length_is_an_error() {
        let body = r#"{"seq":3,"type":"request","command":"threads"}"#;
        let framed = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let msg = read_message(&mut Cursor::new(framed.into_bytes()))
            .unwrap()
            .unwrap();
        assert_eq!(as_request(&msg).unwrap().command, "threads");
        let err = read_message(&mut Cursor::new(b"Foo: bar\r\n\r\n{}".to_vec())).unwrap_err();
        assert!(err.to_string().contains("Content-Length"));
    }

    #[test]
    fn responses_and_events_carry_sequence_numbers() {
        let mut out = Outgoing::default();
        let req = Request {
            seq: 7,
            command: "pause".into(),
            arguments: Value::Null,
        };
        let ok = out.response(&req, Value::Null);
        assert_eq!(ok["seq"], 1);
        assert_eq!(ok["request_seq"], 7);
        assert_eq!(ok["success"], true);
        assert!(ok.get("body").is_none());
        let err = out.error(&req, "nope");
        assert_eq!(err["seq"], 2);
        assert_eq!(err["success"], false);
        assert_eq!(err["message"], "nope");
        assert_eq!(err["body"]["error"]["format"], "nope");
        let ev = out.event("stopped", json!({"reason": "pause"}));
        assert_eq!(ev["seq"], 3);
        assert_eq!(ev["event"], "stopped");
        assert_eq!(ev["body"]["reason"], "pause");
    }
}
