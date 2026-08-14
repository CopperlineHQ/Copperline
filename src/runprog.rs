// SPDX-License-Identifier: GPL-3.0-or-later

//! Warp launch: `--run program` boots straight into an ordinary Amiga
//! executable from the host, with no disk image, no Workbench, and no
//! support assets.
//!
//! The staging mirrors the WHDLoad direct boot (src/whdload.rs) at its
//! smallest: two host directories mounted live through the services board
//! (src/filesys.rs). The boot volume is nothing but a generated
//! `S/Startup-Sequence`; `CD` and `FailAt` are internal shell commands from
//! Kickstart 2.0 on (AROS included), and the program itself is named by an
//! absolute AmigaDOS path, so no `C:` commands are staged at all:
//!
//! - `<config dir>/run/boot/` (volume `RunBoot:`, boot priority 6): the
//!   generated `S/Startup-Sequence`. Regenerated on every launch.
//! - the program's own directory (volume `RunProg:`, writable): mounted in
//!   place, so a freshly built executable is picked up on the next launch
//!   and anything the program writes lands next to it on the host.
//!
//! Unlike WHDLoad no machine is derived: the session boots whatever the
//! configuration and CLI flags describe (the bundled AROS ROM by default).
//!
//! The perceived instant boot comes from [`WarpLaunch`]: a windowed session
//! starts unpaced with live audio muted, polls the LoadSeg tracker
//! (src/amigaos.rs) once per emulated frame, and returns to real-time
//! pacing the moment the guest OS loads the target program -- the same
//! trick the BartmanAbyss WinUAE fork calls warp launch, minus its
//! per-title scan. A fallback deadline keeps a misspelled or crashing
//! Startup-Sequence from warping (silently) forever.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::config::{RawConfig, RawFilesysMount};

/// Boot-volume name; the program volume follows it.
pub const BOOT_VOLUME: &str = "RunBoot";
/// Volume name the program's host directory is mounted under.
pub const PROG_VOLUME: &str = "RunProg";

/// DF0: enters the boot vote at priority 5; the staged boot volume must win.
const BOOT_PRIORITY: i8 = 6;

/// How long a warp launch keeps warping before giving up on ever seeing the
/// program load, in emulated seconds from engagement. A cold AROS or
/// Kickstart boot to the Startup-Sequence is well under a minute of
/// emulated time; anything beyond this is a boot that went wrong, and the
/// session should be watchable (and audible) while the user reads why.
pub const WARP_LAUNCH_TIMEOUT_SECS: f64 = 60.0;

/// A staged quick-run: the generated boot volume and the program mount.
pub struct PreparedRun {
    /// The staged boot volume (mounted as [`BOOT_VOLUME`], boot priority 6).
    pub boot_dir: PathBuf,
    /// The program's own host directory (mounted as [`PROG_VOLUME`]).
    pub prog_dir: PathBuf,
    /// The program's file name, as the guest shell will see it.
    pub prog_name: String,
}

/// The generated `S/Startup-Sequence`: fail only on real errors, make the
/// program's own directory current (so its relative asset paths work), and
/// run it by absolute path.
fn startup_sequence(prog_name: &str, extra_args: Option<&str>) -> String {
    let mut run = format!("\"{PROG_VOLUME}:{prog_name}\"");
    if let Some(args) = extra_args {
        let args = args.trim();
        if !args.is_empty() {
            run.push(' ');
            run.push_str(args);
        }
    }
    format!("FailAt 21\nCD \"{PROG_VOLUME}:\"\n{run}\n")
}

/// Stage the boot volume for `program` and describe the two mounts.
///
/// `stage_root` overrides where the boot volume is written (tests); the
/// default is [`crate::paths::run_stage_dir`]. The boot volume is
/// regenerated from scratch on every launch, exactly like the WHDLoad boot
/// volume, so nothing stale survives a change of program or arguments.
pub fn prepare(
    program: &Path,
    extra_args: Option<&str>,
    stage_root: Option<&Path>,
) -> Result<PreparedRun> {
    let program =
        std::path::absolute(program).with_context(|| format!("resolving {}", program.display()))?;
    if !program.is_file() {
        bail!(
            "--run needs an Amiga executable file, and {} is not one",
            program.display()
        );
    }
    let prog_dir = match program.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => bail!("{} has no parent directory to mount", program.display()),
    };
    let prog_name = program
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let stage_root = match stage_root {
        Some(dir) => dir.to_path_buf(),
        None => crate::paths::run_stage_dir()
            .context("no per-user directory available to stage the --run boot volume")?,
    };
    let boot_dir = stage_root.join("boot");
    if boot_dir.exists() {
        std::fs::remove_dir_all(&boot_dir)
            .with_context(|| format!("clearing {}", boot_dir.display()))?;
    }
    std::fs::create_dir_all(boot_dir.join("S"))
        .with_context(|| format!("creating {}", boot_dir.join("S").display()))?;
    std::fs::write(
        boot_dir.join("S").join("Startup-Sequence"),
        startup_sequence(&prog_name, extra_args),
    )
    .with_context(|| format!("writing the Startup-Sequence under {}", boot_dir.display()))?;

    Ok(PreparedRun {
        boot_dir,
        prog_dir,
        prog_name,
    })
}

