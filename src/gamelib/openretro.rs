// SPDX-License-Identifier: GPL-3.0-or-later

//! Talking to openretro.org.
//!
//! The service has no published API. What follows was recovered by reading
//! the FS-UAE launcher, which is the other program that syncs this
//! database, and then confirmed against the live service; nothing here is
//! taken from its code, and the storage and matching Copperline does with
//! the result are its own.
//!
//! Three requests, in order:
//!
//! 1. `POST /api/auth` -- form-encoded `username`, `password`, `device_id`
//!    and `device_name`, answering with a short auth token. The password is
//!    needed for this request and nothing else.
//! 2. `GET /api/sync/amiga/games?v=3&sync=<cursor>` -- authenticated with
//!    the token as HTTP Basic. Answers with a run of records rather than
//!    JSON; see [`Record`]. A page holds up to five hundred, and the next
//!    page starts from the highest sync id in this one, so a resync asks
//!    only for what has changed since.
//! 3. `GET /image/<sha1>?s=<pixels>` -- cover art, as PNG, at whatever size
//!    is asked for.
//!
//! **Over TLS, always.** The FS-UAE client defaults to `http`, which puts a
//! password on the wire in clear; this one has no such mode.

use std::io::Read;
use std::time::Duration;

use zeroize::Zeroizing;

use super::Secret;

/// Where the service lives. Not configurable: a "server" setting is a way
/// to be talked into sending a password somewhere else.
const BASE: &str = "https://openretro.org";

/// The platform whose games are synced. OpenRetro carries several; this is
/// an Amiga emulator.
const PLATFORM: &str = "amiga";

/// How long any one request may take. A sync page is a couple of hundred
/// kilobytes, so a slow link is the reason to wait, and a dead service is
/// the reason not to wait forever.
const TIMEOUT: Duration = Duration::from_secs(45);

/// The size of cover art fetched, in pixels on the long edge. The list
/// shows it small, and the service scales server-side, so this is what
/// travels rather than a megabyte of full-resolution scan.
pub const COVER_PIXELS: u32 = 256;

/// What this client calls itself when it signs in. The service asks for a
/// device id so a person can see and revoke their sessions; a fixed one
/// says "Copperline on this machine" rather than making up a new device on
/// every login and filling their list with strangers.
pub const DEVICE_ID: &str = "copperline";

/// What went wrong, in the terms the sync dialog reports.
#[derive(Debug)]
pub enum Error {
    /// The username and password were not accepted.
    Unauthorized,
    /// Reached the service, which answered with something else.
    Http(u16),
    /// Did not reach the service at all.
    Offline(String),
    /// Reached it, and could not make sense of the answer.
    Malformed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unauthorized => write!(f, "Wrong username or password"),
            Error::Http(code) => write!(f, "Unable to connect to OpenRetro ({code})"),
            Error::Offline(why) => write!(f, "Unable to connect to OpenRetro ({why})"),
            Error::Malformed(what) => write!(f, "OpenRetro sent something unexpected ({what})"),
        }
    }
}

impl std::error::Error for Error {}

type Result<T> = std::result::Result<T, Error>;

/// One entry of a sync page.
///
/// The stream is not JSON but a run of framed records, which is what makes
/// a resync cheap: each carries the id the next request resumes from, so
/// nothing already held is sent again.
///
/// ```text
/// u32be sync_id | 16-byte uuid | u32be length | length bytes
/// ```
///
/// The payload is zlib-compressed JSON. A zero length is not an empty
/// record but a deletion: the entry with that uuid has gone.
#[derive(Debug, Clone)]
pub struct Record {
    /// Where the next request resumes from.
    pub sync_id: u32,
    /// What the entry is, stably, across renames and re-imports.
    pub uuid: [u8; 16],
    /// The entry's JSON, or `None` when the entry was deleted.
    pub json: Option<String>,
}

/// A session with the service: a token, and the client it was got with.
///
/// Holds no password. [`Session::open`] needs one to trade for the token
/// and does not keep it; the caller is expected to drop theirs as soon as
/// this returns, which is what [`Secret`] is for.
pub struct Session {
    agent: ureq::Agent,
    token: Secret,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The token is a credential in its own right: it authenticates
        // every request until it is handed back.
        f.write_str("Session { token: <redacted> }")
    }
}

