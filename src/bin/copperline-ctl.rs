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
//! MCP server over stdio (docs/debugger/control.md, "MCP server"), for
//! coding agents; with no connection arguments the agent launches or
//! attaches a session through the session_* tools:
//!   copperline-ctl --mcp [--info FILE | --connect ADDR --token TOKEN]
//!
//! Debug Adapter Protocol server (docs/debugger/dap.md) for VS Code,
//! nvim-dap and other DAP clients, on stdio or a TCP listener; with a
//! connection given, the client's launch or attach uses that session:
//!   copperline-ctl --dap [--info FILE | --connect ADDR --token TOKEN]
//!   copperline-ctl --dap-listen ADDR
//!
//! Responses print to stdout as one JSON object per line; server
//! notifications (event.*) print as they arrive. Exit status is nonzero
//! when a one-shot request returns a JSON-RPC error.
//!
//! Deliberately std + serde_json only (the MCP and DAP modes are the
//! library's hand-rolled `control::mcp` and `control::dap`, no async
//! runtime), so it stays a trivially portable sidecar for scripts,
//! agents and editors.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpStream};
use std::process::ExitCode;

struct Options {
    connect: Option<String>,
    token: Option<String>,
    repl: bool,
    mcp: bool,
    dap: bool,
    dap_listen: Option<String>,
    method: Option<String>,
    params: Value,
}

fn usage() -> &'static str {
    "usage: copperline-ctl (--info FILE | --connect ADDR --token TOKEN) \
     [--repl | METHOD [JSON-PARAMS]]\n       \
     copperline-ctl --mcp [--info FILE | --connect ADDR --token TOKEN]\n       \
     copperline-ctl --dap [--info FILE | --connect ADDR --token TOKEN]\n       \
     copperline-ctl --dap-listen ADDR [--info FILE | --connect ADDR --token TOKEN]\n       \
     copperline-ctl profile STATE [--rom ROM] --out PATH [--frames N] [--format native|bartman]\n       \
     copperline-ctl profile-report DIR --program PROG [--elf PROG.ELF] \
       --out FILE [--format chrome|bartman] [--per-frame] \
       [--source-map FROM=TO ...]\n       \
     copperline-ctl exe2adf PROG [--boot] [--out FILE]\n\
     copperline-ctl size-report PROG [--elf PROG.ELF] [--out FILE]"
}