/// Mount the two staged volumes. Nothing else is touched: unlike the
/// WHDLoad derivation the machine, ROM, and memory stay whatever the
/// configuration and CLI flags say.
pub fn apply_to_raw(raw: &mut RawConfig, prepared: &PreparedRun) {
    raw.filesys.push(RawFilesysMount {
        path: prepared.boot_dir.to_string_lossy().into_owned(),
        volume: Some(BOOT_VOLUME.to_string()),
        bootpri: Some(BOOT_PRIORITY),
        readonly: None,
    });
    raw.filesys.push(RawFilesysMount {
        path: prepared.prog_dir.to_string_lossy().into_owned(),
        volume: Some(PROG_VOLUME.to_string()),
        bootpri: None,
        readonly: None,
    });
}

/// What one [`WarpLaunch::note`] poll concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarpLaunchOutcome {
    /// Keep warping.
    Waiting,
    /// The target program was loaded; return to real-time pacing.
    Loaded,
    /// The deadline passed without the target appearing; return to
    /// real-time pacing anyway so the failed boot is watchable.
    TimedOut,
}

/// The launch-phase state machine: warp from power-on until the guest OS
/// loads the target program, then hand back to real-time pacing.
///
/// Pure bookkeeping over emulated time so it tests without an emulator;
/// the owner does the actual pacing and audio calls. `engaged` records
/// whether the session really is warping -- a machine with a bridged
/// physical drive refuses to run unpaced (src/emulator.rs `set_paced`),
/// in which case the gate still watches for the program (and the audio
/// stays live) but there is no warp to disengage.
pub struct WarpLaunch {
    /// The program's file name, matched case-insensitively against loaded
    /// module names.
    target: String,
    /// Emulated-seconds deadline, set at engagement.
    deadline_secs: Option<f64>,
    /// Whether the machine actually entered warp (audio is muted only
    /// while this is true).
    pub engaged: bool,
}