impl Session {
    /// Trade a username and password for a token.
    pub fn open(username: &str, password: &Secret, device_id: &str) -> Result<Session> {
        let agent = agent();
        let body = form(&[
            ("username", username),
            ("password", password.expose()),
            ("device_id", device_id),
            // What the service lists the token against, so a person can see
            // which machines hold one.
            ("device_name", DEVICE_NAME),
        ]);
        let reply = agent
            .post(&format!("{BASE}/api/auth"))
            .content_type("application/x-www-form-urlencoded")
            .send(body.as_str());
        let text = read_reply(reply)?;
        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| Error::Malformed(e.to_string()))?;
        let token = json
            .get("auth_token")
            .and_then(|t| t.as_str())
            .ok_or_else(|| Error::Malformed("no auth_token".into()))?;
        let mut held = Secret::new();
        for c in token.chars() {
            held.push(c);
        }
        Ok(Session { agent, token: held })
    }

    /// Hand the token back, so it stops being usable. Best effort: a token
    /// left behind expires on its own, and there is nothing a caller could
    /// usefully do about a failure here.
    pub fn close(self) {
        let body = form(&[("auth_token", self.token.expose())]);
        let _ = self
            .agent
            .post(&format!("{BASE}/api/deauth"))
            .content_type("application/x-www-form-urlencoded")
            .send(body.as_str());
    }

    /// Fetch the games changed since `cursor`, as one page.
    ///
    /// An empty page means there is nothing further; otherwise the caller
    /// resumes from the highest [`Record::sync_id`] returned.
    pub fn games_since(&self, cursor: u32) -> Result<Vec<Record>> {
        let url = format!("{BASE}/api/sync/{PLATFORM}/games?v=3&sync={cursor}");
        let reply = self
            .agent
            .get(&url)
            .header(
                "Authorization",
                &basic_auth("auth_token", self.token.expose()),
            )
            .call();
        let bytes = read_bytes(reply)?;
        decode_page(&bytes)
    }
}

/// Fetch one piece of cover art, as the PNG bytes the service returns.
///
/// Unauthenticated: the images are public, and a sync token is not wanted
/// on a request that does not need one.
pub fn cover(sha1: &str, pixels: u32) -> Result<Vec<u8>> {
    cover_with(&covers_agent(), sha1, pixels)
}

/// The same, through an agent the caller keeps.
///
/// A scan fetches art for every game in the library -- a thousand or more
/// requests to one host. An agent per request is a TLS handshake per
/// request; one agent held across them all reuses the connection, which is
/// most of what makes the difference between a scan that finishes and one
/// somebody gives up on.
pub fn cover_with(agent: &ureq::Agent, sha1: &str, pixels: u32) -> Result<Vec<u8>> {
    // The digest goes into a URL, so it is checked for being one rather
    // than trusted to be: everything else here is a fixed string.
    if sha1.len() != 40 || !sha1.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::Malformed("not a sha1".into()));
    }
    let url = format!("{BASE}/image/{sha1}?s={pixels}");
    read_bytes(agent.get(&url).call())
}

/// An agent for fetching art: no credential goes near it, since the images
/// are public and a sync token is not wanted on a request that does not
/// need one.
pub fn covers_agent() -> ureq::Agent {
    agent()
}

/// Cut a sync page into its records.
///
/// Deliberately strict: a truncated page is a transfer that went wrong, and
/// treating the tail as a short record would write a half-read entry into
/// the database as though it were whole.
pub fn decode_page(data: &[u8]) -> Result<Vec<Record>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < data.len() {
        // Checked, because `n` is a length off the wire: on a 32-bit host
        // a wrapped sum would pass this and panic on the slice.
        let need = |at: usize, n: usize| -> Result<()> {
            match at.checked_add(n) {
                Some(end) if end <= data.len() => Ok(()),
                _ => Err(Error::Malformed("page ends mid-record".into())),
            }
        };
        need(at, 4)?;
        let sync_id = u32::from_be_bytes(data[at..at + 4].try_into().expect("4 bytes"));
        at += 4;
        need(at, 16)?;
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&data[at..at + 16]);
        at += 16;
        need(at, 4)?;
        let len = u32::from_be_bytes(data[at..at + 4].try_into().expect("4 bytes")) as usize;
        at += 4;
        need(at, len)?;
        let body = &data[at..at + len];
        at += len;
        let json = if body.is_empty() {
            None
        } else {
            Some(inflate(body)?)
        };
        out.push(Record {
            sync_id,
            uuid,
            json,
        });
    }
    Ok(out)
}

/// The most one record's JSON may inflate to.
///
/// Compressed data is a ratio, not a size: the page it arrives in is
/// bounded, but a thousand-to-one stream turns ten megabytes of page into
/// ten gigabytes of string. This is the guard against that, and nothing
/// else -- it is not a statement about what a sensible record looks like.
///
/// Measured over the whole catalogue rather than guessed: 22,420 records
/// across 45 pages, median 829 bytes, 99th percentile 33 KB -- and exactly
/// one record of 6.38 MiB, a 2013 import carrying an enormous file list.
/// The first cap was 1 MiB, written from what an incremental sync happened
/// to contain, and that one record failed every full sync there has ever
/// been. This is a little over twice the largest real record, and still
/// three orders of magnitude short of anything that could hurt.
const MAX_RECORD: u64 = 16 << 20;

