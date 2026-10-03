// SPDX-License-Identifier: GPL-3.0-or-later

//! The About panel's update check: ask GitHub which release is the latest,
//! and say whether it is newer than the build that is running.
//!
//! Nothing here runs unless the user presses Check for updates. The check is
//! one HTTPS GET to GitHub's public releases API carrying the request line,
//! an `Accept` header and the `Copperline/<version>` user agent every
//! Copperline request carries (`crate::http`) -- no identifier, no setting,
//! and nothing remembered between runs. The privacy policy
//! (copperline.dev/privacy) describes this request; a change to what it
//! sends changes that page in the same change.
//!
//! The release page offered afterwards is built here from the tag, never
//! taken from the reply: a URL handed to the host browser comes from
//! Copperline, not from whatever answered.

use std::cmp::Ordering;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

/// GitHub's latest-release endpoint: the newest release that is neither a
/// draft nor marked as a pre-release.
const LATEST_RELEASE: &str = "https://api.github.com/repos/CopperlineHQ/Copperline/releases/latest";
/// Where a release's page lives, by tag.
const RELEASE_PAGE: &str = "https://github.com/CopperlineHQ/Copperline/releases/tag/";
/// Long enough for a slow link, short enough that a dead one is reported
/// while the user is still looking at the panel.
const TIMEOUT: Duration = Duration::from_secs(15);
/// The most of a reply worth reading. A release record, its notes and
/// asset list included, is tens of kilobytes.
const MAX_REPLY: u64 = 1 << 20;

/// The newest published release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The tag as published, e.g. `v1.0.0`.
    pub tag: String,
    /// The tag's version, for comparing and for showing.
    pub version: semver::Version,
}

impl Release {
    /// Whether this release is newer than `running`, by semver precedence:
    /// a pre-release sorts before its release (`1.0.0-rc.1` < `1.0.0`) and
    /// build metadata counts for nothing.
    pub fn newer_than(&self, running: &semver::Version) -> bool {
        self.version.cmp_precedence(running) == Ordering::Greater
    }

    /// The release's page on GitHub, where its notes and downloads are.
    pub fn page(&self) -> String {
        format!("{RELEASE_PAGE}{}", self.tag)
    }
}

/// Why a check found nothing out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No answer: offline, a name that would not resolve, TLS, or the
    /// timeout. The transport's reason, in a few words.
    Unreachable(String),
    /// GitHub turned the request away for now. Its API allows each address
    /// a limited number of unauthenticated requests an hour, shared with
    /// anything else on the same network.
    RateLimited,
    /// Any other HTTP status.
    Http(u16),
    /// An answer that was not a release record whose tag is a version.
    Malformed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unreachable(why) => write!(f, "could not reach GitHub ({why})"),
            Error::RateLimited => f.write_str("GitHub asked to wait; try again later"),
            Error::Http(code) => write!(f, "GitHub answered HTTP {code}"),
            Error::Malformed(why) => write!(f, "unexpected reply from GitHub ({why})"),
        }
    }
}

impl std::error::Error for Error {}

/// This build's version, as Cargo has it: without the `+g<hash>` an
/// untagged build shows, which precedence would ignore anyway.
pub fn running_version() -> semver::Version {
    // Cargo refuses a package version that is not semver, so this cannot
    // fail for a build that got as far as compiling.
    semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("package version is semver")
}

/// Ask GitHub for the latest release. Blocks for up to [`TIMEOUT`]: call
/// it from a worker, or use [`spawn_check`].
pub fn latest_release() -> Result<Release, Error> {
    let reply = crate::http::agent(TIMEOUT)
        .get(LATEST_RELEASE)
        .header("Accept", "application/vnd.github+json")
        .call();
    let mut response = match reply {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(403 | 429)) => return Err(Error::RateLimited),
        Err(ureq::Error::StatusCode(code)) => return Err(Error::Http(code)),
        Err(e) => return Err(Error::Unreachable(reason(&e))),
    };
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_REPLY)
        .read_to_string()
        .map_err(|e| match e {
            ureq::Error::BodyExceedsLimit(_) => Error::Malformed("reply too large".into()),
            e => Error::Unreachable(reason(&e)),
        })?;
    parse_release(&body)
}

