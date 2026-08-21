// SPDX-License-Identifier: GPL-3.0-or-later

//! Fetching the two archives WHDLoad staging needs.
//!
//! `tools/fetch-whdload.sh` does this at packaging time so a release
//! carries them; a build from source has neither until someone runs the
//! script. The Download button does the same work from inside the
//! launcher, from the same URLs and against the same pinned digests, so
//! nobody has to find a shell to get started.
//!
//! The digests are the point. Both archives are fetched over the network
//! from hosts nobody here controls, unpacked, and then *run on the
//! emulated machine*, so a file that is not the file expected is not
//! written at all. They are pinned in several places -- this module, the
//! script, the Flatpak manifest, the Homebrew formula -- and
//! [`tests::the_pinned_digests_match_the_fetch_script`] is what keeps this
//! copy honest with the script's.

use std::path::{Path, PathBuf};

/// One of the two archives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Archive {
    /// The WHDLoad distribution itself.
    Whdload,
    /// Soft-Kicker, whose `.RTB` relocation tables accompany raw Kickstart
    /// images.
    Skick,
}

impl Archive {
    pub const ALL: [Archive; 2] = [Archive::Whdload, Archive::Skick];

    /// Where it comes from.
    pub fn url(self) -> &'static str {
        match self {
            // whdload.de publishes this as a rolling "current release"
            // file, so a new upstream release changes the archive and the
            // digest below stops matching. That is the signal to review the
            // release and bump every pin, not to stop checking.
            Archive::Whdload => "https://whdload.de/whdload/WHDLoad_usr.lha",
            Archive::Skick => "https://aminet.net/util/boot/skick346.lha",
        }
    }

    /// What it must hash to.
    pub fn sha256(self) -> &'static str {
        match self {
            Archive::Whdload => "093333953737528d79c1eda7d21a16a0aa298698722624e7cfb31f588a0a156d",
            Archive::Skick => "02b4d01852d12ab391c6469064f917221a0f7319fd0b3ba6c359403ec1d59f96",
        }
    }

    /// The name it is saved under, which is the name it is published under.
    pub fn file_name(self) -> &'static str {
        self.url()
            .rsplit('/')
            .next()
            .expect("a url has a last part")
    }

    /// What the page calls it.
    pub fn label(self) -> &'static str {
        match self {
            Archive::Whdload => "WHDLoad package",
            Archive::Skick => "SKick package",
        }
    }

    /// Where it lands when nothing says otherwise.
    pub fn default_path(&self) -> Option<PathBuf> {
        crate::paths::whdload_support_dir().map(|dir| dir.join(self.file_name()))
    }

    /// The copy already sitting in the default place, if it is the right
    /// file. Checked by digest rather than by name: a truncated download or
    /// a differently-named file put there by hand is not this archive.
    pub fn found_locally(&self) -> Option<PathBuf> {
        let at = self.default_path()?;
        let bytes = std::fs::read(&at).ok()?;
        (sha256_hex(&bytes) == self.sha256()).then_some(at)
    }
}

/// What went wrong, in the words the page shows.
#[derive(Debug)]
pub enum Error {
    /// Could not work out where to put it.
    NoHome,
    /// Did not reach the host, or it refused.
    Fetch(String),
    /// Arrived, and was not the file expected.
    Digest,
    /// Reached the disk and did not stay there.
    Write(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoHome => write!(f, "No configuration directory to download into"),
            Error::Fetch(why) => write!(f, "Download failed ({why})"),
            Error::Digest => write!(
                f,
                "Download did not match its checksum, so it was discarded"
            ),
            Error::Write(why) => write!(f, "Could not save the download ({why})"),
        }
    }
}

impl std::error::Error for Error {}

/// Fetch an archive into the default place, and answer where it landed.
///
/// A copy already there and already right is left alone: this is the same
/// "up to date" the script reports rather than a fresh download every
/// press. Nothing is written until the digest matches, and the file only
/// takes its real name once it has.
pub fn download(archive: Archive) -> Result<PathBuf, Error> {
    if let Some(held) = archive.found_locally() {
        return Ok(held);
    }
    let at = archive.default_path().ok_or(Error::NoHome)?;
    let dir = at.parent().ok_or(Error::NoHome)?;
    std::fs::create_dir_all(dir).map_err(|e| Error::Write(e.to_string()))?;

    let bytes = fetch(archive.url())?;
    if sha256_hex(&bytes) != archive.sha256() {
        return Err(Error::Digest);
    }
    // Through a temporary, so an interrupted write cannot leave a partial
    // archive under the name staging will later trust.
    let temp = at.with_extension("lha.partial");
    std::fs::write(&temp, &bytes).map_err(|e| Error::Write(e.to_string()))?;
    match std::fs::rename(&temp, &at) {
        Ok(()) => Ok(at),
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(Error::Write(e.to_string()))
        }
    }
}

/// The most a support archive may be. WHDLoad's is around two megabytes
/// and Soft-Kicker's a few hundred kilobytes.
const MAX_ARCHIVE: u64 = 64 << 20;

fn fetch(url: &str) -> Result<Vec<u8>, Error> {
    use std::io::Read;
    // Longer than the API's timeout: this is a couple of megabytes over
    // somebody's home connection, not a request for a record.
    let agent = super::http::agent(std::time::Duration::from_secs(120));
    let mut response = agent
        .get(url)
        .call()
        .map_err(|e| Error::Fetch(short(&e.to_string())))?;
    // Bounded, and read through `take` rather than straight off the
    // reader: the digest is checked afterwards, so a wrong download is
    // caught either way, but only if there was room to finish reading it.
    // Both archives are a couple of megabytes; this is room to grow and
    // still far short of a machine's memory.
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_ARCHIVE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::Fetch(e.to_string()))?;
    if bytes.len() as u64 > MAX_ARCHIVE {
        return Err(Error::Fetch("archive is implausibly large".into()));
    }
    Ok(bytes)
}

fn short(why: &str) -> String {
    let first = why.split([':', ';']).next().unwrap_or(why).trim();
    first.chars().take(48).collect()
}

/// SHA-256, as the lower-case hex the pins are written in. The
/// implementation lives in [`crate::hash`] so builds without the game
/// library (player builds pinning their payload) share it; re-exported
/// here so the pins' callers keep their name.
pub use crate::hash::sha256_hex;

/// The Kickstart directory to look in when none is set: WHDLoad's own,
/// which is where a person who never chose one would have put them.
pub fn default_kickstart_dir() -> Option<PathBuf> {
    crate::paths::whdload_dir().map(|dir| dir.join("Kickstarts"))
}

/// Whether a directory exists and holds anything, so a default worth
/// adopting can be told from one that is merely named.
pub fn holds_anything(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pinned_digests_match_the_fetch_script() {
        // The same two archives are pinned here, in tools/fetch-whdload.sh,
        // in the Flatpak manifest and in the Homebrew formula. This one is
        // the copy a running Copperline checks its download against, so it
        // is the one most worth catching adrift.
        let script = include_str!("../../tools/fetch-whdload.sh");
        for archive in Archive::ALL {
            assert!(
                script.contains(archive.url()),
                "{:?}: the script fetches a different URL",
                archive
            );
            assert!(
                script.contains(archive.sha256()),
                "{:?}: the script pins a different checksum -- upstream released \\
                 a new version and this copy was not bumped with it",
                archive
            );
        }
    }

    #[test]
    fn an_archive_is_named_by_what_it_is_published_as() {
        assert_eq!(Archive::Whdload.file_name(), "WHDLoad_usr.lha");
        assert_eq!(Archive::Skick.file_name(), "skick346.lha");
    }
}
