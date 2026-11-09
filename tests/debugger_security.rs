// SPDX-License-Identifier: GPL-3.0-or-later

//! Asset-free process checks for debugger listener and credential boundaries.
#![cfg(all(feature = "frontend", feature = "ctl-bin", feature = "gdb"))]

use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(20);

struct Process {
    child: Child,
    stderr: Receiver<String>,
    log: String,
}

impl Process {
    fn start(binary: &str, args: &[&str]) -> Self {
        let mut child = Command::new(binary)
            .args(args)
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stderr: rx,
            log: String::new(),
        }
    }

    fn line_containing(&mut self, text: &str) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let line = self
                .stderr
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|e| panic!("waiting for {text}: {e}; {}", self.log));
            self.log.push_str(&line);
            self.log.push('\n');
            if line.contains(text) {
                return line;
            }
        }
    }

    fn wait(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                for line in self.stderr.iter() {
                    self.log.push_str(&line);
                    self.log.push('\n');
                }
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "process did not exit: {}",
                self.log
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn unauthenticated_debugger_processes_reject_remote_listeners() {
    for flag in ["--gdb", "--gdb-gui"] {
        let mut process = Process::start(
            env!("CARGO_BIN_EXE_copperline"),
            &["--factory", "--noaudio", flag, "0.0.0.0:0"],
        );
        assert!(!process.wait().success());
        assert!(
            process.log.contains("--allow-remote-debugging"),
            "{}",
            process.log
        );
    }
    let mut process = Process::start(
        env!("CARGO_BIN_EXE_copperline-ctl"),
        &["--dap-listen", "[::]:0"],
    );
    assert!(!process.wait().success());
    assert!(
        process.log.contains("--allow-remote-debugging"),
        "{}",
        process.log
    );
}

#[test]
fn gdb_remote_opt_in_opens_a_working_transport() {
    let mut process = Process::start(
        env!("CARGO_BIN_EXE_copperline"),
        &[
            "--factory",
            "--noaudio",
            "--gdb",
            "0.0.0.0:0",
            "--allow-remote-debugging",
        ],
    );
    let line = process.line_containing("gdb: listening on ");
    let addr = line
        .split_once("gdb: listening on ")
        .unwrap()
        .1
        .replace("0.0.0.0:", "127.0.0.1:");
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(TIMEOUT)).unwrap();
    stream.write_all(b"$?#3f").unwrap();
    let mut ack = [0];
    stream.read_exact(&mut ack).unwrap();
    assert_eq!(ack, *b"+");
    let mut packet = Vec::new();
    BufReader::new(stream.try_clone().unwrap())
        .read_until(b'#', &mut packet)
        .unwrap();
    assert!(packet.starts_with(b"$T05") || packet.starts_with(b"$S05"));
    stream.write_all(b"+$k#6b").unwrap();
    assert!(process.wait().success(), "{}", process.log);
}

#[test]
fn dap_remote_opt_in_opens_a_working_transport() {
    let mut process = Process::start(
        env!("CARGO_BIN_EXE_copperline-ctl"),
        &["--dap-listen", "0.0.0.0:0", "--allow-remote-debugging"],
    );
    let line = process.line_containing("copperline-dap: listening on ");
    let addr = line
        .strip_prefix("copperline-dap: listening on ")
        .unwrap()
        .replace("0.0.0.0:", "127.0.0.1:");
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(TIMEOUT)).unwrap();
    let body =
        json!({"seq": 1, "type": "request", "command": "initialize", "arguments": {}}).to_string();
    write!(stream, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    stream.flush().unwrap();
    let mut reader = BufReader::new(stream);
    let mut header = String::new();
    reader.read_line(&mut header).unwrap();
    let len: usize = header
        .trim()
        .strip_prefix("Content-Length: ")
        .unwrap()
        .parse()
        .unwrap();
    assert!(len < 65536);
    let mut blank = String::new();
    reader.read_line(&mut blank).unwrap();
    assert_eq!(blank, "\r\n");
    let mut reply = vec![0; len];
    reader.read_exact(&mut reply).unwrap();
    let reply: serde_json::Value = serde_json::from_slice(&reply).unwrap();
    assert_eq!(reply["command"], "initialize");
    assert_eq!(reply["success"], true);
}

#[test]
fn control_tokens_are_handed_off_without_appearing_in_process_logs() {
    for pinned in [None, Some("private-token-with-\"quotes")] {
        let mut args = vec!["--factory", "--noaudio", "--control", ":0"];
        if let Some(token) = pinned {
            args.extend(["--control-token", token]);
        }
        let mut process = Process::start(env!("CARGO_BIN_EXE_copperline"), &args);
        let line = process.line_containing("copperline-control: listen=");
        assert!(!line.contains("token="));
        let info = line
            .split_once(" info=")
            .unwrap()
            .1
            .rsplit_once(" proto=")
            .unwrap()
            .0;
        let path = std::path::Path::new(info);
        let body: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let token = body["token"].as_str().unwrap();
        if let Some(pinned) = pinned {
            assert_eq!(token, pinned);
        } else {
            assert_eq!(token.len(), 32);
        }
        for method in ["status", "shutdown"] {
            let mut client = Process::start(
                env!("CARGO_BIN_EXE_copperline-ctl"),
                &["--info", info, method],
            );
            assert!(client.wait().success(), "{}", client.log);
            assert!(
                !client.log.contains(token),
                "credential appeared in client stderr"
            );
        }
        assert!(process.wait().success(), "{}", process.log);
        assert!(
            !process.log.contains(token),
            "credential appeared in stderr"
        );
        assert!(!path.exists(), "automatic handoff file must be cleaned up");
    }
}
