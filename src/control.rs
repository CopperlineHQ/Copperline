// SPDX-License-Identifier: GPL-3.0-or-later

//! The Copperline Control Protocol (CCP): a versioned JSON-RPC interface
//! over loopback TCP for driving the emulator from scripts and external
//! tools.
//!
//! Two server modes share one command surface:
//!
//! - headless (`--control ADDR`): the server owns the [`Emulator`] and
//!   drives it directly, one client at a time, exactly like the GDB stub
//!   (`gdbstub::run`); see `control::headless`.
//! - windowed (`--control-gui ADDR`): socket threads enqueue typed
//!   commands over an mpsc channel and the winit frame loop drains them
//!   at a frame boundary; see `control::windowed`.
//!
//! Like the GDB stub, this is a host debugger transport, not an emulated
//! device: inspection commands are side-effect-free, and every mutation a
//! client injects (input, media, memory pokes) lands at a deterministic
//! boundary of the emulated timeline and is journaled so the session
//! stays reproducible. The wire format lives in `control::proto`.
//!
//! [`Emulator`]: crate::emulator::Emulator

pub mod bridge;
pub mod catalogue;
#[cfg(feature = "dap")]
pub mod dap;
pub mod diverge;
pub mod exec;
pub mod headless;
pub mod mcp;
pub mod observe;
pub mod proto;
pub mod session;
pub mod toaster_link;
pub mod windowed;

use anyhow::{Context, Result};
use std::io::Write as _;
use std::path::PathBuf;

/// Startup settings for a control server, mirroring `gdbstub::Config`:
/// the CLI carries the listen address and token plumbing, the reverse-
/// debugging knobs come from the same environment variables the other
/// debugger frontends use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub listen: String,
    /// Pinned auth token (`--control-token`); `None` generates one.
    pub token: Option<String>,
    /// File to write the connection info line to (`--control-info`).
    pub info_file: Option<PathBuf>,
    /// Journal control-injected input into this `.clscript`
    /// (`--record-input`, headless mode).
    pub record_input: Option<PathBuf>,
    /// `--run PROG`: arm a one-shot loadseg catch for this program at
    /// startup, so the first resume stops the moment the guest OS loads
    /// it (before its first instruction), like `gdbstub::Config`.
    pub stop_on_load: Option<String>,
    /// The machine's screenshot framing (`[video] overscan`, the TV
    /// centre), so `--record-input` can write light-pen lines in the
    /// pixels `--pen-after` replays.
    pub screenshot_framing: crate::video::present_common::ScreenshotFraming,
    pub reverse_budget_mb: usize,
    pub reverse_interval_frames: u64,
}

impl Config {
    pub fn new(listen: String) -> Self {
        Self {
            listen,
            token: None,
            info_file: None,
            record_input: None,
            stop_on_load: None,
            screenshot_framing: Default::default(),
            reverse_budget_mb: crate::envcfg::var("COPPERLINE_DBG_RR_BUDGET_MB")
                .and_then(|s| s.trim().parse::<usize>().ok())
                .unwrap_or(crate::debugger::RR_DEFAULT_BUDGET_MB),
            reverse_interval_frames: crate::envcfg::var("COPPERLINE_DBG_RR_INTERVAL")
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(crate::debugger::RR_DEFAULT_INTERVAL_FRAMES),
        }
    }

    /// The pinned token, or a fresh token from the operating system CSPRNG.
    /// Randomness failure prevents the server from starting.
    pub fn resolve_token(&self) -> Result<String> {
        match &self.token {
            Some(token) if token.is_empty() => anyhow::bail!("control token must not be empty"),
            Some(token) => Ok(token.clone()),
            None => generate_token(),
        }
    }
}

/// Generate a 128-bit session token as 32 lowercase hex characters.
/// There is no fallback when the OS random source fails.
pub fn generate_token() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).context("generating control token from OS randomness")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Announce only the endpoint and handoff path, never the token. An explicit
/// info file is replaced atomically, so existing permissions and symlinks
/// cannot expose the new token. With no path, keep a private temporary file
/// alive for the server's lifetime and remove it on normal shutdown.
pub fn announce(
    addr: &std::net::SocketAddr,
    token: &str,
    info_file: Option<&PathBuf>,
) -> Result<Option<tempfile::NamedTempFile>> {
    let mut contents = serde_json::to_vec(&serde_json::json!({
        "listen": addr.to_string(), "token": token, "proto": proto::PROTO_VERSION,
    }))?;
    contents.push(b'\n');
    let auto_file = if let Some(path) = info_file {
        write_private_file(path, &contents)
            .with_context(|| format!("writing control info file {}", path.display()))?;
        None
    } else {
        let mut file = tempfile::Builder::new()
            .prefix("copperline-control-")
            .suffix(".json")
            .tempfile()
            .context("creating private control info file")?;
        file.write_all(&contents)?;
        file.flush()?;
        Some(file)
    };
    let path = info_file
        .map(|p| p.as_path())
        .or_else(|| auto_file.as_ref().map(|f| f.path()))
        .expect("control info file was written");
    eprintln!(
        "copperline-control: listen={addr} info={} proto={}",
        path.display(),
        proto::PROTO_VERSION
    );
    Ok(auto_file)
}

