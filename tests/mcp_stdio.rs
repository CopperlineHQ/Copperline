// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end smoke test of `copperline-ctl --mcp`: the built ctl binary
//! is driven over its stdio like an MCP client would, and it in turn
//! launches the built emulator with the bundled AROS ROM, so no local
//! assets are needed. Ignored like the rest of this directory because it
//! spawns two processes and runs the emulator; run it with
//!
//! ```sh
//! cargo test --release --test mcp_stdio -- --ignored
//! ```

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

fn request(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

/// The JSON document in a tool result's first text block.
fn text_json(reply: &Value) -> Value {
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text block in {reply}"));
    serde_json::from_str(text).expect("text block is JSON")
}

#[test]
#[ignore]
fn mcp_over_stdio_launches_runs_and_closes_an_emulator() {
    let ctl = env!("CARGO_BIN_EXE_copperline-ctl");
    let emulator = env!("CARGO_BIN_EXE_copperline");
    let mut child = Command::new(ctl)
        .arg("--mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("starting copperline-ctl --mcp");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let mut exchange = |line: String| -> Value {
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
        let mut reply = String::new();
        assert!(
            stdout.read_line(&mut reply).unwrap() > 0,
            "server closed stdout"
        );
        serde_json::from_str(reply.trim()).expect("reply is JSON")
    };

    let init = exchange(request(
        1,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    ));
    assert_eq!(init["result"]["serverInfo"]["name"], "copperline");
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");

    let list = exchange(request(2, "tools/list", json!({})));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"session_launch"));
    assert!(names.contains(&"run_until"));
    assert!(names.contains(&"capture_screenshot"));

    let launch = exchange(request(
        3,
        "tools/call",
        json!({"name": "session_launch", "arguments": {"factory": true, "binary": emulator, "model": "A500", "timeout_ms": 60000}}),
    ));
    assert_eq!(launch["result"]["isError"], false, "{launch}");
    let doc = text_json(&launch);
    assert_eq!(doc["launched"], true);
    let pid = doc["pid"].as_u64().expect("pid");
    assert_eq!(doc["status"]["state"], "paused");

    let stop = exchange(request(
        4,
        "tools/call",
        json!({"name": "run_until", "arguments": {"frame": 5, "wait_ms": 60000}}),
    ));
    assert_eq!(stop["result"]["isError"], false, "{stop}");
    let stop = text_json(&stop);
    assert_eq!(stop["reason"], "target");
    assert_eq!(stop["frame"], 5);

    let shot = exchange(request(
        5,
        "tools/call",
        json!({"name": "capture_screenshot"}),
    ));
    let blocks = shot["result"]["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 2, "{shot}");
    assert_eq!(blocks[1]["type"], "image");
    assert_eq!(blocks[1]["mimeType"], "image/png");

    let closed = exchange(request(6, "tools/call", json!({"name": "session_close"})));
    let doc = text_json(&closed);
    assert_eq!(doc["closed"], true);
    assert_eq!(doc["terminated_pid"], pid);

    drop(stdin);
    let status = child.wait().expect("ctl exits on EOF");
    assert!(status.success(), "ctl exit status {status}");
    // The launched emulator is gone with it.
    #[cfg(unix)]
    {
        std::thread::sleep(Duration::from_millis(200));
        let alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(!alive, "emulator pid {pid} outlived the session");
    }
}
