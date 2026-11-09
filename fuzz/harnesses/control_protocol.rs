//! CCP framing, JSON-RPC requests, authentication and payload codecs. No
//! sockets are opened and parsed requests never reach the command executor.

use copperline::control::proto;
use std::io::Cursor;

pub fn exercise(data: &[u8]) {
    let mut reader = Cursor::new(data);
    let mut gate = proto::AuthGate::new("fuzz-token".into());
    while let Ok(Some(line)) = proto::read_msg_line(&mut reader) {
        if let Ok(request) = proto::parse_request(&line) {
            let _ = gate.handle(&request);
        }
        let _ = proto::decode_hex(&line);
        let _ = proto::decode_base64(&line);
    }
    // The original bytes also reach the parser without newline splitting,
    // including truncated JSON and escaped/non-ASCII codec inputs.
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = proto::parse_request(text);
        let _ = proto::decode_hex(text);
        let _ = proto::decode_base64(text);
    }
}
