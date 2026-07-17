// SPDX-License-Identifier: GPL-3.0-or-later

//! copperline-ctl: a thin command-line client for the Copperline Control
//! Protocol (docs/debugger/control.md).
//!
//! One-shot:
//!   copperline-ctl --info /tmp/ccp.json status
//!   copperline-ctl --connect 127.0.0.1:7710 --token HEX \
//!       run_until '{"pc": "0xFC0100"}'
//!
//! REPL (one `METHOD [JSON-PARAMS]` per line on stdin):
//!   copperline-ctl --info /tmp/ccp.json --repl
//!
//! Responses print to stdout as one JSON object per line; server
//! notifications (event.*) print as they arrive. Exit status is nonzero
//! when a one-shot request returns a JSON-RPC error.
//!
//! Deliberately std + serde_json only, so it stays a trivially portable
//! sidecar for scripts and agents.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpStream};
use std::process::ExitCode;

struct Options {
    connect: Option<String>,
    token: Option<String>,
    repl: bool,
    method: Option<String>,
    params: Value,
}

fn usage() -> &'static str {
    "usage: copperline-ctl (--info FILE | --connect ADDR --token TOKEN) \
     [--repl | METHOD [JSON-PARAMS]]"
}

fn parse_options() -> Result<Options, String> {
    let mut connect = None;
    let mut token = None;
    let mut repl = false;
    let mut method = None;
    let mut params = Value::Null;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--connect" => {
                connect = Some(args.next().ok_or("--connect requires ADDR")?);
            }
            "--token" => {
                token = Some(args.next().ok_or("--token requires a token")?);
            }
            "--info" => {
                let path = args.next().ok_or("--info requires a file path")?;
                let body =
                    std::fs::read_to_string(&path).map_err(|e| format!("reading {path}: {e}"))?;
                let info: Value = serde_json::from_str(body.trim())
                    .map_err(|e| format!("parsing {path}: {e}"))?;
                connect = info
                    .get("listen")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .or(connect);
                token = info
                    .get("token")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .or(token);
            }
            "--repl" => repl = true,
            "-h" | "--help" => return Err(usage().to_string()),
            _ if method.is_none() && !arg.starts_with('-') => method = Some(arg),
            _ if method.is_some() && params.is_null() => {
                params =
                    serde_json::from_str(&arg).map_err(|e| format!("params must be JSON: {e}"))?;
            }
            other => return Err(format!("unexpected argument {other:?}\n{}", usage())),
        }
    }
    if connect.is_none() {
        return Err(format!("no server address\n{}", usage()));
    }
    if !repl && method.is_none() {
        return Err(format!("no method\n{}", usage()));
    }
    Ok(Options {
        connect,
        token,
        repl,
        method,
        params,
    })
}

struct Client {
    reader: Option<BufReader<TcpStream>>,
    writer: TcpStream,
    next_id: u64,
}

impl Client {
    fn connect(addr: &str) -> Result<Self, String> {
        let stream = TcpStream::connect(addr).map_err(|e| format!("connecting to {addr}: {e}"))?;
        stream.set_nodelay(true).ok();
        let reader = BufReader::new(
            stream
                .try_clone()
                .map_err(|e| format!("cloning stream: {e}"))?,
        );
        Ok(Self {
            reader: Some(reader),
            writer: stream,
            next_id: 1,
        })
    }

