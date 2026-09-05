// SPDX-License-Identifier: GPL-3.0-or-later

//! Warp launch: `--run program` boots straight into an ordinary Amiga
//! executable from the host, with no disk image, no Workbench, and no
//! support assets.
//!
//! The staging mirrors the WHDLoad direct boot (src/whdload.rs) at its
//! smallest: two host directories mounted live through the services board
//! (src/filesys.rs). The boot volume contains a generated
//! `S/Startup-Sequence` and small redistributable `C:` commands for the
//! bare Kickstart 1.3 CLI. Later ROMs can use their internal commands.
//!
//! - `<config dir>/run/boot-<pid>/` (volume `RunBoot:`, boot priority 6):
//!   the generated `S/Startup-Sequence` and bundled commands. Regenerated on every launch and
//!   per-process, so concurrent instances never rewrite each other's
//!   live-mounted volume; stale siblings are swept by age.
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
//! per-title scan. A completion marker echoed by the Startup-Sequence
//! catches a program too short-lived for the frame poll to see, and a
//! fallback deadline keeps a misspelled or crashing Startup-Sequence
//! from warping (silently) forever.

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
#[derive(Debug)]
pub struct PreparedRun {
    /// The staged boot volume (mounted as [`BOOT_VOLUME`], boot priority 6).
    pub boot_dir: PathBuf,
    /// The program's own host directory (mounted as [`PROG_VOLUME`]).
    pub prog_dir: PathBuf,
    /// The program's file name, as the guest shell will see it.
    pub prog_name: String,
}

/// Name of the completion marker the Startup-Sequence echoes into the
/// boot volume after the program returns. The warp gate polls the LoadSeg
/// tracker once per emulated frame, which can miss a program that loads,
/// runs, and exits inside a single frame; the marker is host-visible the
/// moment the guest writes it, so even the fastest program ends the warp.
pub const DONE_MARKER: &str = "done";
const DETACHED_SCRIPT: &str = "Detached-Run";

/// Normal relocatable 68000 hunk executables, rebuilt with
/// `make -C guest/run-tools`; embedding keeps packaged launches self-contained.
const BOOT_COMMANDS: &[(&str, &[u8])] = &[
    ("FailAt", include_bytes!("../guest/run-tools/FailAt")),
    ("CD", include_bytes!("../guest/run-tools/CD")),
    ("Stack", include_bytes!("../guest/run-tools/Stack")),
    ("Echo", include_bytes!("../guest/run-tools/Echo")),
    ("Execute", include_bytes!("../guest/run-tools/Execute")),
];

/// Guest-shell options used by debugger launches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunOptions {
    /// Shell stack size in bytes (`Stack N`) before starting the program.
    pub stack: Option<u32>,
    /// Start the program asynchronously and close the boot shell.
    pub detach: bool,
}

/// Keep host launch validation aligned with the bundled 1.x Stack command:
/// at least 2 KiB, with room to round up within DOS's signed byte count.
pub fn validate_stack_size(bytes: u64) -> Result<u32> {
    if !(2048..=2_147_483_644).contains(&bytes) {
        bail!("stack must be between 2048 and 2147483644 bytes");
    }
    Ok(bytes as u32)
}

/// The script that actually runs the program and drops the completion marker
/// when it returns. Detached launches execute this in their child CLI, so
/// closing the boot CLI cannot skip the marker.
fn program_sequence(prog_name: &str, extra_args: Option<&str>, stack: Option<u32>) -> String {
    let mut run = format!("\"{PROG_VOLUME}:{prog_name}\"");
    if let Some(args) = extra_args {
        let args = args.trim();
        if !args.is_empty() {
            run.push(' ');
            run.push_str(args);
        }
    }
    let stack = stack.map_or_else(String::new, |n| format!("Stack {n}\n"));
    format!(
        "FailAt 21\n{stack}CD \"{PROG_VOLUME}:\"\n{run}\nEcho >\"{BOOT_VOLUME}:{DONE_MARKER}\" \"done\"\n"
    )
}

/// The generated `S/Startup-Sequence`: a foreground launch performs the work
/// directly. A detached launch starts a child `Execute` script and closes only
/// the parent CLI; the child owns both the program and completion marker.
/// Detached launches require the ROM-resident Run/EndCLI commands
/// available on Kickstart 2.0+ and AROS.
fn startup_sequence(prog_name: &str, extra_args: Option<&str>, options: RunOptions) -> String {
    if options.detach {
        format!(
            "FailAt 21\nRun >NIL: <NIL: Execute \"{BOOT_VOLUME}:S/{DETACHED_SCRIPT}\"\nEndCLI\n"
        )
    } else {
        program_sequence(prog_name, extra_args, options.stack)
    }
}

