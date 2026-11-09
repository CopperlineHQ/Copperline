//! Exercise package listing, selective reads, full LHA reads and streaming
//! extraction in fresh temporary directories. No panics are caught.

use copperline::{archive::Limits, lha, package};

pub fn exercise(data: &[u8]) {
    // Deliberately smaller than the production limits: a valid compressed
    // run of zeros must reach a budget error quickly, not starve a campaign.
    let limits = Limits {
        member_bytes: 64 * 1024,
        total_bytes: 256 * 1024,
        entries: 128,
    };
    let dir = tempfile::tempdir().expect("create archive fuzz directory");
    for extension in ["lha", "zip"] {
        let path = dir.path().join(format!("input.{extension}"));
        std::fs::write(&path, data).expect("write archive fuzz input");
        let _ = package::read_first_with_limits(&path, |_| true, limits);
        if let Ok(members) = package::list_with_limits(&path, limits) {
            for member in members.iter().take(2) {
                let _ = package::read_with_limits(&path, member, limits);
            }
        }
        let out = dir.path().join(format!("out-{extension}"));
        let _ = package::extract_to_dir_with_limits(&path, &out, limits);
        if extension == "lha" {
            let _ = lha::read_archive_with_limits(&path, limits);
        }
    }
}
