// SPDX-License-Identifier: GPL-3.0-or-later

//! Wire layer of the control protocol: JSON-RPC 2.0 messages, one JSON
//! object per newline-terminated UTF-8 line, plus the auth handshake and
//! the small payload codecs. This module knows nothing about the
//! emulator; both server modes (headless owner and windowed drain) speak
//! through it.
//!
//! Protocol rules enforced here:
//! - every client request must carry an `id` (server notifications to the
//!   client carry none, per JSON-RPC 2.0);
//! - the first successful `hello {token}` or `auth {token}` authenticates
//!   the connection; a wrong token gets one error reply and the
//!   connection is closed; anything else before auth is refused;
//! - `hello` never exposes machine state, only version fields, so it is
//!   safe to answer from the socket thread pre-auth.

use serde::Serialize;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

/// Control protocol version returned by `hello`. Bump on breaking wire
/// changes; additive fields and methods do not bump it.
pub const PROTO_VERSION: u32 = 1;

/// Cap on one wire line, so a hostile or broken client cannot balloon
/// the reader; generous enough for a 1 MiB `mem.write` payload as hex
/// plus JSON overhead.
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

// JSON-RPC 2.0 standard error codes.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
// Implementation-defined error codes (-32000..-32099 reserved range).
pub const AUTH_FAILED: i64 = -32000;
pub const NOT_AUTHED: i64 = -32001;
pub const RESUME_PENDING: i64 = -32002;
pub const INVALID_STATE: i64 = -32003;
pub const UNSUPPORTED: i64 = -32004;
pub const IO_ERROR: i64 = -32005;
pub const HISTORY_EXHAUSTED: i64 = -32006;
pub const NOT_FOUND: i64 = -32007;

/// A protocol-level error: code plus human-readable message, rendered
/// into the JSON-RPC `error` object.
#[derive(Debug, Clone, PartialEq)]
pub struct CtlError {
    pub code: i64,
    pub message: String,
}

impl CtlError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, message)
    }

    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(INVALID_STATE, message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(UNSUPPORTED, message)
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new(IO_ERROR, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(NOT_FOUND, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(INTERNAL_ERROR, message)
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(METHOD_NOT_FOUND, format!("unknown method: {method}"))
    }

    pub fn auth_failed() -> Self {
        Self::new(AUTH_FAILED, "auth failed")
    }

    pub fn not_authed() -> Self {
        Self::new(
            NOT_AUTHED,
            "not authenticated; call hello or auth with the session token",
        )
    }
}

/// A parsed client request. `id` is kept as raw JSON (number or string)
/// and echoed back verbatim in the response.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcRequest {
    pub id: Value,
    pub method: String,
    pub params: Value,
}

/// Parse one wire line into a request. On failure, returns the response
/// line to send back (JSON-RPC prescribes `id: null` for unparseable
/// requests, the echoed id for malformed ones).
pub fn parse_request(line: &str) -> Result<RpcRequest, String> {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Err(err_line(
                &Value::Null,
                &CtlError::new(PARSE_ERROR, format!("parse error: {e}")),
            ))
        }
    };
    let Some(obj) = value.as_object() else {
        return Err(err_line(
            &Value::Null,
            &CtlError::new(INVALID_REQUEST, "request must be a JSON object"),
        ));
    };
    let id = obj.get("id").cloned().unwrap_or(Value::Null);
    if let Some(version) = obj.get("jsonrpc") {
        if version.as_str() != Some("2.0") {
            return Err(err_line(
                &id,
                &CtlError::new(INVALID_REQUEST, "jsonrpc must be \"2.0\""),
            ));
        }
    }
    if id.is_null() {
        return Err(err_line(
            &Value::Null,
            &CtlError::new(INVALID_REQUEST, "requests must carry an id"),
        ));
    }
    let Some(method) = obj.get("method").and_then(|m| m.as_str()) else {
        return Err(err_line(
            &id,
            &CtlError::new(INVALID_REQUEST, "request has no method"),
        ));
    };
    Ok(RpcRequest {
        id,
        method: method.to_string(),
        params: obj.get("params").cloned().unwrap_or(Value::Null),
    })
}