/// Open options that create a file readable by the owner only (0600 on
/// unix, the platform default elsewhere), for the log the MCP bridge
/// points a launched emulator's stderr at. The mode is set as the file is
/// created, never by a chmod afterwards.
pub(crate) fn owner_only_create() -> std::fs::OpenOptions {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    opts
}

/// Atomically replace the handoff file with a new private file. Creating in
/// the destination directory keeps the rename on the same filesystem.
fn write_private_file(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(contents)?;
    file.flush()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Build a minimal asset-free emulator for control tests: a 68000 whose
/// ROM program loops incrementing D0 and writing it to chip RAM, so
/// stepping, breakpoints, watches, and last-writer scans all have
/// something deterministic to bite on.
///
/// ```text
/// F80010  NOP
/// F80012  ADDQ.L #1,D0
/// F80014  MOVE.W D0,($20000).L
/// F8001A  BRA.S  F80010
/// ```
#[cfg(test)]
pub(crate) fn test_emulator() -> crate::emulator::Emulator {
    let mut rom = vec![0u8; crate::memory::ROM_SIZE];
    let put_word = |mem: &mut [u8], off: usize, word: u16| {
        mem[off..off + 2].copy_from_slice(&word.to_be_bytes());
    };
    put_word(&mut rom, 0x10, 0x4E71); // NOP
    put_word(&mut rom, 0x12, 0x5280); // ADDQ.L #1,D0
    put_word(&mut rom, 0x14, 0x33C0); // MOVE.W D0,(abs).L
    put_word(&mut rom, 0x16, 0x0002);
    put_word(&mut rom, 0x18, 0x0000);
    put_word(&mut rom, 0x1A, 0x60F4); // BRA.S F80010

    let mut chip_ram = vec![0u8; 512 * 1024];
    chip_ram[0..4].copy_from_slice(&0x0000_4000u32.to_be_bytes()); // reset SSP
    chip_ram[4..8].copy_from_slice(&0x00F8_0010u32.to_be_bytes()); // reset PC

    let bus = crate::bus::Bus::new(
        crate::memory::Memory {
            chip_ram,
            slow_ram: Vec::new(),
            mb_ram: Vec::new(),
            accel_ram: Vec::new(),
            rom,
            overlay: false,
            zorro: crate::zorro::ZorroChain::default(),
            extended_rom: Vec::new(),
            extended_rom_base: 0,
            wcs: Vec::new(),
            wcs_write_protected: false,
        },
        crate::chipset::paula::Paula::new(
            Box::new(crate::serial::NullSerialSink),
            Box::new(crate::audio::NullSink),
        ),
        crate::floppy::FloppyController::default(),
    );
    crate::emulator::Emulator::new(
        bus,
        crate::config::CpuModel::M68000,
        false,
        Default::default(),
        crate::config::PacingBudget::Cycles,
        2,
        false,
    )
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_32_hex_and_unique() {
        let a = generate_token().unwrap();
        let b = generate_token().unwrap();
        assert_eq!(a.len(), 32);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(a, b);
    }

    #[test]
    fn config_pins_token() {
        let mut cfg = Config::new(":0".into());
        assert_eq!(cfg.resolve_token().unwrap().len(), 32);
        cfg.token = Some("sesame".into());
        assert_eq!(cfg.resolve_token().unwrap(), "sesame");
        cfg.token = Some(String::new());
        assert!(cfg.resolve_token().is_err());
    }

    #[test]
    fn automatic_info_file_is_private_and_removed_on_drop() -> Result<()> {
        let addr = "127.0.0.1:4321".parse()?;
        let file = announce(&addr, "secret\"\nvalue", None)?.unwrap();
        let path = file.path().to_owned();
        let body: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        assert_eq!(body["token"], "secret\"\nvalue");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path)?.permissions().mode() & 0o777,
                0o600
            );
        }
        drop(file);
        assert!(!path.exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn info_file_replaces_public_files_and_symlinks_without_writing_through() -> Result<()> {
        use std::os::unix::{fs::symlink, fs::PermissionsExt as _};
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("info.json");
        let target = dir.path().join("public.txt");
        std::fs::write(&target, "untouched")?;
        symlink(&target, &path)?;
        let addr = "127.0.0.1:4321".parse()?;
        announce(&addr, "new-secret", Some(&path))?;
        assert!(!path.is_symlink());
        assert_eq!(std::fs::read_to_string(&target)?, "untouched");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        announce(&addr, "replacement-secret", Some(&path))?;
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );
        let body: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        assert_eq!(body["token"], "replacement-secret");
        Ok(())
    }

    #[test]
    fn info_file_is_json_line() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("ccp-info-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("info.json");
        let addr: std::net::SocketAddr = "127.0.0.1:4321".parse()?;
        announce(&addr, "deadbeef", Some(&path))?;
        let body = std::fs::read_to_string(&path)?;
        assert!(body.ends_with('\n'));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body)?,
            serde_json::json!({"listen": "127.0.0.1:4321", "token": "deadbeef", "proto": 1})
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path)?.permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).ok();
        Ok(())
    }
}
