// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end host <-> guest clipboard sharing through the real services
//! ROM under the bundled AROS ROM. The one local asset is a
//! `clipboard.device` for the guest (the bundled AROS ROM has none, and
//! AROS's own is disk-based like Kickstart's): `COPPERLINE_CLIPBOARD_DEVICE`
//! names the file, or it is looked up as `clipboard.device` /
//! `Devs/clipboard.device` in the test-asset directory; the test skips
//! without one. It runs the emulator for a minute or so, hence `#[ignore]`.
//!
//! The guest probe (`guest/clipboard-test/cliptest`, run through `--run`)
//! installs that device into `DEVS:`, posts an FTXT clip, which the ROM's
//! bridge must push to the host; the test reads it back over the control
//! protocol, stages a reply with `clipboard.set`, and the probe -- polling
//! `clipboard.device` for a clip that is not its own -- writes what it
//! received into `clip-out.txt` in its (host) directory.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn asset_dir() -> PathBuf {
    std::env::var_os("COPPERLINE_TEST_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("test-assets"))
}

/// The guest `clipboard.device` to stage, if one is available locally.
fn clipboard_device() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("COPPERLINE_CLIPBOARD_DEVICE") {
        return Some(PathBuf::from(explicit));
    }
    ["clipboard.device", "Devs/clipboard.device"]
        .iter()
        .map(|name| asset_dir().join(name))
        .find(|p| p.is_file())
}

/// A minimal newline-delimited JSON-RPC client for the headless server.
struct Ctl {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    next_id: u64,
}

impl Ctl {
    fn connect(info: &serde_json::Value) -> Result<Self, Box<dyn std::error::Error>> {
        let listen = info["listen"].as_str().ok_or("no listen address")?;
        let stream = TcpStream::connect(listen)?;
        stream.set_read_timeout(Some(Duration::from_secs(300)))?;
        let mut ctl = Self {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
            next_id: 1,
        };
        let token = info["token"].as_str().ok_or("no token")?;
        ctl.call("auth", serde_json::json!({"token": token}))?;
        Ok(ctl)
    }

    fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let id = self.next_id;
        self.next_id += 1;
        let req =
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.writer, "{req}")?;
        // Skip notifications (no id) until our reply arrives.
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 {
                return Err("server closed the connection".into());
            }
            let v: serde_json::Value = serde_json::from_str(&line)?;
            if v["id"] == serde_json::json!(id) {
                if let Some(err) = v.get("error") {
                    return Err(format!("{method}: {err}").into());
                }
                return Ok(v["result"].clone());
            }
        }
    }
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "runs the emulator for about a minute and needs a clipboard.device"]
fn clipboard_text_crosses_both_ways_under_aros() -> Result<(), Box<dyn std::error::Error>> {
    let Some(device) = clipboard_device() else {
        eprintln!(
            "skipping clipboard bridge; no clipboard.device (COPPERLINE_CLIPBOARD_DEVICE or {})",
            asset_dir().join("clipboard.device").display()
        );
        return Ok(());
    };
    let temp = std::env::temp_dir().join(format!(
        "copperline-clipboard-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&temp)?;
    // The probe runs from its own directory (RunProg:), which is where it
    // writes clip-out.txt; a copy keeps the checked-in tree clean.
    let probe = temp.join("cliptest");
    std::fs::copy(repo_root().join("guest/clipboard-test/cliptest"), &probe)?;
    std::fs::copy(&device, temp.join("clipboard.device"))?;
    let info_path = temp.join("ccp.json");

    let child = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .env("RUST_LOG", "copperline=info")
        .arg("--factory")
        .arg("--noaudio")
        .arg("--clipboard")
        .arg("--run")
        .arg(&probe)
        .arg("--control")
        .arg(":0")
        .arg("--control-info")
        .arg(&info_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut child = KillOnDrop(child);
    let stderr = child.0.stderr.take().unwrap();
    let log = std::thread::spawn(move || {
        let mut out = String::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            out.push_str(&line);
            out.push('\n');
        }
        out
    });

    let deadline = Instant::now() + Duration::from_secs(30);
    let info = loop {
        if let Ok(text) = std::fs::read_to_string(&info_path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                break v;
            }
        }
        if Instant::now() > deadline {
            drop(child);
            return Err(format!("no control-info file; log:\n{}", log.join().unwrap()).into());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut ctl = Ctl::connect(&info)?;

    // Boot until the guest loads the probe (the --run loadseg stop).
    let stop = ctl.call("run_until", serde_json::json!({"seconds": 180}))?;
    assert_eq!(
        stop["reason"], "loadseg",
        "expected the --run loadseg stop, got {stop}"
    );

    // Guest -> host: the probe's clip arrives through the ROM bridge.
    let mut got = None;
    for _ in 0..60 {
        ctl.call("step_frame", serde_json::json!({"n": 50}))?;
        let clip = ctl.call("clipboard.get", serde_json::json!({}))?;
        assert_eq!(clip["fitted"], true);
        assert_eq!(clip["sharing"], true);
        if let Some(text) = clip["text"].as_str() {
            got = Some(text.to_string());
            break;
        }
    }
    assert_eq!(
        got.as_deref(),
        Some("Hello from the guest"),
        "guest clip never reached the host"
    );
    let clip = ctl.call("clipboard.get", serde_json::json!({}))?;
    assert_eq!(clip["guest_ready"], true);

    // Host -> guest: stage a reply and let the probe pick it up.
    let set = ctl.call(
        "clipboard.set",
        serde_json::json!({"text": "Hello from the host\r\nline two"}),
    )?;
    assert_eq!(set["staged"], true);
    let out_path = temp.join("clip-out.txt");
    let mut received = None;
    for _ in 0..60 {
        ctl.call("step_frame", serde_json::json!({"n": 50}))?;
        if let Ok(text) = std::fs::read_to_string(&out_path) {
            received = Some(text);
            break;
        }
    }
    assert_eq!(
        received.as_deref(),
        Some("Hello from the host\nline two"),
        "guest never received the host text (CRLF folded to LF)"
    );
    let clip = ctl.call("clipboard.get", serde_json::json!({}))?;
    assert_eq!(
        clip["guest_gen"], clip["host_gen"],
        "the guest acknowledged the generation"
    );

    drop(child);
    let _ = log.join();
    let _ = std::fs::remove_dir_all(&temp);
    Ok(())
}