/// Serialize a success response line (no trailing newline).
pub fn ok_line(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

/// Serialize an error response line (no trailing newline).
pub fn err_line(id: &Value, err: &CtlError) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": err.code, "message": err.message}})
        .to_string()
}

/// Serialize a server-to-client notification line (no trailing newline).
pub fn event_line(method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string()
}

/// Write one message line and flush, so a blocked emulator never sits on
/// a buffered reply.
pub fn write_line(w: &mut impl Write, line: &str) -> io::Result<()> {
    w.write_all(line.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()
}

/// Read the next non-blank message line. Returns `Ok(None)` on a clean
/// EOF; blank lines are skipped so interactive `nc` sessions stay
/// friendly. Enforces [`MAX_LINE_BYTES`].
pub fn read_msg_line<R: BufRead>(r: &mut R) -> io::Result<Option<String>> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let chunk = r.fill_buf()?;
        if chunk.is_empty() {
            if buf.iter().all(|b| b.is_ascii_whitespace()) {
                return Ok(None);
            }
            return line_from_utf8(buf);
        }
        let newline = chunk.iter().position(|&b| b == b'\n');
        let take = newline.unwrap_or(chunk.len());
        buf.extend_from_slice(&chunk[..take]);
        let consumed = newline.map_or(take, |p| p + 1);
        r.consume(consumed);
        if buf.len() > MAX_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "control message exceeds the line limit",
            ));
        }
        if newline.is_some() {
            if buf.iter().all(|b| b.is_ascii_whitespace()) {
                buf.clear();
                continue;
            }
            return line_from_utf8(buf);
        }
    }
}

fn line_from_utf8(buf: Vec<u8>) -> io::Result<Option<String>> {
    String::from_utf8(buf)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "control message is not UTF-8"))
}

/// The position/stop report returned by every resume verb and carried by
/// `event.stopped`: a consistent coordinate on the emulated timeline.
#[derive(Debug, Clone, Serialize)]
pub struct StopEvent {
    /// `breakpoint`, `watchpoint`, `reg_watch`, `beam_trap`,
    /// `copper_break`, `catch`, `step`, `target`, `pause`, `user_pause`,
    /// `double_fault`, `reverse`, or `history_partial`.
    pub reason: String,
    /// Human-readable detail (the debug-stop description, target spec...).
    pub detail: String,
    pub pc: u32,
    pub frame: u64,
    pub vpos: u16,
    pub hpos: u16,
    pub cck: u64,
    pub seconds: f64,
    pub retired_instructions: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collect: Option<Vec<Value>>,
}

/// Pre-auth gatekeeper shared by both server modes. `hello` and `auth`
/// are answered here and never reach the executor; anything else is
/// refused until a token has been presented.
pub struct AuthGate {
    token: String,
    authed: bool,
}

/// What to do with an incoming request, as decided by the [`AuthGate`].
#[derive(Debug, PartialEq)]
pub enum Gate {
    /// Authenticated request for the executor.
    Pass,
    /// Session-layer method handled here; send this line.
    Reply(String),
    /// Send this line, then drop the connection (failed auth).
    ReplyAndClose(String),
}

impl AuthGate {
    pub fn new(token: String) -> Self {
        Self {
            token,
            authed: false,
        }
    }

    pub fn authed(&self) -> bool {
        self.authed
    }

