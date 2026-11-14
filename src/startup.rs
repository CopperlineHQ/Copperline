// SPDX-License-Identifier: GPL-3.0-or-later

//! Error presentation at the native binary's startup boundary. Desktop
//! launchers mark their invocation explicitly; pipes are also used by CLI
//! clients and are not evidence of a desktop launch.

use crate::cli::CliArgs;

/// Linux's desktop entry supplies this as its first argument. Consume only
/// that position, so a guest argument or filename cannot turn dialogs on.
pub fn arguments(args: impl IntoIterator<Item = String>) -> (bool, Vec<String>) {
    let mut args = args.into_iter().peekable();
    let desktop = args.next_if(|arg| arg == "--desktop").is_some();
    (desktop, args.collect())
}

/// Whether a desktop launch that failed to parse may still show its error
/// in a dialog. A command line of nothing but paths is what a file manager
/// builds for "Open With": it names no session mode that could be headless,
/// and its mistakes are the user's selection (two CD images, an archive
/// with a disk), not syntax. Any option leaves the error on stderr, since
/// the mode it asked for is unknown until the line parses. A leading `--`
/// is no option: it ends them, and is how macOS document opens pass their
/// files (`open_macos_documents`), so what follows it is paths whatever
/// they look like.
pub fn file_open_launch(desktop: bool, args: &[String]) -> bool {
    let (files, only_paths) = match args.split_first() {
        Some((first, rest)) if first == "--" => (rest, true),
        _ => (args, false),
    };
    desktop && !files.is_empty() && (only_paths || files.iter().all(|arg| !arg.starts_with('-')))
}

pub fn error_dialog_allowed(desktop: bool, cli: &CliArgs) -> bool {
    desktop
        && cli.screenshot_after.is_empty()
        && cli.expect_screenshot.is_empty()
        && cli.frame_dump.is_none()
        && cli.gif_after.is_empty()
        && cli.coverage.is_none()
        && !cli.exit_on_return
        && cli.benchmark_until.is_none()
        && cli.gdb.is_none()
        && cli.control.is_none()
        && cli.live_audio_profile_secs.is_none()
        && !cli.calibrate_gamepad
        && !cli.list_midi
        && !cli.list_serial_ports
        && !cli.list_audio_devices
        && !cli.list_monitors
        && !cli.list_net_interfaces
        && !cli.list_disks
        && !cli.list_sampler_inputs
        && !cli.list_drive_profiles
        && cli.host_disk_broker.is_none()
        && cli.net_helper_action.is_none()
}

pub fn show_error(error: &anyhow::Error) {
    rfd::MessageDialog::new()
        .set_title("Copperline could not start")
        .set_description(format!("{error:#}"))
        .set_level(rfd::MessageLevel::Error)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::parse_args_from;

    #[test]
    fn desktop_marker_is_only_consumed_at_the_launch_boundary() {
        let (desktop, args) = arguments(["--desktop", "--factory"].map(str::to_owned));
        assert!(desktop);
        assert_eq!(args, ["--factory"]);
        let (desktop, args) = arguments(["--run-args", "--desktop"].map(str::to_owned));
        assert!(!desktop);
        assert_eq!(args, ["--run-args", "--desktop"]);
    }

    /// A file manager opening a selection Copperline cannot combine must
    /// say so: the launch has no terminal for the error to go to.
    #[test]
    fn a_desktop_file_selection_that_does_not_parse_gets_a_dialog() {
        let args = |list: &[&str]| list.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        let two_cds = args(&["first.iso", "second.iso"]);
        assert!(parse_args_from(two_cds.clone()).is_err());
        assert!(file_open_launch(true, &two_cds));
        // Not a desktop launch, no files at all, or an option on the line:
        // the error stays on stderr.
        assert!(!file_open_launch(false, &two_cds));
        assert!(!file_open_launch(true, &[]));
        assert!(!file_open_launch(
            true,
            &args(&["--screenshot-after", "1", "a.iso", "b.iso"])
        ));
        assert!(!file_open_launch(true, &args(&["--bogus"])));
        // macOS hands a document selection over behind "--", which ends
        // the options: everything after it is a path, whatever its name.
        let behind = args(&["--", "first.iso", "second.iso"]);
        assert!(parse_args_from(behind.clone()).is_err());
        assert!(file_open_launch(true, &behind));
        assert!(file_open_launch(true, &args(&["--", "-odd name-.adf"])));
        assert!(!file_open_launch(true, &args(&["--"])));
        assert!(!file_open_launch(
            true,
            &args(&["--noaudio", "--", "a.iso", "b.iso"])
        ));
    }

    #[test]
    fn only_desktop_interactive_sessions_allow_a_dialog() {
        let parse =
            |args: &[&str]| parse_args_from(args.iter().map(|arg| arg.to_string())).unwrap();
        let interactive = parse(&["--factory", "--noaudio", "--config", "missing.toml"]);
        assert!(error_dialog_allowed(true, &interactive));
        assert!(!error_dialog_allowed(false, &interactive));
        for args in [
            vec!["--screenshot-after=0", "out.png"],
            vec!["--expect-screenshot=0", "out.png"],
            vec!["--dump-frames=out", "--dump-count=1"],
            vec!["--gif-after=0", "out.gif"],
            vec!["--coverage=out.info"],
            vec!["--exit-on-return"],
            vec!["--benchmark-until=1"],
            vec!["--gdb=:0"],
            vec!["--control=:0"],
            vec!["--profile-live-audio=1"],
            vec!["--calibrate-gamepad"],
            vec!["--list-monitors"],
            vec!["--list-midi"],
            vec!["--list-serial-ports"],
            vec!["--list-audio-devices"],
            vec!["--list-net-interfaces"],
            vec!["--list-disks"],
            vec!["--sampler-list-audio-inputs"],
            vec!["--list-drive-profiles"],
            vec!["--net-helper-status"],
        ] {
            assert!(!error_dialog_allowed(true, &parse(&args)), "{args:?}");
        }
    }
}