    fn send(&mut self, method: &str, params: &Value) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let line = format!("{msg}\n");
        self.writer
            .write_all(line.as_bytes())
            .and_then(|_| self.writer.flush())
            .map_err(|e| format!("sending request: {e}"))?;
        Ok(id)
    }

    /// Read until the response for `id` arrives; notifications and other
    /// responses are printed as they pass by.
    fn wait_for(&mut self, id: u64) -> Result<Value, String> {
        loop {
            let mut line = String::new();
            let n = self
                .reader
                .as_mut()
                .expect("client reader already moved to REPL")
                .read_line(&mut line)
                .map_err(|e| format!("reading response: {e}"))?;
            if n == 0 {
                return Err("server closed the connection".to_string());
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let msg: Value =
                serde_json::from_str(trimmed).map_err(|e| format!("bad server message: {e}"))?;
            if msg.get("id").and_then(Value::as_u64) == Some(id) {
                return Ok(msg);
            }
            println!("{msg}");
        }
    }

    /// One request-response round trip; returns false when the reply is
    /// a JSON-RPC error.
    fn call(&mut self, method: &str, params: &Value) -> Result<bool, String> {
        let id = self.send(method, params)?;
        let msg = self.wait_for(id)?;
        println!("{msg}");
        Ok(msg.get("error").is_none())
    }

    fn auth(&mut self, token: &str) -> Result<(), String> {
        let id = self.send("hello", &json!({"token": token}))?;
        let msg = self.wait_for(id)?;
        if msg["result"]["authed"] == Value::Bool(true) {
            Ok(())
        } else {
            Err(format!("auth failed: {msg}"))
        }
    }

    /// Run the interactive client with a dedicated socket reader. Keeping the
    /// reader active while stdin waits at the prompt is what makes subscribed
    /// notifications genuinely streaming rather than merely piggybacking on
    /// the next request/response round trip.
    fn run_repl(mut self) -> Result<(), String> {
        let reader = self
            .reader
            .take()
            .expect("client reader already moved to REPL");
        let (response_tx, response_rx) = std::sync::mpsc::channel();
        let reader_thread = std::thread::Builder::new()
            .name("copperline-ctl-read".into())
            .spawn(move || read_repl_messages(reader, response_tx))
            .map_err(|e| format!("starting response reader: {e}"))?;

        let mut result = Ok(());
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let line = match line {
                Ok(line) => line,
                Err(e) => {
                    result = Err(format!("reading command: {e}"));
                    break;
                }
            };
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (method, rest) = match line.split_once(char::is_whitespace) {
                Some((method, rest)) => (method, rest.trim()),
                None => (line, ""),
            };
            let params: Value = if rest.is_empty() {
                Value::Null
            } else {
                match serde_json::from_str(rest) {
                    Ok(params) => params,
                    Err(e) => {
                        eprintln!("params must be JSON: {e}");
                        continue;
                    }
                }
            };
            let id = match self.send(method, &params) {
                Ok(id) => id,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            };
            loop {
                match response_rx.recv() {
                    Ok(Ok(response_id)) if response_id == id => break,
                    Ok(Ok(_)) => {} // no pipelining today, but tolerate it
                    Ok(Err(e)) => {
                        result = Err(e);
                        break;
                    }
                    Err(_) => {
                        result = Err("server response reader stopped".to_string());
                        break;
                    }
                }
            }
            if result.is_err() {
                break;
            }
        }

        let _ = self.writer.shutdown(Shutdown::Both);
        if reader_thread.join().is_err() && result.is_ok() {
            result = Err("server response reader panicked".to_string());
        }
        result
    }
}

fn read_repl_messages(
    mut reader: impl BufRead,
    response_tx: std::sync::mpsc::Sender<Result<u64, String>>,
) {
    loop {
        let mut line = String::new();
        let message = match reader.read_line(&mut line) {
            Ok(0) => Err("server closed the connection".to_string()),
            Ok(_) => serde_json::from_str::<Value>(line.trim())
                .map_err(|e| format!("bad server message: {e}")),
            Err(e) => Err(format!("reading response: {e}")),
        };
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                let _ = response_tx.send(Err(error));
                return;
            }
        };
        println!("{message}");
        if let Some(id) = message.get("id").and_then(Value::as_u64) {
            if response_tx.send(Ok(id)).is_err() {
                return;
            }
        }
    }
}

fn main() -> ExitCode {
    let options = match parse_options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let addr = options.connect.as_deref().expect("checked in parsing");
    let mut client = match Client::connect(addr) {
        Ok(client) => client,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(token) = &options.token {
        if let Err(message) = client.auth(token) {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    }
    if options.repl {
        if let Err(message) = client.run_repl() {
            eprintln!("{message}");
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    } else {
        let method = options.method.as_deref().expect("checked in parsing");
        match client.call(method, &options.params) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(message) => {
                eprintln!("{message}");
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_lines_are_json_rpc_shaped() {
        // The request builder is exercised through send() over a real
        // socket in the server's e2e tests; here just pin the envelope.
        let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "status", "params": Value::Null});
        let line = msg.to_string();
        assert!(line.contains("\"jsonrpc\":\"2.0\""));
        assert!(line.contains("\"method\":\"status\""));
    }

    #[test]
    fn repl_reader_prints_notifications_and_reports_only_response_ids() {
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"method\":\"event.frame\",\"params\":{}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{}}\n",
        );
        let (tx, rx) = std::sync::mpsc::channel();
        read_repl_messages(std::io::Cursor::new(input), tx);
        assert_eq!(rx.recv().unwrap(), Ok(7));
        assert_eq!(
            rx.recv().unwrap(),
            Err("server closed the connection".to_string())
        );
    }
}