fn parse_options() -> Result<Options, String> {
    let mut connect = None;
    let mut token = None;
    let mut repl = false;
    let mut mcp = false;
    let mut dap = false;
    let mut dap_listen = None;
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
            "--mcp" => mcp = true,
            "--dap" => dap = true,
            "--dap-listen" => {
                dap_listen = Some(args.next().ok_or("--dap-listen requires ADDR")?);
            }
            "-h" | "--help" => return Err(usage().to_string()),
            _ if method.is_none() && !arg.starts_with('-') => method = Some(arg),
            _ if method.is_some() && params.is_null() => {
                params =
                    serde_json::from_str(&arg).map_err(|e| format!("params must be JSON: {e}"))?;
            }
            other => return Err(format!("unexpected argument {other:?}\n{}", usage())),
        }
    }
    let dap = dap || dap_listen.is_some();
    if mcp && dap {
        return Err(format!("--mcp and --dap are exclusive\n{}", usage()));
    }
    if mcp || dap {
        let mode = if mcp { "--mcp" } else { "--dap" };
        if repl || method.is_some() {
            return Err(format!("{mode} takes no method or --repl\n{}", usage()));
        }
        if connect.is_some() && token.is_none() {
            return Err(format!("{mode} --connect needs --token\n{}", usage()));
        }
    } else {
        if connect.is_none() {
            return Err(format!("no server address\n{}", usage()));
        }
        if !repl && method.is_none() {
            return Err(format!("no method\n{}", usage()));
        }
    }
    Ok(Options {
        connect,
        token,
        repl,
        mcp,
        dap,
        dap_listen,
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

/// `--mcp`: serve MCP on stdin/stdout, attached to the session named on
/// the command line if there is one.
fn run_mcp(options: &Options) -> ExitCode {
    use copperline::control::bridge::Bridge;
    let attach = match (&options.connect, &options.token) {
        (Some(addr), Some(token)) => match Bridge::connect(addr, token) {
            Ok(bridge) => Some(bridge),
            Err(message) => {
                eprintln!("copperline-ctl: {message}");
                return ExitCode::FAILURE;
            }
        },
        _ => None,
    };
    match copperline::control::mcp::run_stdio(attach) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("copperline-ctl: mcp transport: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `--dap` / `--dap-listen`: serve the Debug Adapter Protocol, attached
/// to the session named on the command line if there is one.
fn run_dap(options: &Options) -> ExitCode {
    use copperline::control::bridge::Bridge;
    let attach = match (&options.connect, &options.token) {
        (Some(addr), Some(token)) => match Bridge::connect(addr, token) {
            Ok(bridge) => Some(bridge),
            Err(message) => {
                eprintln!("copperline-ctl: {message}");
                return ExitCode::FAILURE;
            }
        },
        _ => None,
    };
    let result = match &options.dap_listen {
        Some(addr) => copperline::control::dap::run_listen(addr, attach),
        None => copperline::control::dap::run_stdio(attach),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("copperline-ctl: dap transport: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_state_profile() -> anyhow::Result<std::path::PathBuf> {
    use anyhow::{anyhow, bail};
    use copperline::profile::{ProfileOptions, ScreenshotMode};
    let mut args = std::env::args().skip(2);
    let input = std::path::PathBuf::from(
        args.next()
            .ok_or_else(|| anyhow!("profile requires a USS or CLSTATE file"))?,
    );
    let mut rom = None;
    let mut out = None;
    let mut frames = 1;
    let mut bartman = false;
    while let Some(arg) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| anyhow!("{arg} requires a value"))?;
        match arg.as_str() {
            "--rom" => rom = Some(std::path::PathBuf::from(value)),
            "--out" => out = Some(std::path::PathBuf::from(value)),
            "--frames" => frames = value.parse::<u64>()?,
            "--format" => match value.as_str() {
                "native" => bartman = false,
                "bartman" => bartman = true,
                _ => bail!("format must be native or bartman"),
            },
            _ => bail!("unexpected profile argument {arg}"),
        }
    }
    if !(1..=100).contains(&frames) {
        bail!("profile requires 1..100 frames");
    }
    let out =
        out.ok_or_else(|| anyhow!("profile requires --out DIR (native) or FILE (bartman)"))?;
    let mut cfg = copperline::config::Config::default();
    let uss = input
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("uss"));
    let state = if uss {
        Some(copperline::uss::UssFile::read(&input)?)
    } else {
        None
    };
    if let Some(state) = &state {
        state.configure(&mut cfg)?;
        cfg.rom_path = rom.ok_or_else(|| anyhow!("USS profiling requires --rom KICKSTART"))?;
    }
    let mut emu = copperline::emulator::build_machine(
        &cfg,
        Box::new(copperline::audio::NullSink),
        false,
        !uss,
    )?;
    if let Some(state) = state {
        for warning in &state.warnings {
            eprintln!("warning: {warning}");
        }
        state.load(&mut emu)?;
    } else {
        emu.load_state(&input)?;
    }
    if bartman {
        copperline::profile::bartman::capture(
            &mut emu,
            &copperline::profile::bartman::Request {
                frames: frames as u32,
                unwind: None,
                out: out.clone(),
            },
            |line| {
                eprint!("{line}");
                Ok(())
            },
        )?;
    } else {
        emu.profile_start(ProfileOptions {
            path: out.clone(),
            frames,
            slots: true,
            memory: true,
            screenshots: ScreenshotMode::Every,
            pc_samples: true,
            samples: true,
            registers: true,
            unwind: None,
            relocation_bases: Vec::new(),
            code_ranges: Vec::new(),
            trigger: None,
        })?;
        while emu.profile_status_value()["frames_written"]
            .as_u64()
            .unwrap_or(0)
            < frames
        {
            emu.step_frame()?;
            if emu.machine.cpu_double_faulted() {
                bail!("CPU double fault during state profile");
            }
        }
        let machine = serde_json::to_value(emu.machine_descriptor())?;
        emu.profile_stop(machine, serde_json::json!([]))?;
    }
    Ok(out)
}

fn run_profile_report() -> Result<Vec<std::path::PathBuf>, String> {
    use copperline::profile::report::{ReportFormat, ReportOptions};

    let mut args = std::env::args().skip(2);
    let input_dir = std::path::PathBuf::from(args.next().ok_or("profile-report requires DIR")?);
    let mut program = None;
    let mut elf = None;
    let mut out = None;
    let mut format = ReportFormat::Chrome;
    let mut per_frame = false;
    let mut source_map = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--program" => {
                program = Some(std::path::PathBuf::from(
                    args.next().ok_or("--program requires a path")?,
                ));
            }
            "--elf" => {
                elf = Some(std::path::PathBuf::from(
                    args.next().ok_or("--elf requires a path")?,
                ));
            }
            "--out" => {
                out = Some(std::path::PathBuf::from(
                    args.next().ok_or("--out requires a path")?,
                ));
            }
            "--format" => {
                let value = args.next().ok_or("--format requires chrome|bartman")?;
                format = ReportFormat::parse(&value)
                    .ok_or_else(|| format!("bad --format {value:?}; expected chrome|bartman"))?;
            }
            "--per-frame" => per_frame = true,
            "--source-map" => {
                let value = args.next().ok_or("--source-map requires FROM=TO")?;
                let (from, to) = value
                    .split_once('=')
                    .ok_or("--source-map requires FROM=TO")?;
                source_map.push((from.to_string(), to.to_string()));
            }
            "-h" | "--help" => return Err(usage().to_string()),
            other => return Err(format!("unexpected profile-report argument {other:?}")),
        }
    }
    copperline::profile::report::generate(&ReportOptions {
        input_dir,
        program: program.ok_or("profile-report requires --program PROG")?,
        elf,
        out: out.ok_or("profile-report requires --out FILE")?,
        format,
        per_frame,
        source_map,
    })
}

