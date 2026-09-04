// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end test of `copperline-ctl --dap`: the built ctl binary is
//! driven over its stdio like an IDE would, and it in turn launches the
//! built emulator headless with the bundled AROS ROM to run the
//! `guest/dap-test/hello` probe, so no local assets are needed. Ignored
//! like the rest of this directory because it spawns two processes and
//! boots the emulator; run it with
//!
//! ```sh
//! cargo test --release --test dap_stdio -- --ignored
//! ```
//! Set `COPPERLINE_BLESS_DAP_PROFILE=1` to replace the deterministic CPU
//! profile golden after reviewing an intentional profiler-format change.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

fn canonical_profile(profile: &Value) -> Value {
    let mut weights = std::collections::BTreeMap::<u64, u64>::new();
    for (sample, delta) in profile["samples"]
        .as_array()
        .unwrap()
        .iter()
        .zip(profile["timeDeltas"].as_array().unwrap())
    {
        *weights.entry(sample.as_u64().unwrap()).or_default() += delta.as_u64().unwrap();
    }
    let nodes: Vec<Value> = profile["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| {
            json!({
                "id": node["id"],
                "callFrame": node["callFrame"],
                "children": node["children"],
            })
        })
        .collect();
    json!({
        "nodes": nodes,
        "startTime": profile["startTime"],
        "endTime": profile["endTime"],
        "samples": weights.keys().collect::<Vec<_>>(),
        "timeDeltas": weights.values().collect::<Vec<_>>(),
        "$copperline": {
            "version": profile["$copperline"]["version"],
            "clockUnit": profile["$copperline"]["clockUnit"],
            "clockHz": profile["$copperline"]["clockHz"],
            "frameCount": profile["$copperline"]["frames"].as_array().unwrap().len(),
            "contentionNode": profile["$copperline"]["contentionNode"],
        },
    })
}

struct Client {
    stdin: std::process::ChildStdin,
    rx: Receiver<Value>,
    seq: i64,
    /// Messages received while waiting for something else, kept for
    /// the waits that come after (events precede responses at times).
    backlog: std::cell::RefCell<Vec<Value>>,
}

impl Client {
    fn send(&mut self, command: &str, arguments: Value) -> i64 {
        self.seq += 1;
        let body = json!({
            "seq": self.seq,
            "type": "request",
            "command": command,
            "arguments": arguments,
        })
        .to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.stdin.flush().unwrap();
        self.seq
    }

    /// The first message `pred` accepts: from the backlog, else from the
    /// adapter within `timeout`; the others wait in the backlog.
    fn wait(&self, what: &str, pred: impl Fn(&Value) -> bool, timeout: Duration) -> Value {
        {
            let mut backlog = self.backlog.borrow_mut();
            if let Some(i) = backlog.iter().position(&pred) {
                return backlog.remove(i);
            }
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let msg = self
                .rx
                .recv_timeout(remaining)
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
            if pred(&msg) {
                return msg;
            }
            self.backlog.borrow_mut().push(msg);
        }
    }

    fn call(&mut self, command: &str, arguments: Value) -> Value {
        let seq = self.send(command, arguments);
        let reply = self.wait(
            command,
            |m| m["type"] == "response" && m["request_seq"] == seq,
            Duration::from_secs(120),
        );
        assert_eq!(reply["success"], true, "{command}: {reply}");
        reply["body"].clone()
    }

    fn event(&self, name: &str) -> Value {
        self.wait(
            name,
            |m| m["type"] == "event" && m["event"] == name,
            Duration::from_secs(180),
        )["body"]
            .clone()
    }

    fn stopped(&self) -> Value {
        self.event("stopped")
    }
}

