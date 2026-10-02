//! The About panel's Check for updates: start the check, collect its
//! answer, open the release page, and say in the panel's footer where
//! things stand.
//!
//! Nothing is asked until the button is pressed, and the answer lives for
//! the session only: reopening About shows it again, and quitting forgets
//! it. See `crate::update` for the request itself.

use std::sync::mpsc::{Receiver, TryRecvError};

use super::App;
use crate::update::{self, Release};
use crate::video::about::{FooterTone, UpdateFooter};

/// Where the session's update check stands.
pub(super) enum UpdateCheck {
    /// Not asked this session.
    Idle,
    /// A worker is asking GitHub.
    Asking(Receiver<Result<Release, update::Error>>),
    /// The latest release is this build's, or older (a development build
    /// can be ahead of every release).
    Current(Release),
    /// A newer release is out.
    Newer(Release),
    /// A newer release is out, and the browser would not take its page.
    /// Whether its address went to the host clipboard instead.
    Unopened { release: Release, copied: bool },
    /// No answer worth having.
    Failed(update::Error),
}

impl App {
    /// The footer button: ask, or open the page of what asking found.
    pub(super) fn about_update_pressed(&mut self) {
        match &self.update_check {
            UpdateCheck::Asking(_) => {}
            UpdateCheck::Newer(release) | UpdateCheck::Unopened { release, .. } => {
                let release = release.clone();
                self.open_release_page(release);
            }
            UpdateCheck::Idle | UpdateCheck::Current(_) | UpdateCheck::Failed(_) => {
                log::info!("update: asking GitHub for the latest release");
                self.update_check = UpdateCheck::Asking(update::spawn_check());
            }
        }
    }

    /// Collect the worker's answer once it has one.
    pub(super) fn poll_update_check(&mut self) {
        let UpdateCheck::Asking(rx) = &self.update_check else {
            return;
        };
        let answer = match rx.try_recv() {
            Ok(answer) => answer,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                Err(update::Error::Unreachable("the check stopped".into()))
            }
        };
        self.update_check = settle(answer, &update::running_version());
        self.request_redraw();
    }

    /// Hand the release page to the browser; failing that, put its
    /// address on the clipboard so it can be pasted into one.
    fn open_release_page(&mut self, release: Release) {
        let page = release.page();
        match update::open_in_browser(&page) {
            Ok(()) => self.update_check = UpdateCheck::Newer(release),
            Err(e) => {
                log::warn!("update: could not open {page}: {e}");
                let copied = self
                    .host_clipboard()
                    .is_some_and(|board| board.set_text(page.clone()).is_ok());
                self.update_check = UpdateCheck::Unopened { release, copied };
            }
        }
    }

    /// The footer the About panel draws for where the check stands.
    pub(super) fn about_update_footer(&self) -> UpdateFooter {
        footer(&self.update_check)
    }
}

/// What an answer from the worker leaves the check at.
fn settle(answer: Result<Release, update::Error>, running: &semver::Version) -> UpdateCheck {
    match answer {
        Ok(release) => {
            log::info!("update: latest release {}, running {running}", release.tag);
            if release.newer_than(running) {
                UpdateCheck::Newer(release)
            } else {
                UpdateCheck::Current(release)
            }
        }
        Err(e) => {
            log::warn!("update: {e}");
            UpdateCheck::Failed(e)
        }
    }
}