    pub fn handle(&mut self, req: &RpcRequest) -> Gate {
        match req.method.as_str() {
            "hello" => {
                if let Some(supplied) = req.params.get("token").and_then(Value::as_str) {
                    if supplied == self.token {
                        self.authed = true;
                    } else {
                        return Gate::ReplyAndClose(err_line(&req.id, &CtlError::auth_failed()));
                    }
                }
                Gate::Reply(ok_line(
                    &req.id,
                    json!({
                        "proto": PROTO_VERSION,
                        "emulator": concat!("copperline ", env!("CARGO_PKG_VERSION")),
                        "authed": self.authed,
                    }),
                ))
            }
            "auth" => match req.params.get("token").and_then(Value::as_str) {
                Some(supplied) if supplied == self.token => {
                    self.authed = true;
                    Gate::Reply(ok_line(&req.id, json!({"authed": true})))
                }
                _ => Gate::ReplyAndClose(err_line(&req.id, &CtlError::auth_failed())),
            },
            _ if !self.authed => Gate::Reply(err_line(&req.id, &CtlError::not_authed())),
            _ => Gate::Pass,
        }
    }
}

/// Lowercase hex encoding for memory payloads.
pub fn encode_hex(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 2);
    for b in data {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Decode hex (case-insensitive, no separators). `None` on any
/// malformed input.
pub fn decode_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding, for bulk memory payloads.
pub fn encode_base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(BASE64_ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(BASE64_ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Decode standard base64 (padding optional). `None` on any malformed
/// input, including nonzero discarded padding bits.
pub fn decode_base64(s: &str) -> Option<Vec<u8>> {
    fn val(b: u8) -> Option<u32> {
        match b {
            b'A'..=b'Z' => Some(u32::from(b - b'A')),
            b'a'..=b'z' => Some(u32::from(b - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(b - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let raw = s.trim().as_bytes();
    let stripped = raw
        .strip_suffix(b"==")
        .or_else(|| raw.strip_suffix(b"="))
        .unwrap_or(raw);
    if stripped.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(stripped.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &b in stripped {
        acc = (acc << 6) | val(b)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if bits > 0 && (acc & ((1 << bits) - 1)) != 0 {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn req(line: &str) -> RpcRequest {
        parse_request(line).expect("request should parse")
    }

    #[test]
    fn framing_reads_lines_skips_blanks_and_reports_eof() {
        let mut r = Cursor::new(b"{\"a\":1}\n\n   \n{\"b\":2}\nno-newline-tail".to_vec());
        assert_eq!(read_msg_line(&mut r).unwrap().as_deref(), Some("{\"a\":1}"));
        assert_eq!(read_msg_line(&mut r).unwrap().as_deref(), Some("{\"b\":2}"));
        assert_eq!(
            read_msg_line(&mut r).unwrap().as_deref(),
            Some("no-newline-tail")
        );
        assert_eq!(read_msg_line(&mut r).unwrap(), None);
    }

    #[test]
    fn framing_survives_split_reads() {
        // A one-byte buffer forces the smallest possible fill_buf chunks,
        // the worst case for reassembling a line from a TCP stream.
        let data = b"{\"method\":\"status\",\"id\":7}\n".to_vec();
        let mut r = std::io::BufReader::with_capacity(1, Cursor::new(data));
        let line = read_msg_line(&mut r).unwrap().unwrap();
        assert_eq!(req(&line).method, "status");
    }

    #[test]
    fn framing_caps_line_length() {
        let data = vec![b'x'; MAX_LINE_BYTES + 2];
        let mut r = Cursor::new(data);
        let err = read_msg_line(&mut r).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn parse_error_replies_with_null_id() {
        let line = parse_request("{not json").unwrap_err();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], Value::Null);
        assert_eq!(v["error"]["code"], PARSE_ERROR);
    }

    #[test]
    fn requests_require_id_and_method() {
        let line = parse_request("{\"method\":\"status\"}").unwrap_err();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["error"]["code"], INVALID_REQUEST);

        let line = parse_request("{\"id\":1}").unwrap_err();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], 1);
        assert_eq!(v["error"]["code"], INVALID_REQUEST);

        let line = parse_request("{\"jsonrpc\":\"1.0\",\"id\":1,\"method\":\"x\"}").unwrap_err();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["error"]["code"], INVALID_REQUEST);
    }

    #[test]
    fn request_round_trips_id_and_params() {
        let r = req(
            "{\"jsonrpc\":\"2.0\",\"id\":\"a1\",\"method\":\"mem.read\",\"params\":{\"addr\":4}}",
        );
        assert_eq!(r.id, json!("a1"));
        assert_eq!(r.method, "mem.read");
        assert_eq!(r.params["addr"], 4);
        let ok = ok_line(&r.id, json!({"data": "00"}));
        let v: serde_json::Value = serde_json::from_str(&ok).unwrap();
        assert_eq!(v["id"], "a1");
        assert_eq!(v["result"]["data"], "00");
    }

    #[test]
    fn auth_gate_flows() {
        // Method before auth is refused without closing.
        let mut gate = AuthGate::new("sesame".into());
        let refused = gate.handle(&req("{\"id\":1,\"method\":\"status\"}"));
        match refused {
            Gate::Reply(line) => {
                let v: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(v["error"]["code"], NOT_AUTHED);
            }
            other => panic!("expected refusal reply, got {other:?}"),
        }

        // Bare hello answers version fields but does not authenticate.
        let hello = gate.handle(&req("{\"id\":2,\"method\":\"hello\"}"));
        match hello {
            Gate::Reply(line) => {
                let v: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(v["result"]["proto"], PROTO_VERSION);
                assert_eq!(v["result"]["authed"], false);
            }
            other => panic!("expected hello reply, got {other:?}"),
        }
        assert!(!gate.authed());

        // Wrong token closes the connection.
        let bad = gate.handle(&req(
            "{\"id\":3,\"method\":\"auth\",\"params\":{\"token\":\"wrong\"}}",
        ));
        assert!(matches!(bad, Gate::ReplyAndClose(_)));

        // Token in hello authenticates in one round trip.
        let mut gate = AuthGate::new("sesame".into());
        let hello = gate.handle(&req(
            "{\"id\":4,\"method\":\"hello\",\"params\":{\"token\":\"sesame\"}}",
        ));
        match hello {
            Gate::Reply(line) => {
                let v: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(v["result"]["authed"], true);
            }
            other => panic!("expected hello reply, got {other:?}"),
        }
        assert!(gate.authed());
        assert_eq!(
            gate.handle(&req("{\"id\":5,\"method\":\"status\"}")),
            Gate::Pass
        );
    }

    #[test]
    fn hex_codec_round_trips() {
        let data: Vec<u8> = (0..=255).collect();
        assert_eq!(decode_hex(&encode_hex(&data)).unwrap(), data);
        assert_eq!(
            decode_hex("DEADbeef").unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
        assert!(decode_hex("abc").is_none());
        assert!(decode_hex("zz").is_none());
    }

    #[test]
    fn base64_codec_round_trips() {
        for len in 0..64usize {
            let data: Vec<u8> = (0..len as u8).map(|b| b.wrapping_mul(37)).collect();
            let enc = encode_base64(&data);
            assert_eq!(decode_base64(&enc).unwrap(), data, "len {len}");
        }
        assert_eq!(encode_base64(b"Amiga"), "QW1pZ2E=");
        assert_eq!(decode_base64("QW1pZ2E=").unwrap(), b"Amiga");
        assert_eq!(decode_base64("QW1pZ2E").unwrap(), b"Amiga");
        assert!(decode_base64("Q").is_none());
        assert!(decode_base64("Q!==").is_none());
        // Nonzero discarded padding bits are malformed, not silently dropped.
        assert!(decode_base64("QX==").is_none());
    }

    #[test]
    fn stop_event_omits_absent_collect() {
        let ev = StopEvent {
            reason: "step".into(),
            detail: String::new(),
            pc: 0xFC0000,
            frame: 2,
            vpos: 44,
            hpos: 101,
            cck: 12345,
            seconds: 0.03,
            retired_instructions: 99,
            collect: None,
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["pc"], 0xFC0000);
        assert!(v.get("collect").is_none());
    }
}
