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
