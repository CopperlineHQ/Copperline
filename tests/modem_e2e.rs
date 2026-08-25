//! Whole-machine end-to-end check for the Hayes/WiModem232 modem
//! personality (`[serial] mode = "modem"`, `src/modem/`).
//!
//! `src/modem`'s own unit suite exhaustively covers the AT state machine
//! against a fake transport; what it cannot cover is the hardware path
//! between it and a real guest: Paula's SERPER-paced UART, the CIA-B
//! control-line overlay, and a real `serial.device` (ROM/driver code, not
//! Copperline's own) actually driving them. This test closes that gap the
//! same way `tests/image_regression.rs`'s `hostfs_boot_*` tests do for the
//! hostfs board: boot from a `[[filesys]]` mount holding a committed guest
//! probe (`guest/modem-test/modemtest`), let a real Startup-Sequence run
//! it, and inspect what it wrote back to the host side afterwards.
//!
//! Unlike those hostfs tests, this one cannot use the bundled AROS ROM:
//! `serial.device` is not ROM-resident on real AmigaOS either (Kickstart
//! 2.0+ loads it from `DEVS:serial.device` on demand, via `LoadSeg`, the
//! first time something opens it -- confirmed empirically while writing
//! this test: a bare hostfs boot volume with nothing but the probe gets
//! `OpenDevice` `IOERR_OPENFAIL`, and copying a real `Devs/serial.device`
//! onto it is what makes the open succeed), and the bundled AROS build
//! Copperline ships carries no `serial.device` at all -- neither ROM tag
//! nor disk-loadable module (`strings` on both ROM images turns up no
//! occurrence of the string). So, like `tests/atapi_cd.rs`'s real
//! `cdfs.rom`, this needs a local Kickstart ROM and a real
//! `Devs/serial.device` driver file as never-committed test assets (see
//! tests/README.md); it skips cleanly wherever either is absent.
//!
//! The probe (`guest/modem-test/modemtest.c`) opens `serial.device`
//! directly via `OpenDevice`/`DoIO` -- the way a real terminal program
//! (Term, NComm) does it, not through a dos.library filename (`SER:`
//! needs a `Mount` entry this boot volume has no reason to carry; `AUX:`
//! hands back an interactive console, not a byte stream a program reads
//! and writes itself) -- runs `ATZ`, dials the host:port this test hands
//! it through a `DIALTARGET` file next to the binary (an ephemeral port
//! avoids baking a fixed one into the committed binary), sends a line,
//! escapes, and hangs up, logging a full transcript to `MODEMLOG`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Local ROM/driver assets: `COPPERLINE_TEST_ASSETS`, else `test-assets/`,
/// else the repo root (tests/README.md).
fn asset_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("COPPERLINE_TEST_ASSETS") {
        return PathBuf::from(dir);
    }
    let dir = repo_root().join("test-assets");
    if dir.is_dir() {
        return dir;
    }
    repo_root()
}

/// The Kickstart image, by the filename `AGENTS.md`'s own examples use.
/// `Devs/serial.device` has no equally obvious conventional name, so it is
/// looked for at a fixed path under the asset directory instead:
/// `modem/Devs/serial.device`.
const KICKSTART: &str = "KICK31.ROM";

fn required_assets_present() -> Option<(PathBuf, PathBuf)> {
    let assets = asset_dir();
    let kick = [assets.clone(), repo_root()]
        .into_iter()
        .map(|dir| dir.join(KICKSTART))
        .find(|p| p.is_file())?;
    let driver = assets.join("modem/Devs/serial.device");
    if !driver.is_file() {
        return None;
    }
    Some((kick, driver))
}

fn tail_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let start = text.len().saturating_sub(4096);
    text[start..].to_string()
}

/// A one-shot local "BBS": accepts a single connection, sends a greeting,
/// reads whatever the guest sends, echoes it back prefixed, then closes.
/// Bound to an OS-assigned port so the test carries no fixed-port
/// collision risk with anything else running on the host.
fn spawn_peer() -> (u16, mpsc::Receiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral peer port");
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("modem-e2e-peer".into())
        .spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = stream.write_all(b"BBS WELCOME\r\n");
            let mut buf = [0u8; 256];
            let mut got = Vec::new();
            // TCP gives no message boundaries: the guest's line can arrive
            // split across more than one segment, so keep reading (with a
            // short per-read timeout as the "nothing more is coming" signal)
            // until the terminating CR shows up or the read times out.
            stream
                .set_read_timeout(Some(std::time::Duration::from_millis(500)))
                .ok();
            while !got.contains(&b'\r') {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => got.extend_from_slice(&buf[..n]),
                    Err(_) => break,
                }
            }
            let mut reply = b"ECHO: ".to_vec();
            reply.extend_from_slice(&got);
            let _ = stream.write_all(&reply);
            let _ = tx.send(got);
            std::thread::sleep(std::time::Duration::from_secs(2));
        })
        .expect("spawn peer thread");
    (port, rx)
}

