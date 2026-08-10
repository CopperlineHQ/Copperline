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

/// SHA-256, as the lower-case hex the pins are written in.
///
/// Written out rather than taken from a crate: it is forty lines, it is
/// used for exactly these two files, and the alternative is a dependency
/// in the tree of a program that otherwise hashes nothing.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let mut message = data.to_vec();
    let bits = (data.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_be_bytes());

    for block in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes(word.try_into().expect("4 bytes"));
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, add) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(add);
        }
    }
    h.iter().map(|word| format!("{word:08x}")).collect()
}

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
    fn sha256_matches_the_published_vectors() {
        // The three every implementation is checked against, plus the
        // block-boundary lengths where the padding is easiest to get wrong.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // 55, 56 and 64 bytes: either side of the point where the length
        // no longer fits in the block it padded, and exactly one block.
        assert_eq!(
            sha256_hex(&[b'a'; 55]),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            sha256_hex(&[b'a'; 56]),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        assert_eq!(
            sha256_hex(&[b'a'; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

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