/// Whether the guest can address this file name at all. The services
/// board serves Latin-1 names, the generated script is written as UTF-8,
/// and the run line quotes the name -- so anything outside printable
/// ASCII, a quote, or an AmigaDOS path separator would either be hidden
/// from the guest or corrupt the script, and the launch would warp until
/// the timeout with nothing to show. Rejecting the name up front turns
/// that into an immediate, explainable error.
fn guest_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| matches!(c, ' '..='~') && !matches!(c, '"' | ':' | '/'))
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
    prepare_with_options(program, extra_args, RunOptions::default(), stage_root)
}

/// Stage a run with debugger-controlled guest shell options.
pub fn prepare_with_options(
    program: &Path,
    extra_args: Option<&str>,
    options: RunOptions,
    stage_root: Option<&Path>,
) -> Result<PreparedRun> {
    if let Some(stack) = options.stack {
        validate_stack_size(u64::from(stack))?;
    }
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
    if !guest_safe_name(&prog_name) {
        bail!(
            "--run cannot address {prog_name:?} from the guest (the host filesystem \
             mount serves plain ASCII names, and quotes, colons, and slashes are \
             AmigaDOS syntax); rename the file"
        );
    }

    let stage_root = match stage_root {
        Some(dir) => dir.to_path_buf(),
        None => crate::paths::run_stage_dir()
            .context("no per-user directory available to stage the --run boot volume")?,
    };
    // Per-process staging: two Copperline instances launched together must
    // not delete or rewrite each other's live-mounted boot volume (the
    // mount resolves the path on every packet). Siblings left behind by
    // dead processes are a single tiny file each; sweep the stale ones
    // (best effort) instead of tracking process liveness.
    let boot_dir = stage_root.join(format!("boot-{}", std::process::id()));
    sweep_stale_boot_dirs(&stage_root, &boot_dir);
    if boot_dir.exists() {
        std::fs::remove_dir_all(&boot_dir)
            .with_context(|| format!("clearing {}", boot_dir.display()))?;
    }
    std::fs::create_dir_all(boot_dir.join("S"))
        .with_context(|| format!("creating {}", boot_dir.join("S").display()))?;
    let commands_dir = boot_dir.join("C");
    std::fs::create_dir_all(&commands_dir)
        .with_context(|| format!("creating {}", commands_dir.display()))?;
    for (name, bytes) in BOOT_COMMANDS {
        let path = commands_dir.join(name);
        std::fs::write(&path, bytes)
            .with_context(|| format!("writing boot command {}", path.display()))?;
    }
    std::fs::write(
        boot_dir.join("S").join("Startup-Sequence"),
        startup_sequence(&prog_name, extra_args, options),
    )
    .with_context(|| format!("writing the Startup-Sequence under {}", boot_dir.display()))?;
    if options.detach {
        std::fs::write(
            boot_dir.join("S").join(DETACHED_SCRIPT),
            program_sequence(&prog_name, extra_args, options.stack),
        )
        .with_context(|| {
            format!(
                "writing the detached run script under {}",
                boot_dir.display()
            )
        })?;
    }

    Ok(PreparedRun {
        boot_dir,
        prog_dir,
        prog_name,
    })
}