/// Escape a path for inclusion in a single-quoted TOML literal string (no
/// escape processing, so it survives Windows backslashes unmodified).
fn toml_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

#[test]
#[ignore = "runs the emulator and requires a local Kickstart ROM plus Devs/serial.device (tests/README.md)"]
fn modem_dials_out_and_relays_over_real_tcp() -> Result<(), Box<dyn std::error::Error>> {
    let Some((kickstart, serial_device)) = required_assets_present() else {
        eprintln!(
            "skipping modem e2e test; need {KICKSTART} and test-assets/modem/Devs/serial.device"
        );
        return Ok(());
    };

    let (peer_port, peer_rx) = spawn_peer();

    let mount = std::env::temp_dir().join(format!(
        "copperline-modem-e2e-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let _ = std::fs::remove_dir_all(&mount);
    std::fs::create_dir_all(mount.join("S"))?;
    std::fs::create_dir_all(mount.join("Devs"))?;
    std::fs::copy(
        repo_root().join("guest/modem-test/modemtest"),
        mount.join("modemtest"),
    )?;
    std::fs::copy(&serial_device, mount.join("Devs/serial.device"))?;
    std::fs::write(mount.join("DIALTARGET"), format!("127.0.0.1:{peer_port}"))?;
    std::fs::write(mount.join("S/Startup-Sequence"), "modemtest\n")?;

    let config = std::env::temp_dir().join(format!(
        "copperline-modem-e2e-{}-{}.toml",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    std::fs::write(
        &config,
        format!(
            r#"
rom = '{}'

[serial]
mode = "modem"

[[filesys]]
path = '{}'
volume = "HOSTFS0"
bootpri = 5
"#,
            toml_path(&kickstart),
            toml_path(&mount)
        ),
    )?;

    let screenshot = std::env::temp_dir().join(format!(
        "copperline-modem-e2e-{}-{}.png",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));

    let output = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .env("RUST_LOG", "copperline=warn")
        .arg("--noaudio")
        .arg("--config")
        .arg(&config)
        .arg("--screenshot-after")
        .arg("25.0")
        .arg(&screenshot)
        .output()?;
    if !output.status.success() {
        panic!(
            "copperline exited with {}\nstdout tail:\n{}\nstderr tail:\n{}",
            output.status,
            tail_text(&output.stdout),
            tail_text(&output.stderr)
        );
    }
    let _ = std::fs::remove_file(&screenshot);
    let _ = std::fs::remove_file(&config);

    let log = std::fs::read_to_string(mount.join("MODEMLOG"))
        .map_err(|e| format!("MODEMLOG missing or unreadable ({e}); the probe may not have run"))?;

    assert!(
        !log.contains("could not open DIALTARGET"),
        "probe could not find its dial target: {log}"
    );
    assert!(
        !log.contains("OpenDevice(serial.device) err="),
        "probe could not open serial.device: {log}"
    );
    assert!(log.contains("[TX 4] ATZ"), "no ATZ transmitted: {log}");
    assert!(log.contains("OK"), "ATZ was not answered OK: {log}");
    assert!(
        log.contains(&format!("ATDT127.0.0.1:{peer_port}")),
        "ATDT was not transmitted with the right target: {log}"
    );
    assert!(
        log.contains("CONNECT"),
        "the dial never reported CONNECT: {log}"
    );
    assert!(
        log.contains("BBS WELCOME"),
        "the peer's greeting never reached the guest: {log}"
    );
    assert!(
        log.contains("modemtest done"),
        "probe did not finish: {log}"
    );

    let received_by_peer = peer_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| "peer never received anything from the guest")?;
    assert!(
        received_by_peer.starts_with(b"HELLO FROM AMIGA"),
        "peer received {received_by_peer:?}"
    );

    let _ = std::fs::remove_dir_all(&mount);
    Ok(())
}