/// Run [`latest_release`] on a worker of its own. The answer arrives on the
/// returned channel; a channel that disconnects with nothing on it means
/// the worker died.
pub fn spawn_check() -> Receiver<Result<Release, Error>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(latest_release());
    });
    rx
}

/// The release a latest-release record describes.
fn parse_release(body: &str) -> Result<Release, Error> {
    #[derive(serde::Deserialize)]
    struct Record {
        tag_name: String,
    }
    let record: Record =
        serde_json::from_str(body).map_err(|e| Error::Malformed(short(&e.to_string())))?;
    let tag = record.tag_name;
    let bare = tag.strip_prefix('v').unwrap_or(&tag);
    let version = semver::Version::parse(bare)
        .map_err(|_| Error::Malformed(format!("tag {} is not a version", short(&tag))))?;
    // Parsing has already confined the tag to these characters; checked
    // again because the tag goes into a URL that is handed to a browser,
    // and that should not rest on another crate's grammar.
    if !tag
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
    {
        return Err(Error::Malformed("tag has stray characters".into()));
    }
    Ok(Release { tag, version })
}

/// Hand `url` to the host's default browser. Whether it took the page
/// arrives on the returned channel, an error as a few words to show.
///
/// On Windows the shell answers the call itself, so the answer is there
/// at once. Elsewhere it comes when the opener exits -- usually straight
/// after handing the page on, though `xdg-open` falling back to running a
/// browser itself waits for that browser to close. An opener that cannot
/// be started at all answers at once too.
pub fn open_in_browser(url: &str) -> Receiver<Result<(), String>> {
    let (tx, rx) = channel();
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain([0]).collect() };
        let (verb, file) = (wide("open"), wide(url));
        // SAFETY: both strings are NUL-terminated UTF-16 that outlive the
        // call; the window, parameters and directory may all be null.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // Above 32 is success; at or below, one of its error codes.
        let _ = tx.send(if result as usize > 32 {
            Ok(())
        } else {
            Err(format!("ShellExecuteW failed ({})", result as usize))
        });
    }
    #[cfg(not(windows))]
    {
        use std::process::{Command, Stdio};
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let spawned = Command::new(opener)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            // Waited for on a thread of its own, which also reaps it: a
            // child nobody waits for stays a zombie until Copperline quits.
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = tx.send(match child.wait() {
                        Ok(status) if status.success() => Ok(()),
                        Ok(status) => Err(format!("{opener} failed ({status})")),
                        Err(e) => Err(format!("{opener}: {e}")),
                    });
                });
            }
            Err(e) => {
                let _ = tx.send(Err(format!("{opener}: {e}")));
            }
        }
    }
    rx
}

/// A transport failure in a few words: the ones a person can act on by
/// name, anything else by its own description.
fn reason(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Timeout(_) => "timed out".into(),
        ureq::Error::HostNotFound => "host not found".into(),
        ureq::Error::ConnectionFailed => "connection failed".into(),
        // Without ureq's "io: " in front, which says nothing to a reader.
        ureq::Error::Io(io) => short(&io.to_string()),
        e => short(&e.to_string()),
    }
}