/// Remove `boot-*` siblings that have not been touched for a week. A
/// PID-named directory outlives its process only when that process died
/// without a next launch to reuse the id, so age is the honest signal;
/// a week comfortably exceeds any live session. Errors are ignored --
/// sweeping is housekeeping, never a reason to fail a launch.
fn sweep_stale_boot_dirs(stage_root: &Path, current: &Path) {
    const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);
    let Ok(entries) = std::fs::read_dir(stage_root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == current
            || !path.is_dir()
            || !entry.file_name().to_string_lossy().starts_with("boot-")
        {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > STALE_AFTER);
        if stale {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// Mount the two staged volumes. Nothing else is touched: unlike the
/// WHDLoad derivation the machine, ROM, and memory stay whatever the
/// configuration and CLI flags say.
pub fn apply_to_raw(raw: &mut RawConfig, prepared: &PreparedRun) {
    raw.run_program_dir = Some(prepared.prog_dir.clone());
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
    /// The completion marker appeared: the program already ran to the
    /// end -- fast enough that the per-frame LoadSeg poll never saw it
    /// scheduled. Return to real-time pacing.
    Finished,
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
    /// Host path of the [`DONE_MARKER`] the Startup-Sequence writes after
    /// the program returns; None disables the completion check (tests).
    done_marker: Option<PathBuf>,
    /// Emulated-seconds deadline, set at engagement.
    deadline_secs: Option<f64>,
    /// Whether the machine actually entered warp (audio is muted only
    /// while this is true).
    pub engaged: bool,
}

impl WarpLaunch {
    pub fn new(target: String, done_marker: Option<PathBuf>) -> Self {
        Self {
            target,
            done_marker,
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
    /// the LoadSeg tracker just saw loaded, if any. The completion-marker
    /// probe is one host `stat` per poll -- fifty per emulated second,
    /// noise even inside a full-speed warp burst.
    pub fn note(&mut self, now_secs: f64, loaded_name: Option<&str>) -> WarpLaunchOutcome {
        if loaded_name.is_some_and(|name| name.eq_ignore_ascii_case(&self.target)) {
            return WarpLaunchOutcome::Loaded;
        }
        if self.done_marker.as_deref().is_some_and(Path::exists) {
            return WarpLaunchOutcome::Finished;
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

    const DONE_LINE: &str = "Echo >\"RunBoot:done\" \"done\"\n";

    #[test]
    fn startup_sequence_quotes_the_program_and_cds_to_the_volume() {
        assert_eq!(
            startup_sequence("hello", None, RunOptions::default()),
            format!("FailAt 21\nCD \"RunProg:\"\n\"RunProg:hello\"\n{DONE_LINE}")
        );
        assert_eq!(
            startup_sequence("my game", Some("  -level 2  "), RunOptions::default()),
            format!("FailAt 21\nCD \"RunProg:\"\n\"RunProg:my game\" -level 2\n{DONE_LINE}")
        );
        // Whitespace-only args collapse to none.
        assert_eq!(
            startup_sequence("hello", Some("   "), RunOptions::default()),
            format!("FailAt 21\nCD \"RunProg:\"\n\"RunProg:hello\"\n{DONE_LINE}")
        );
    }

    #[test]
    fn startup_sequence_honours_stack_and_detach() {
        assert_eq!(
            startup_sequence(
                "hello",
                Some("-x"),
                RunOptions {
                    stack: Some(32_768),
                    detach: true,
                },
            ),
            "FailAt 21\nRun >NIL: <NIL: Execute \"RunBoot:S/Detached-Run\"\nEndCLI\n"
        );
        assert_eq!(
            program_sequence("hello", Some("-x"), Some(32_768)),
            format!("FailAt 21\nStack 32768\nCD \"RunProg:\"\n\"RunProg:hello\" -x\n{DONE_LINE}")
        );
    }

    #[test]
    fn detached_prepare_stages_the_child_completion_script() {
        let dir = temp_dir("runprog-detached");
        let program = dir.join("hello");
        std::fs::write(&program, b"hunk").unwrap();
        let prepared = prepare_with_options(
            &program,
            Some("-x"),
            RunOptions {
                stack: Some(32_768),
                detach: true,
            },
            Some(&dir.join("stage")),
        )
        .unwrap();
        let child =
            std::fs::read_to_string(prepared.boot_dir.join("S").join(DETACHED_SCRIPT)).unwrap();
        assert_eq!(
            child,
            format!("FailAt 21\nStack 32768\nCD \"RunProg:\"\n\"RunProg:hello\" -x\n{DONE_LINE}")
        );
    }

    #[test]
    fn invalid_stack_preserves_the_existing_boot_volume() {
        let dir = temp_dir("runprog-invalid-stack");
        let program = dir.join("hello");
        std::fs::write(&program, b"hunk").unwrap();
        let stage = dir.join("stage");
        let prepared = prepare(&program, None, Some(&stage)).unwrap();
        let sentinel = prepared.boot_dir.join("existing");
        std::fs::write(&sentinel, b"keep").unwrap();
        for stack in [0, 4, 2047, 2_147_483_645, u32::MAX] {
            let err = prepare_with_options(
                &program,
                None,
                RunOptions {
                    stack: Some(stack),
                    detach: false,
                },
                Some(&stage),
            )
            .unwrap_err();
            assert!(err
                .to_string()
                .contains("stack must be between 2048 and 2147483644"));
            assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn guest_safe_name_rejects_unaddressable_names() {
        assert!(guest_safe_name("hello"));
        assert!(guest_safe_name("my game 2"));
        assert!(guest_safe_name("demo_final-v2.exe"));
        assert!(!guest_safe_name(""));
        assert!(!guest_safe_name("say \"hi\"")); // breaks the quoted run line
        assert!(!guest_safe_name("vol:name")); // AmigaDOS device separator
        assert!(!guest_safe_name("a/b")); // AmigaDOS path separator
        assert!(!guest_safe_name("line\nbreak")); // control character
        assert!(!guest_safe_name("caf\u{e9}")); // outside ASCII, hidden or mangled
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
        // Per-process staging: concurrent instances must not share (and
        // delete) each other's live-mounted boot volume.
        assert_eq!(
            prepared.boot_dir,
            stage.join(format!("boot-{}", std::process::id()))
        );
        let script =
            std::fs::read_to_string(prepared.boot_dir.join("S").join("Startup-Sequence")).unwrap();
        assert_eq!(
            script,
            format!("FailAt 21\nCD \"RunProg:\"\n\"RunProg:hello\" -x\n{DONE_LINE}")
        );
        for name in ["FailAt", "CD", "Stack", "Echo", "Execute"] {
            let command = std::fs::read(prepared.boot_dir.join("C").join(name)).unwrap();
            assert_eq!(
                &command[..4],
                &0x3f3u32.to_be_bytes(),
                "{name}: HUNK_HEADER"
            );
        }

        // A second launch regenerates the boot volume from scratch: a file
        // left over from an earlier stage must not survive.
        std::fs::write(prepared.boot_dir.join("stale"), b"old").unwrap();
        std::fs::write(prepared.boot_dir.join("C/FailAt"), b"stale command").unwrap();
        let prepared = prepare(&program, None, Some(&stage)).unwrap();
        assert!(!prepared.boot_dir.join("stale").exists());
        assert_eq!(
            std::fs::read(prepared.boot_dir.join("C/FailAt")).unwrap(),
            include_bytes!("../guest/run-tools/FailAt")
        );
        let script =
            std::fs::read_to_string(prepared.boot_dir.join("S").join("Startup-Sequence")).unwrap();
        assert_eq!(
            script,
            format!("FailAt 21\nCD \"RunProg:\"\n\"RunProg:hello\"\n{DONE_LINE}")
        );

        // The stale-sibling sweep leaves fresh directories (like another
        // live instance's) alone and removes week-old ones.
        let fresh = stage.join("boot-99999");
        std::fs::create_dir_all(&fresh).unwrap();
        let prepared = prepare(&program, None, Some(&stage)).unwrap();
        assert!(fresh.exists());
        assert!(prepared.boot_dir.exists());
    }

    #[test]
    fn prepare_rejects_missing_and_directory_targets() {
        let dir = temp_dir("runprog-reject");
        assert!(prepare(&dir.join("absent"), None, Some(&dir)).is_err());
        assert!(prepare(&dir, None, Some(&dir)).is_err());
        // A name the guest cannot address fails up front, not by warping
        // to the timeout.
        let bad = dir.join("caf\u{e9}");
        std::fs::write(&bad, b"hunk").unwrap();
        let err = prepare(&bad, None, Some(&dir)).unwrap_err();
        assert!(err.to_string().contains("rename"), "err: {err:#}");
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
        assert_eq!(
            raw.run_program_dir.as_deref(),
            Some(prepared.prog_dir.as_path())
        );
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
    fn warp_launch_finishes_on_the_completion_marker() {
        let dir = temp_dir("runprog-marker");
        let marker = dir.join(DONE_MARKER);
        let mut launch = WarpLaunch::new("hello".to_string(), Some(marker.clone()));
        launch.engage(0.0, true);
        assert_eq!(launch.note(1.0, None), WarpLaunchOutcome::Waiting);
        // The program loaded, ran, and exited between two polls: the
        // guest-written marker still ends the warp.
        std::fs::write(&marker, b"done").unwrap();
        assert_eq!(launch.note(2.0, None), WarpLaunchOutcome::Finished);
        // A live load event outranks the marker.
        assert_eq!(launch.note(3.0, Some("hello")), WarpLaunchOutcome::Loaded);
    }

    #[test]
    fn warp_launch_matches_case_insensitively_and_times_out() {
        let mut launch = WarpLaunch::new("Hello".to_string(), None);
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
        let mut paced = WarpLaunch::new("hello".to_string(), None);
        paced.engage(0.0, false);
        assert!(!paced.engaged);
        assert_eq!(paced.note(1.0, Some("hello")), WarpLaunchOutcome::Loaded);
    }
}