fn footer(check: &UpdateCheck) -> UpdateFooter {
    let (status, tone, button, enabled) = match check {
        UpdateCheck::Idle => (
            "Asks GitHub whether a newer release is out".to_string(),
            FooterTone::Quiet,
            "Check for updates",
            true,
        ),
        UpdateCheck::Asking(_) => (
            "Asking GitHub...".to_string(),
            FooterTone::Quiet,
            "Checking...",
            false,
        ),
        UpdateCheck::Current(release) => (
            format!("Up to date: the latest release is {}", release.version),
            FooterTone::Quiet,
            "Check again",
            true,
        ),
        UpdateCheck::Newer(release) => (
            format!("Copperline {} is available", release.version),
            FooterTone::News,
            "Open release page",
            true,
        ),
        UpdateCheck::Unopened { release, copied } => (
            if *copied {
                format!(
                    "No browser opened; the address of the {} page is on the clipboard",
                    release.version
                )
            } else {
                format!(
                    "No browser opened; {} is on GitHub's Copperline releases page",
                    release.version
                )
            },
            FooterTone::Trouble,
            "Open release page",
            true,
        ),
        UpdateCheck::Failed(e) => (
            format!("Update check failed: {e}"),
            FooterTone::Trouble,
            "Try again",
            true,
        ),
    };
    UpdateFooter {
        status,
        tone,
        button,
        enabled,
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_app;
    use super::*;

    fn release(text: &str) -> Release {
        Release {
            tag: format!("v{text}"),
            version: semver::Version::parse(text).unwrap(),
        }
    }

    #[test]
    fn an_answer_settles_into_current_newer_or_failed() {
        let running = semver::Version::parse("1.0.0-rc.1").unwrap();
        assert!(matches!(
            settle(Ok(release("1.0.0")), &running),
            UpdateCheck::Newer(r) if r.tag == "v1.0.0"
        ));
        assert!(matches!(
            settle(Ok(release("1.0.0-rc.1")), &running),
            UpdateCheck::Current(_)
        ));
        assert!(matches!(
            settle(Err(update::Error::RateLimited), &running),
            UpdateCheck::Failed(update::Error::RateLimited)
        ));
    }

    #[test]
    fn the_footer_says_where_the_check_stands() {
        let idle = footer(&UpdateCheck::Idle);
        assert_eq!(idle.button, "Check for updates");
        assert!(idle.enabled);

        let (_tx, rx) = std::sync::mpsc::channel();
        let asking = footer(&UpdateCheck::Asking(rx));
        assert!(!asking.enabled, "a check that is out cannot be asked twice");

        let current = footer(&UpdateCheck::Current(release("1.0.0")));
        assert_eq!(current.status, "Up to date: the latest release is 1.0.0");
        assert_eq!(current.tone, FooterTone::Quiet);

        let newer = footer(&UpdateCheck::Newer(release("1.0.1")));
        assert_eq!(newer.status, "Copperline 1.0.1 is available");
        assert_eq!(newer.button, "Open release page");
        assert_eq!(newer.tone, FooterTone::News);

        let failed = footer(&UpdateCheck::Failed(update::Error::Http(502)));
        assert_eq!(
            failed.status,
            "Update check failed: GitHub answered HTTP 502"
        );
        assert_eq!(failed.button, "Try again");
        assert_eq!(failed.tone, FooterTone::Trouble);
    }

    #[test]
    fn a_finished_check_is_collected_and_shown() {
        let mut app = test_app();
        let (tx, rx) = std::sync::mpsc::channel();
        app.update_check = UpdateCheck::Asking(rx);
        app.poll_update_check();
        assert!(
            matches!(app.update_check, UpdateCheck::Asking(_)),
            "nothing has arrived yet"
        );
        tx.send(Ok(release("999.0.0"))).unwrap();
        app.poll_update_check();
        assert!(matches!(app.update_check, UpdateCheck::Newer(_)));
        assert_eq!(
            app.about_update_footer().status,
            "Copperline 999.0.0 is available"
        );
    }

    #[test]
    fn a_worker_that_dies_is_reported_not_waited_for() {
        let mut app = test_app();
        let (tx, rx) = std::sync::mpsc::channel::<Result<Release, update::Error>>();
        drop(tx);
        app.update_check = UpdateCheck::Asking(rx);
        app.poll_update_check();
        assert!(matches!(
            app.update_check,
            UpdateCheck::Failed(update::Error::Unreachable(_))
        ));
    }

    #[test]
    fn pressing_while_asking_asks_nothing_more() {
        let mut app = test_app();
        let (tx, rx) = std::sync::mpsc::channel();
        app.update_check = UpdateCheck::Asking(rx);
        app.about_update_pressed();
        // Still the same worker's channel: an answer sent on it lands.
        tx.send(Ok(release("0.1.0"))).unwrap();
        app.poll_update_check();
        assert!(matches!(app.update_check, UpdateCheck::Current(_)));
    }
}