fn read_messages(
    mut stdout: BufReader<std::process::ChildStdout>,
    tx: std::sync::mpsc::Sender<Value>,
) {
    loop {
        let mut length: Option<usize> = None;
        loop {
            let mut line = String::new();
            if stdout.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                length = value.trim().parse().ok();
            }
        }
        let Some(length) = length else { return };
        let mut body = vec![0u8; length];
        if stdout.read_exact(&mut body).is_err() {
            return;
        }
        let msg: Value = serde_json::from_slice(&body).expect("adapter message is JSON");
        eprintln!("<- {}", serde_json::to_string(&msg).unwrap());
        if tx.send(msg).is_err() {
            return;
        }
    }
}

#[test]
#[ignore]
fn dap_over_stdio_debugs_the_hello_probe_by_source_line() {
    let ctl = env!("CARGO_BIN_EXE_copperline-ctl");
    let emulator = env!("CARGO_BIN_EXE_copperline");
    let fixture_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("guest/dap-test");
    let program = fixture_dir.join("hello");
    let source = fixture_dir.join("hello.c");
    let fixture_debug =
        copperline::debuginfo::DebugInfo::load(&std::fs::read(&program).unwrap(), None)
            .expect("loading fixture debug information");
    let recorded_dir = fixture_debug
        .files
        .iter()
        .find(|file| file.path.ends_with("/guest/dap-test/hello.c"))
        .and_then(|file| std::path::Path::new(&file.path).parent())
        .and_then(std::path::Path::to_str)
        .expect("fixture DWARF source directory");
    let mut source_map = serde_json::Map::new();
    source_map.insert(recorded_dir.into(), Value::from("guest/dap-test"));
    let source_text = std::fs::read_to_string(&source).unwrap();
    let line_of = |needle: &str| {
        source_text
            .lines()
            .position(|l| l.contains(needle))
            .map(|i| i as u64 + 1)
            .unwrap_or_else(|| panic!("no line containing {needle:?}"))
    };
    let counter_line = line_of("counter = counter + sum");
    let call_line = line_of("LONG r = add(n, 0);");

    let mut child = Command::new(ctl)
        .arg("--dap")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("starting copperline-ctl --dap");
    let stdin = child.stdin.take().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let (tx, rx) = channel();
    std::thread::spawn(move || read_messages(stdout, tx));
    let mut c = Client {
        stdin,
        rx,
        seq: 0,
        backlog: std::cell::RefCell::new(Vec::new()),
    };

    let caps = c.call(
        "initialize",
        json!({"clientID": "test", "linesStartAt1": true, "columnsStartAt1": true}),
    );
    assert_eq!(caps["supportsStepBack"], true);
    assert_eq!(caps["supportsDisassembleRequest"], true);

    c.call(
        "launch",
        json!({
            "program": program.display().to_string(),
            "copperline": emulator,
            "factory": true,
            "headless": true,
            "stopOnEntry": true,
            "entryPoint": "entry",
            "sourceMap": source_map,
            "timeoutMs": 120000,
        }),
    );
    c.event("initialized");

    // Breakpoints set before the program is loaded bind at the load.
    let bps = c.call(
        "setBreakpoints",
        json!({"source": {"path": source.display().to_string()}, "breakpoints": [{"line": counter_line}]}),
    );
    assert_eq!(bps["breakpoints"][0]["verified"], false, "{bps}");
    let counter_bp = bps["breakpoints"][0]["id"].as_i64().unwrap();
    let fbps = c.call(
        "setFunctionBreakpoints",
        json!({"breakpoints": [{"name": "scale"}]}),
    );
    let scale_bp = fbps["breakpoints"][0]["id"].as_i64().unwrap();
    c.call("configurationDone", json!({}));

    // The load: the module announces itself and the breakpoints bind.
    let module = c.event("module");
    assert_eq!(module["module"]["name"], "hello");
    let bound = c.wait(
        "breakpoint event",
        |m| {
            m["type"] == "event"
                && m["event"] == "breakpoint"
                && m["body"]["breakpoint"]["id"] == counter_bp
        },
        Duration::from_secs(30),
    );
    assert_eq!(bound["body"]["breakpoint"]["verified"], true, "{bound}");

    let stop = c.stopped();
    assert_eq!(stop["reason"], "entry", "{stop}");
    let frames = c.call("stackTrace", json!({"threadId": 1}));
    let top = &frames["stackFrames"][0];
    assert_eq!(top["name"], "entry", "{frames}");
    assert!(
        top["source"]["path"].as_str().unwrap().ends_with("hello.c"),
        "{top}"
    );
    let entry_line = top["line"].as_u64().unwrap();
    assert!(entry_line > 40, "{top}");
    let rom_frame = frames["stackFrames"]
        .as_array()
        .unwrap()
        .iter()
        .find(|frame| frame["presentationHint"] == "subtle")
        .unwrap_or_else(|| panic!("no ROM frame in {frames}"));
    assert!(
        rom_frame["name"]
            .as_str()
            .is_some_and(|name| name.starts_with('[')),
        "{rom_frame}"
    );
    let rom_dis = c.call(
        "disassemble",
        json!({
            "memoryReference": rom_frame["instructionPointerReference"],
            "instructionCount": 1,
        }),
    );
    assert!(
        rom_dis["instructions"][0]["symbol"]
            .as_str()
            .is_some_and(|name| name.starts_with('[')),
        "{rom_dis}"
    );

    // Run to the function breakpoint: scale(1).
    c.call("continue", json!({"threadId": 1}));
    let stop = c.stopped();
    assert_eq!(stop["reason"], "breakpoint", "{stop}");
    assert!(
        stop["hitBreakpointIds"]
            .as_array()
            .unwrap()
            .contains(&json!(scale_bp)),
        "{stop}"
    );
    let frames = c.call("stackTrace", json!({"threadId": 1}));
    let names: Vec<&str> = frames["stackFrames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(names[0], "scale", "{names:?}");
    assert!(names.contains(&"entry"), "call stack {names:?}");
    let frame_id = frames["stackFrames"][0]["id"].clone();
    let scopes = c.call("scopes", json!({"frameId": frame_id}));
    let scope_names: Vec<&str> = scopes["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        scope_names,
        vec!["Registers", "Locals", "Globals", "Chipset"]
    );
    let locals_ref = scopes["scopes"][1]["variablesReference"].clone();
    let locals = c.call("variables", json!({"variablesReference": locals_ref}));
    let n = locals["variables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "n")
        .unwrap_or_else(|| panic!("no parameter n in {locals}"));
    assert_eq!(n["value"], "1", "{n}");

    // Step over the call line, then into add(): the line changes each time.
    let before = c.call("stackTrace", json!({"threadId": 1}))["stackFrames"][0]["line"].clone();
    c.call("next", json!({"threadId": 1}));
    let stop = c.stopped();
    assert_eq!(stop["reason"], "step");
    let after = c.call("stackTrace", json!({"threadId": 1}))["stackFrames"][0]["line"].clone();
    assert_ne!(before, after);
    c.call("stepIn", json!({"threadId": 1}));
    c.stopped();
    let frames = c.call("stackTrace", json!({"threadId": 1}));
    assert_eq!(frames["stackFrames"][0]["name"], "add", "{frames}");
    assert_eq!(frames["stackFrames"][1]["name"], "scale", "{frames}");

    // Hover on a global and a parameter, and the raw control-protocol escape.
    let frame_id = frames["stackFrames"][0]["id"].clone();
    let counter = c.call(
        "evaluate",
        json!({"expression": "counter", "frameId": frame_id, "context": "hover"}),
    );
    assert_eq!(counter["result"], "5", "{counter}");
    let a = c.call(
        "evaluate",
        json!({"expression": "a", "frameId": frame_id, "context": "hover"}),
    );
    assert_eq!(a["result"], "1", "{a}");
    let status = c.call(
        "evaluate",
        json!({"expression": "!status", "context": "repl"}),
    );
    assert!(
        status["result"].as_str().unwrap().contains("\"paused\""),
        "{status}"
    );

    // The source breakpoint inside add(), then the counter changed.
    c.call("continue", json!({"threadId": 1}));
    let stop = c.stopped();
    assert!(
        stop["hitBreakpointIds"]
            .as_array()
            .unwrap()
            .contains(&json!(counter_bp)),
        "{stop}"
    );
    c.call("next", json!({"threadId": 1}));
    c.stopped();
    let counter = c.call(
        "evaluate",
        json!({"expression": "counter", "context": "watch"}),
    );
    assert_eq!(counter["result"], "6", "{counter}");

    // Disassembly around the PC, with source lines attached.
    let frames = c.call("stackTrace", json!({"threadId": 1}));
    let pc = frames["stackFrames"][0]["instructionPointerReference"].clone();
    let dis = c.call(
        "disassemble",
        json!({"memoryReference": pc, "instructionOffset": -3, "instructionCount": 8}),
    );
    let instructions = dis["instructions"].as_array().unwrap();
    assert_eq!(instructions.len(), 8, "{dis}");
    assert_eq!(instructions[3]["address"], pc, "{dis}");
    assert!(
        instructions.iter().any(|i| i.get("line").is_some()),
        "{dis}"
    );

    // Step back one line (to the breakpoint's line), then out to the
    // call site in scale().
    c.call("stepBack", json!({"threadId": 1}));
    let stop = c.stopped();
    assert_eq!(stop["reason"], "step");
    let frames = c.call("stackTrace", json!({"threadId": 1}));
    assert_eq!(frames["stackFrames"][0]["name"], "add", "{frames}");
    assert_eq!(frames["stackFrames"][0]["line"], counter_line, "{frames}");
    c.call("stepOut", json!({"threadId": 1}));
    c.stopped();
    let frames = c.call("stackTrace", json!({"threadId": 1}));
    assert_eq!(frames["stackFrames"][0]["name"], "scale", "{frames}");
    assert_eq!(frames["stackFrames"][0]["line"], call_line, "{frames}");

    // Clear the breakpoints and let the program finish: its greeting
    // reaches the serial port, which the adapter shows as stdout.
    c.call(
        "setBreakpoints",
        json!({"source": {"path": source.display().to_string()}, "breakpoints": []}),
    );
    c.call("setFunctionBreakpoints", json!({"breakpoints": []}));
    let profile = c.call("copperline/profile", json!({"frames": 1}));
    assert_eq!(profile["framesCaptured"], 1, "{profile}");
    assert!(profile["samples"].as_u64().unwrap() > 0, "{profile}");
    let stop = c.stopped();
    assert_eq!(stop["reason"], "step", "{stop}");
    let profile_path = std::path::PathBuf::from(profile["path"].as_str().unwrap());
    let cpu_profile: Value =
        serde_json::from_slice(&std::fs::read(&profile_path).unwrap()).unwrap();
    assert_eq!(
        cpu_profile["$copperline"]["frames"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        cpu_profile["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["callFrame"]["functionName"] == "[Bus wait]"),
        "{cpu_profile}"
    );
    let canonical = canonical_profile(&cpu_profile);
    if std::env::var_os("COPPERLINE_BLESS_DAP_PROFILE").is_some() {
        let golden_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden/hello-one-frame.cpuprofile");
        std::fs::write(&golden_path, serde_json::to_vec(&canonical).unwrap()).unwrap();
        eprintln!("blessed {}", golden_path.display());
    } else {
        let golden: Value =
            serde_json::from_str(include_str!("golden/hello-one-frame.cpuprofile")).unwrap();
        assert_eq!(canonical, golden);
    }
    c.call("continue", json!({"threadId": 1}));
    c.wait(
        "the guest's greeting",
        |m| {
            m["type"] == "event"
                && m["event"] == "output"
                && m["body"]["category"] == "stdout"
                && m["body"]["output"]
                    .as_str()
                    .is_some_and(|s| s.contains("hello from the guest"))
        },
        Duration::from_secs(120),
    );
    c.call("pause", json!({"threadId": 1}));
    c.stopped();

    c.call("disconnect", json!({"terminateDebuggee": true}));
    drop(c);
    let status = child.wait().expect("adapter exits");
    assert!(status.success(), "{status}");
}
