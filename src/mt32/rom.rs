// SPDX-License-Identifier: GPL-3.0-or-later

//! What Copperline reads out of the control ROM for itself.
//!
//! The synthesis engine keeps its own copy of the parts it needs and hands
//! none of it back, so anything the panel wants to show about the image is
//! read from the file here. That is not a limitation worked around: the ROM
//! is on disk and Copperline put it there, so reading it is the direct way
//! round rather than the long one.

/// How wide the display is, and so how long the fields below are: the unit
/// writes them straight to the LCD, so they are already exactly one line.
const LINE: usize = super::LCD_WIDTH;

/// The version and date the control ROM names itself by.
///
/// Every image carries it as a ready-made display line -- `MT-32 v2.07
/// 90-05-23` on the later ROMs, `ver1.07 10 Oct, 87` on the earlier ones,
/// `CM32/LAPC1.02 891205` on a CM-32L -- so it is found rather than built:
/// the first twenty printable characters that read as a version.
///
/// `None` for an image with no such line, which is what a display that
/// cannot answer the question shows nothing for.
pub fn version_line(image: &[u8]) -> Option<String> {
    image
        .windows(LINE)
        .find(|w| w.iter().all(|&b| (0x20..0x7F).contains(&b)) && reads_as_version(w))
        .map(|w| String::from_utf8_lossy(w).trim().to_string())
}

/// Whether a run of characters carries a version field: a figure, a point,
/// then two more, which is how every one of them is written. `X` counts as
/// a figure -- the unreleased ROMs carry `verX.XX`, the field there and the
/// number never filled in.
fn reads_as_version(w: &[u8]) -> bool {
    let figure = |b: u8| b.is_ascii_digit() || b.eq_ignore_ascii_case(&b'X');
    w.windows(4)
        .any(|q| figure(q[0]) && q[1] == b'.' && figure(q[2]) && figure(q[3]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_line_is_found_where_the_rom_writes_one() {
        let mut image = vec![0u8; 0x400];
        image[0x100..0x114].copy_from_slice(b"MT-32 v2.07 90-05-23");
        assert_eq!(
            version_line(&image).as_deref(),
            Some("MT-32 v2.07 90-05-23")
        );
    }

    #[test]
    fn the_earlier_wording_is_found_too_and_comes_back_trimmed() {
        let mut image = vec![0u8; 0x400];
        image[0x80..0x94].copy_from_slice(b" ver1.07 10 Oct, 87 ");
        assert_eq!(version_line(&image).as_deref(), Some("ver1.07 10 Oct, 87"));
    }

    #[test]
    fn an_unreleased_rom_with_its_number_left_blank_still_answers() {
        let mut image = vec![0u8; 0x400];
        image[0x80..0x94].copy_from_slice(b"verX.XX  30 Sep, 88 ");
        assert_eq!(version_line(&image).as_deref(), Some("verX.XX  30 Sep, 88"));
    }

    #[test]
    fn text_without_a_version_in_it_is_not_mistaken_for_one() {
        let mut image = vec![0u8; 0x400];
        // Twenty printable characters, but nothing that reads as a version.
        image[0x40..0x54].copy_from_slice(b"MIDI Verify Error   ");
        assert_eq!(version_line(&image), None);
    }

    /// Every control ROM on the runner answers, and answers something that
    /// fits the display. Roland's images cannot be committed, so this looks
    /// for whatever pair the ROM-backed tests already use.
    #[test]
    #[ignore = "needs control ROMs in COPPERLINE_MT32_ROMS"]
    fn the_real_images_all_name_themselves() {
        let Some(dir) = std::env::var_os("COPPERLINE_MT32_ROMS").map(std::path::PathBuf::from)
        else {
            return;
        };
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir)
            .expect("the ROM directory opens")
            .flatten()
        {
            let path = entry.path();
            let name = path.to_string_lossy().to_lowercase();
            // Whatever the pair is called: a PCM image carries samples and
            // no such line, and a half image is one side of a control ROM.
            if !name.ends_with(".rom") || name.contains("pcm") {
                continue;
            }
            let image = std::fs::read(&path).expect("the image reads");
            if image.len() < 0x10000 {
                continue;
            }
            let line = version_line(&image);
            println!(
                "  {:<26} {line:?}",
                path.file_name().unwrap().to_string_lossy()
            );
            let line = line.expect("a control ROM names itself");
            assert!(line.len() <= super::LINE, "fits the display: {line:?}");
            seen += 1;
        }
        assert!(seen > 0, "no control ROMs found in {}", dir.display());
    }

    #[test]
    fn an_image_with_nothing_to_say_says_nothing() {
        assert_eq!(version_line(&[0u8; 0x400]), None);
        assert_eq!(version_line(&[]), None);
    }
}