/// A reason as one short line of printable ASCII, for a small panel whose
/// font has nothing else to draw. What GitHub or the transport said is not
/// trusted to be short or plain.
fn short(why: &str) -> String {
    let plain: String = why
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .collect();
    let plain = plain.trim();
    if plain.len() <= 40 {
        return plain.to_string();
    }
    format!("{}...", plain[..40].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> semver::Version {
        semver::Version::parse(text).unwrap()
    }

    /// Trimmed from a real reply: the record carries far more than the tag,
    /// and everything else is ignored.
    const RECORD: &str = r#"{
        "url": "https://api.github.com/repos/CopperlineHQ/Copperline/releases/1",
        "html_url": "https://example.invalid/not-where-we-send-anyone",
        "tag_name": "v1.0.0-rc.1",
        "name": "Copperline 1.0.0-rc.1",
        "draft": false,
        "prerelease": false,
        "assets": [{"name": "Copperline-1.0.0-rc.1.dmg", "size": 123}],
        "body": "Release notes"
    }"#;

    #[test]
    fn a_release_record_gives_its_tag_and_version() {
        let release = parse_release(RECORD).unwrap();
        assert_eq!(release.tag, "v1.0.0-rc.1");
        assert_eq!(release.version, version("1.0.0-rc.1"));
    }

    #[test]
    fn a_tag_without_the_v_is_a_version_too() {
        let release = parse_release(r#"{"tag_name": "1.2.3"}"#).unwrap();
        assert_eq!(release.version, version("1.2.3"));
        assert_eq!(release.tag, "1.2.3");
    }

    #[test]
    fn the_page_is_built_from_the_tag_not_taken_from_the_reply() {
        let release = parse_release(RECORD).unwrap();
        assert_eq!(
            release.page(),
            "https://github.com/CopperlineHQ/Copperline/releases/tag/v1.0.0-rc.1"
        );
    }

    #[test]
    fn a_tag_that_is_not_a_version_is_refused() {
        for body in [
            r#"{"tag_name": "nightly"}"#,
            r#"{"tag_name": "v1.0"}"#,
            r#"{"tag_name": "v1.0.0 "}"#,
            r#"{"tag_name": "v1.0.0/../../evil"}"#,
            r#"{"tag_name": "v1.0.0?x=<script>"}"#,
            r#"{"tag_name": ""}"#,
            r#"{"name": "no tag at all"}"#,
            "not json",
            "",
        ] {
            assert!(
                matches!(parse_release(body), Err(Error::Malformed(_))),
                "{body:?} should be refused"
            );
        }
    }

    #[test]
    fn newer_follows_release_precedence() {
        let release = |text: &str| Release {
            tag: format!("v{text}"),
            version: version(text),
        };
        // (latest published, running, whether the latest is newer)
        for (latest, running, newer) in [
            ("1.0.0", "1.0.0-rc.1", true),
            ("1.0.0-rc.2", "1.0.0-rc.1", true),
            ("1.0.0-rc.10", "1.0.0-rc.9", true),
            ("1.0.1", "1.0.0", true),
            ("1.1.0", "1.0.9", true),
            ("1.0.0-rc.1", "0.21.0", true),
            ("1.0.0-rc.1", "1.0.0-rc.1", false),
            ("1.0.0", "1.0.0", false),
            // A development build ahead of the latest release.
            ("1.0.0", "1.1.0", false),
            ("1.0.0-rc.1", "1.0.0", false),
        ] {
            assert_eq!(
                release(latest).newer_than(&version(running)),
                newer,
                "{latest} newer than {running}"
            );
        }
        // Build metadata says which commit, not which is newer.
        assert!(!release("1.0.0").newer_than(&version("1.0.0+g1234abcd")));
    }

    #[test]
    fn the_running_version_is_a_version() {
        assert_eq!(running_version().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn a_reason_is_cut_to_one_short_printable_line() {
        assert_eq!(short(" connection refused "), "connection refused");
        assert_eq!(short("caf\u{e9}\nbar"), "cafbar");
        let long = "x".repeat(100);
        assert_eq!(short(&long), format!("{}...", "x".repeat(40)));
    }

    #[test]
    fn a_transport_failure_is_named_in_plain_words() {
        assert_eq!(reason(&ureq::Error::HostNotFound), "host not found");
        let refused = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        assert_eq!(reason(&ureq::Error::Io(refused)), "refused");
    }

    #[test]
    fn every_failure_reads_as_a_line() {
        for (error, text) in [
            (
                Error::Unreachable("timeout".into()),
                "could not reach GitHub (timeout)",
            ),
            (Error::RateLimited, "GitHub asked to wait; try again later"),
            (Error::Http(502), "GitHub answered HTTP 502"),
            (
                Error::Malformed("eof".into()),
                "unexpected reply from GitHub (eof)",
            ),
        ] {
            assert_eq!(error.to_string(), text);
        }
    }

    /// The real request, against the real API:
    ///
    /// ```sh
    /// cargo test --release --lib update_live -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "talks to api.github.com"]
    fn update_live_latest_release() {
        let release = latest_release().expect("GitHub answers");
        println!(
            "latest {} ({}), running {}, newer: {}",
            release.tag,
            release.page(),
            running_version(),
            release.newer_than(&running_version())
        );
    }
}