fn run_exe2adf() -> Result<std::path::PathBuf, String> {
    use std::path::{Path, PathBuf};

    let mut args = std::env::args().skip(2);
    let program = PathBuf::from(args.next().ok_or("exe2adf requires PROG")?);
    let mut bootable = false;
    let mut out = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--boot" => bootable = true,
            "--out" => out = Some(PathBuf::from(args.next().ok_or("--out requires FILE")?)),
            other => return Err(format!("unexpected exe2adf argument {other:?}")),
        }
    }
    if !program.is_file() {
        return Err(format!(
            "{} is not a regular executable file",
            program.display()
        ));
    }
    let name = program
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("the executable name is not valid UTF-8")?;
    let amiga_name = encode_amiga_name(name)?;
    let out = out.unwrap_or_else(|| program.with_extension("adf"));
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let staging = std::env::temp_dir().join(format!(
        "copperline-exe2adf-{}-{unique}",
        std::process::id()
    ));
    let result = (|| {
        std::fs::create_dir_all(staging.join("S"))
            .map_err(|error| format!("creating staging directory: {error}"))?;
        std::fs::copy(&program, staging.join(name))
            .map_err(|error| format!("copying {}: {error}", program.display()))?;
        let mut startup = Vec::with_capacity(5 + amiga_name.len());
        startup.extend_from_slice(b"SYS:");
        startup.extend_from_slice(&amiga_name);
        startup.push(b'\n');
        std::fs::write(staging.join("S").join("Startup-Sequence"), startup)
            .map_err(|error| format!("writing Startup-Sequence: {error}"))?;
        let image = copperline::dirfs::build_floppy_image(
            &staging,
            "Copperline",
            copperline::diskimage::FileSystem::OFS,
            bootable,
        )
        .map_err(|error| error.to_string())?;
        copperline::diskimage::write_standard_adf(Path::new(&out), &image)
            .map_err(|error| format!("writing {}: {error}", out.display()))?;
        Ok(out.clone())
    })();
    if let Err(error) = std::fs::remove_dir_all(&staging) {
        eprintln!(
            "copperline-ctl: exe2adf: warning: removing {}: {error}",
            staging.display()
        );
    }
    result
}

