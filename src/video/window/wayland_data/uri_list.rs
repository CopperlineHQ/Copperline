//! `text/uri-list` decoding for dropped files (RFC 2483 lists of RFC 8089
//! `file:` URIs). Only the Wayland receiver calls it, but it is plain byte
//! handling, so it builds and is tested on every Unix host.
#![cfg_attr(
    not(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )),
    allow(dead_code)
)]

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

/// The local paths in a `text/uri-list` payload, in order.
pub(super) fn parse_uri_list(data: &[u8]) -> Vec<PathBuf> {
    uri_list_paths(data, local_hostname().as_deref())
}

/// The paths in a URI list whose `file:` URIs name `local_host` (or no
/// host). Comment lines, blank lines and every other URI are skipped.
fn uri_list_paths(data: &[u8], local_host: Option<&[u8]>) -> Vec<PathBuf> {
    data.split(|&b| b == b'\n')
        .map(|line| line.trim_ascii())
        .filter(|line| !line.is_empty() && !line.starts_with(b"#"))
        .filter_map(|uri| file_uri_path(uri, local_host))
        .collect()
}

/// Decode a `file:` URI (RFC 8089) to a path: `file:///p`, `file:/p`,
/// `file://localhost/p`, or `file://<this host>/p`. A URI naming another
/// host, any other scheme, or a path that does not decode is refused.
fn file_uri_path(uri: &[u8], local_host: Option<&[u8]>) -> Option<PathBuf> {
    let scheme = uri.get(..5)?;
    if !scheme.eq_ignore_ascii_case(b"file:") {
        return None;
    }
    let rest = &uri[5..];
    let path = if let Some(authority_and_path) = rest.strip_prefix(b"//") {
        let slash = authority_and_path.iter().position(|&b| b == b'/')?;
        let host = &authority_and_path[..slash];
        let local = host.is_empty()
            || host.eq_ignore_ascii_case(b"localhost")
            || local_host.is_some_and(|name| host.eq_ignore_ascii_case(name));
        if !local {
            return None;
        }
        &authority_and_path[slash..]
    } else if rest.starts_with(b"/") {
        rest
    } else {
        return None;
    };
    // A query or fragment is not part of the file's name; senders encode a
    // literal '?' or '#' in a name as %3F and %23.
    let end = path
        .iter()
        .position(|&b| b == b'?' || b == b'#')
        .unwrap_or(path.len());
    let decoded = percent_decode(&path[..end])?;
    if decoded.contains(&0) {
        return None;
    }
    Some(PathBuf::from(OsString::from_vec(decoded)))
}

/// Undo %XX escapes; a '%' not followed by two hex digits is malformed.
fn percent_decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' {
            let hi = hex_digit(*input.get(i + 1)?)?;
            let lo = hex_digit(*input.get(i + 2)?)?;
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(input[i]);
            i += 1;
        }
    }
    Some(out)
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn local_hostname() -> Option<Vec<u8>> {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most buf.len() bytes into buf.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return None;
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    (len > 0).then(|| buf[..len].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_list_decodes_local_file_uris_in_order() {
        let list = b"# dragged from a file manager\r\n\
            file:///home/amiga/Games/Turrican%20II.adf\r\n\
            file://localhost/tmp/disk%231.adf\r\n\
            file:/tmp/plain.lha\r\n";
        assert_eq!(
            uri_list_paths(list, None),
            vec![
                PathBuf::from("/home/amiga/Games/Turrican II.adf"),
                PathBuf::from("/tmp/disk#1.adf"),
                PathBuf::from("/tmp/plain.lha"),
            ]
        );
    }

    #[test]
    fn uri_list_accepts_lf_only_lines_and_utf8_escapes() {
        let list = b"file:///tmp/a.adf\nfile:///tmp/%C3%A9t%C3%A9.hdf\n";
        assert_eq!(
            uri_list_paths(list, None),
            vec![
                PathBuf::from("/tmp/a.adf"),
                PathBuf::from("/tmp/\u{e9}t\u{e9}.hdf")
            ]
        );
    }

    #[test]
    fn uri_list_keeps_this_host_and_refuses_others() {
        let list = b"file://amiga-box/tmp/here.adf\r\nfile://elsewhere/tmp/there.adf\r\n";
        assert_eq!(
            uri_list_paths(list, Some(b"AMIGA-BOX")),
            vec![PathBuf::from("/tmp/here.adf")]
        );
    }

    #[test]
    fn uri_list_skips_other_schemes_and_malformed_uris() {
        let list = b"https://example.com/game.lha\r\n\
            trash:///old.adf\r\n\
            file:relative.adf\r\n\
            file:///tmp/bad%2.adf\r\n\
            file:///tmp/nul%00.adf\r\n\
            FILE:///tmp/upper.adf\r\n";
        assert_eq!(
            uri_list_paths(list, None),
            vec![PathBuf::from("/tmp/upper.adf")]
        );
    }

    #[test]
    fn uri_list_drops_query_and_fragment() {
        assert_eq!(
            file_uri_path(b"file:///tmp/game.lha?x=1#frag", None),
            Some(PathBuf::from("/tmp/game.lha"))
        );
    }

    #[test]
    fn parse_uri_list_reads_a_whole_payload() {
        assert_eq!(
            parse_uri_list(b"file:///tmp/one.adf\r\nfile:///tmp/two.adf\r\n"),
            vec![PathBuf::from("/tmp/one.adf"), PathBuf::from("/tmp/two.adf")]
        );
    }
}