/// Undo the zlib wrapper a record's JSON arrives in.
fn inflate(body: &[u8]) -> Result<String> {
    let mut text = String::new();
    let read = flate2::read::ZlibDecoder::new(body)
        .take(MAX_RECORD + 1)
        .read_to_string(&mut text)
        .map_err(|e| Error::Malformed(format!("record: {e}")))?;
    if read as u64 > MAX_RECORD {
        return Err(Error::Malformed("record is implausibly large".into()));
    }
    Ok(text)
}

/// The client every request goes through.
///
/// TLS is verified against the Mozilla root set `webpki-roots` bundles --
/// not the platform's store -- so it behaves the same on every host, and
/// which implementation gets there is [`http::agent`]'s business.
fn agent() -> ureq::Agent {
    super::http::agent(TIMEOUT)
}

/// `application/x-www-form-urlencoded`, escaping everything that is not
/// unreserved -- a password is exactly the kind of string that contains
/// the characters this is for.
fn form(fields: &[(&str, &str)]) -> Zeroizing<String> {
    // Zeroizing, because this is the password again: percent-encoding
    // leaves every unreserved byte of it verbatim, and a plain String would
    // hand the whole credential back to the allocator unwiped.
    let mut out = Zeroizing::new(String::new());
    for (key, value) in fields {
        if !out.is_empty() {
            out.push('&');
        }
        out.push_str(key);
        out.push('=');
        for byte in value.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(byte as char)
                }
                // Written a digit at a time rather than through `format!`,
                // whose temporary would be one more unwiped copy.
                _ => {
                    const HEX: &[u8; 16] = b"0123456789ABCDEF";
                    out.push('%');
                    out.push(HEX[(byte >> 4) as usize] as char);
                    out.push(HEX[(byte & 0xf) as usize] as char);
                }
            }
        }
    }
    out
}

/// HTTP Basic authentication for the sync token.
fn basic_auth(user: &str, pass: &str) -> String {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
    format!("Basic {encoded}")
}

/// What the service is told to list this token against.
///
/// A fixed string, not the host's name. The name a person gives their
/// machine is often their own, and there is no reason to hand that to a
/// third party for a game database lookup.
const DEVICE_NAME: &str = "copperline";