impl WarpLaunch {
    pub fn new(target: String) -> Self {
        Self {
            target,
            deadline_secs: None,
            engaged: false,
        }
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    /// Whether [`WarpLaunch::engage`] has run.
    pub fn started(&self) -> bool {
        self.deadline_secs.is_some()
    }

    /// Start the launch phase at `now_secs` of emulated time. `unpaced`
    /// says whether the machine really went unpaced.
    pub fn engage(&mut self, now_secs: f64, unpaced: bool) {
        self.deadline_secs = Some(now_secs + WARP_LAUNCH_TIMEOUT_SECS);
        self.engaged = unpaced;
    }

    /// Feed one poll: the current emulated time and the name of a module
    /// the LoadSeg tracker just saw loaded, if any.
    pub fn note(&mut self, now_secs: f64, loaded_name: Option<&str>) -> WarpLaunchOutcome {
        if loaded_name.is_some_and(|name| name.eq_ignore_ascii_case(&self.target)) {
            return WarpLaunchOutcome::Loaded;
        }
        match self.deadline_secs {
            Some(deadline) if now_secs >= deadline => WarpLaunchOutcome::TimedOut,
            _ => WarpLaunchOutcome::Waiting,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lha::tests::temp_dir;

    #[test]
    fn startup_sequence_quotes_the_program_and_cds_to_the_volume() {
        assert_eq!(
            startup_sequence("hello", None),
            "FailAt 21\nCD \"RunProg:\"\n\"RunProg:hello\"\n"
        );
        assert_eq!(
            startup_sequence("my game", Some("  -level 2  ")),
            "FailAt 21\nCD \"RunProg:\"\n\"RunProg:my game\" -level 2\n"
        );
        // Whitespace-only args collapse to none.
        assert_eq!(
            startup_sequence("hello", Some("   ")),
            "FailAt 21\nCD \"RunProg:\"\n\"RunProg:hello\"\n"
        );
    }

    #[test]
    fn prepare_stages_and_regenerates_the_boot_volume() {
        let dir = temp_dir("runprog-stage");
        let program = dir.join("build").join("hello");
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, b"hunk").unwrap();
        let stage = dir.join("stage");

        let prepared = prepare(&program, Some("-x"), Some(&stage)).unwrap();
        assert_eq!(prepared.prog_name, "hello");
        assert_eq!(prepared.prog_dir, program.parent().unwrap());
        assert_eq!(prepared.boot_dir, stage.join("boot"));
        let script =
            std::fs::read_to_string(prepared.boot_dir.join("S").join("Startup-Sequence")).unwrap();
        assert_eq!(script, "FailAt 21\nCD \"RunProg:\"\n\"RunProg:hello\" -x\n");

        // A second launch regenerates the boot volume from scratch: a file
        // left over from an earlier stage must not survive.
        std::fs::write(prepared.boot_dir.join("stale"), b"old").unwrap();
        let prepared = prepare(&program, None, Some(&stage)).unwrap();
        assert!(!prepared.boot_dir.join("stale").exists());
        let script =
            std::fs::read_to_string(prepared.boot_dir.join("S").join("Startup-Sequence")).unwrap();
        assert_eq!(script, "FailAt 21\nCD \"RunProg:\"\n\"RunProg:hello\"\n");
    }

    #[test]
    fn prepare_rejects_missing_and_directory_targets() {
        let dir = temp_dir("runprog-reject");
        assert!(prepare(&dir.join("absent"), None, Some(&dir)).is_err());
        assert!(prepare(&dir, None, Some(&dir)).is_err());
    }

    #[test]
    fn apply_to_raw_mounts_boot_then_program_volume() {
        let dir = temp_dir("runprog-mounts");
        let program = dir.join("hello");
        std::fs::write(&program, b"hunk").unwrap();
        let prepared = prepare(&program, None, Some(&dir.join("stage"))).unwrap();

        let mut raw = RawConfig::default();
        apply_to_raw(&mut raw, &prepared);
        assert_eq!(raw.filesys.len(), 2);
        assert_eq!(raw.filesys[0].volume.as_deref(), Some(BOOT_VOLUME));
        assert_eq!(raw.filesys[0].bootpri, Some(6));
        assert_eq!(
            raw.filesys[0].path,
            prepared.boot_dir.to_string_lossy().into_owned()
        );
        assert_eq!(raw.filesys[1].volume.as_deref(), Some(PROG_VOLUME));
        assert_eq!(raw.filesys[1].bootpri, None);
        assert_eq!(
            raw.filesys[1].path,
            prepared.prog_dir.to_string_lossy().into_owned()
        );
        // Neither mount is read-only: the boot volume is disposable and the
        // program writes output next to itself.
        assert!(raw.filesys.iter().all(|m| m.readonly.is_none()));
    }

    #[test]
    fn warp_launch_matches_case_insensitively_and_times_out() {
        let mut launch = WarpLaunch::new("Hello".to_string());
        launch.engage(10.0, true);
        assert!(launch.engaged);
        assert_eq!(launch.note(11.0, None), WarpLaunchOutcome::Waiting);
        assert_eq!(
            launch.note(12.0, Some("SetPatch")),
            WarpLaunchOutcome::Waiting
        );
        assert_eq!(launch.note(13.0, Some("HELLO")), WarpLaunchOutcome::Loaded);
        // Past the deadline the gate gives up (a match still wins).
        assert_eq!(
            launch.note(10.0 + WARP_LAUNCH_TIMEOUT_SECS, None),
            WarpLaunchOutcome::TimedOut
        );
        assert_eq!(
            launch.note(10.0 + WARP_LAUNCH_TIMEOUT_SECS, Some("hello")),
            WarpLaunchOutcome::Loaded
        );

        // A bridged physical drive refuses warp: the gate still watches,
        // but records that nothing was engaged.
        let mut paced = WarpLaunch::new("hello".to_string());
        paced.engage(0.0, false);
        assert!(!paced.engaged);
        assert_eq!(paced.note(1.0, Some("hello")), WarpLaunchOutcome::Loaded);
    }
}