fn encode_amiga_name(name: &str) -> Result<Vec<u8>, String> {
    let encoded: Vec<u8> = name
        .chars()
        .map(|ch| u8::try_from(ch as u32))
        .collect::<Result<_, _>>()
        .map_err(|_| "the Amiga executable name must use Latin-1 characters")?;
    if encoded.is_empty() || encoded.len() > 30 {
        return Err("the Amiga executable name must be 1..30 Latin-1 characters".into());
    }
    if encoded.iter().any(|byte| matches!(byte, b':' | b'/')) {
        return Err("the Amiga executable name must not contain ':' or '/'".into());
    }
    Ok(encoded)
}

fn run_size_report() -> Result<std::path::PathBuf, String> {
    use std::path::PathBuf;

    let mut args = std::env::args().skip(2);
    let program = PathBuf::from(args.next().ok_or("size-report requires PROG")?);
    let mut elf = None;
    let mut out = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--elf" => elf = Some(PathBuf::from(args.next().ok_or("--elf requires FILE")?)),
            "--out" => out = Some(PathBuf::from(args.next().ok_or("--out requires FILE")?)),
            other => return Err(format!("unexpected size-report argument {other:?}")),
        }
    }
    let out = out.unwrap_or_else(|| {
        let mut name = program.as_os_str().to_os_string();
        name.push(".size.cpuprofile");
        PathBuf::from(name)
    });
    copperline::profile::size::generate(&program, elf.as_deref(), &out)
}

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("size-report") {
        return match run_size_report() {
            Ok(path) => {
                println!("{}", path.display());
                ExitCode::SUCCESS
            }
            Err(message) => {
                eprintln!("copperline-ctl: size-report: {message}");
                ExitCode::from(2)
            }
        };
    }
    if std::env::args().nth(1).as_deref() == Some("exe2adf") {
        return match run_exe2adf() {
            Ok(path) => {
                println!("{}", path.display());
                ExitCode::SUCCESS
            }
            Err(message) => {
                eprintln!("copperline-ctl: exe2adf: {message}");
                ExitCode::from(2)
            }
        };
    }
    if std::env::args().nth(1).as_deref() == Some("profile") {
        return match run_state_profile() {
            Ok(path) => {
                println!("{}", path.display());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("copperline-ctl: profile: {error:#}");
                ExitCode::from(2)
            }
        };
    }
    if std::env::args().nth(1).as_deref() == Some("profile-report") {
        return match run_profile_report() {
            Ok(paths) => {
                for path in paths {
                    println!("{}", path.display());
                }
                ExitCode::SUCCESS
            }
            Err(message) => {
                eprintln!("copperline-ctl: profile-report: {message}");
                ExitCode::FAILURE
            }
        };
    }
    let options = match parse_options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    if options.mcp {
        return run_mcp(&options);
    }
    if options.dap {
        return run_dap(&options);
    }
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
    fn amiga_executable_names_are_single_byte_and_path_safe() {
        assert_eq!(encode_amiga_name("caf\u{e9}").unwrap(), b"caf\xe9");
        assert!(encode_amiga_name("bad:name").is_err());
        assert!(encode_amiga_name("bad/name").is_err());
        assert!(encode_amiga_name("snowman-\u{2603}").is_err());
        assert!(encode_amiga_name("").is_err());
        assert!(encode_amiga_name(&"x".repeat(31)).is_err());
    }

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