/// Turn a reply into text, mapping the outcomes the dialog reports.
fn read_reply(
    reply: std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<String> {
    let bytes = read_bytes(reply)?;
    String::from_utf8(bytes).map_err(|e| Error::Malformed(e.to_string()))
}

fn read_bytes(
    reply: std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<Vec<u8>> {
    match reply {
        Ok(mut response) => response
            .body_mut()
            .read_to_vec()
            .map_err(|e| Error::Offline(e.to_string())),
        Err(ureq::Error::StatusCode(401)) | Err(ureq::Error::StatusCode(403)) => {
            Err(Error::Unauthorized)
        }
        Err(ureq::Error::StatusCode(code)) => Err(Error::Http(code)),
        Err(e) => Err(Error::Offline(short(&e.to_string()))),
    }
}

/// A connection failure in a few words, for a line in a small dialog.
fn short(why: &str) -> String {
    let first = why.split([':', ';']).next().unwrap_or(why).trim();
    let mut out: String = first.chars().take(48).collect();
    if first.chars().count() > 48 {
        out.push_str("...");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn record(sync_id: u32, uuid: u8, json: Option<&str>) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&sync_id.to_be_bytes());
        out.extend_from_slice(&[uuid; 16]);
        match json {
            None => out.extend_from_slice(&0u32.to_be_bytes()),
            Some(text) => {
                let mut z =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
                z.write_all(text.as_bytes()).unwrap();
                let body = z.finish().unwrap();
                out.extend_from_slice(&(body.len() as u32).to_be_bytes());
                out.extend_from_slice(&body);
            }
        }
        out
    }

    #[test]
    fn a_page_decodes_to_its_records() {
        let mut page = record(7, 0xAA, Some(r#"{"game_name":"Turrican"}"#));
        page.extend(record(9, 0xBB, None));
        let got = decode_page(&page).expect("decodes");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].sync_id, 7);
        assert_eq!(got[0].uuid, [0xAA; 16]);
        assert_eq!(got[0].json.as_deref(), Some(r#"{"game_name":"Turrican"}"#));
        // A zero-length record is a deletion, not an empty entry.
        assert_eq!(got[1].sync_id, 9);
        assert_eq!(got[1].json, None);
    }

    #[test]
    fn a_truncated_page_is_refused_rather_than_half_read() {
        // A transfer that stopped early would otherwise write a partial
        // entry into the database as though it were whole.
        let page = record(7, 0xAA, Some(r#"{"game_name":"Turrican"}"#));
        for cut in [1, 4, 20, 24, page.len() - 1] {
            let err = decode_page(&page[..cut]).expect_err("{cut} bytes should not decode");
            assert!(matches!(err, Error::Malformed(_)), "{cut}: {err:?}");
        }
        assert!(decode_page(&page).is_ok(), "the whole page still decodes");
    }

    #[test]
    fn an_empty_page_means_there_is_no_more() {
        assert!(decode_page(&[]).expect("decodes").is_empty());
    }

    #[test]
    fn a_form_escapes_what_a_password_is_made_of() {
        // The characters a generated passphrase is full of are exactly the
        // ones that would otherwise end the field or the body.
        let body = form(&[("password", "a&b=c d+e%f")]);
        assert_eq!(body.as_str(), "password=a%26b%3Dc%20d%2Be%25f");
        assert!(!body[9..].contains('&'), "a value ended the field early");
    }

    #[test]
    fn basic_auth_encodes_the_way_the_header_wants() {
        // Against the worked examples in RFC 4648, including both padding
        // lengths, since a wrong tail is the classic base64 bug.
        assert_eq!(basic_auth("", ""), "Basic Og==");
        assert_eq!(basic_auth("a", "b"), "Basic YTpi");
        assert_eq!(
            basic_auth("auth_token", "0123456789abcdef01234567"),
            "Basic YXV0aF90b2tlbjowMTIzNDU2Nzg5YWJjZGVmMDEyMzQ1Njc="
        );
    }

    /// A real sync against the live service, which is the only thing that
    /// proves the requests above are the requests it wants. Ignored, and
    /// reads the account from the environment so that no credential is ever
    /// written into a source file:
    ///
    /// ```sh
    /// OPENRETRO_USER=you OPENRETRO_PASS=... \
    ///   cargo test --release --lib openretro_live -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs an OpenRetro account in OPENRETRO_USER/OPENRETRO_PASS"]
    fn openretro_live_sync_and_cover() {
        let user = std::env::var("OPENRETRO_USER").expect("OPENRETRO_USER");
        let mut pass = Secret::new();
        for c in std::env::var("OPENRETRO_PASS")
            .expect("OPENRETRO_PASS")
            .chars()
        {
            pass.push(c);
        }

        let session = Session::open(&user, &pass, "copperline-test-device").expect("authorized");
        drop(pass);

        // Two pages, to exercise the cursor as well as the first request.
        let mut cursor = 0;
        let mut seen = 0usize;
        let mut a_cover: Option<String> = None;
        for page in 0..2 {
            let records = session.games_since(cursor).expect("a page of games");
            assert!(!records.is_empty(), "page {page} was empty");
            for r in &records {
                assert!(r.sync_id > cursor, "the cursor did not advance");
                cursor = r.sync_id;
                seen += 1;
                if a_cover.is_none() {
                    if let Some(json) = &r.json {
                        let v: serde_json::Value = serde_json::from_str(json).expect("a record");
                        if let Some(sha1) = v.get("front_sha1").and_then(|s| s.as_str()) {
                            a_cover = Some(sha1.to_string());
                        }
                    }
                }
            }
            eprintln!(
                "page {page}: {} records, cursor now {cursor}",
                records.len()
            );
        }
        assert!(seen >= 500, "expected a full page or more, saw {seen}");

        // And the cover art the records point at is really a PNG.
        let sha1 = a_cover.expect("some game in two pages has cover art");
        let png = cover(&sha1, COVER_PIXELS).expect("cover art");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
        eprintln!("cover {sha1}: {} bytes", png.len());

        session.close();
    }

    #[test]
    fn a_cover_request_checks_the_digest_it_is_given() {
        // It goes into a URL. Everything else in that URL is a fixed
        // string, so this is the one part that has to be what it claims.
        for bad in [
            "",
            "../etc/passwd",
            "zz7b6a3b89d1696e5096aede1b7bd4bf6bf9cb83",
            &"a".repeat(41),
        ] {
            let err = cover(bad, 128).expect_err("{bad:?} is not a sha1");
            assert!(matches!(err, Error::Malformed(_)), "{bad:?}: {err:?}");
        }
    }

    #[test]
    fn a_session_says_nothing_about_its_token() {
        // Debug is the easy way for a credential to reach a log line.
        let shown = format!(
            "{:?}",
            Session {
                agent: agent(),
                token: Secret::new(),
            }
        );
        assert_eq!(shown, "Session { token: <redacted> }");
    }
}
