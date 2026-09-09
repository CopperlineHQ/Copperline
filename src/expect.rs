// SPDX-License-Identifier: GPL-3.0-or-later

//! Scheduled screenshot expectations (`--expect-screenshot SECS PATH
//! [TOLERANCE]`).
//!
//! At SECS of emulated time the run captures the presented frame through
//! the very code path `--screenshot-after` saves through, so an expected
//! image made with `--screenshot-after` compares pixel for pixel, and
//! checks it against the PNG at PATH. A mismatch (or a missing or
//! differently sized expected image) prints one diagnostic line, writes
//! the captured frame as `<stem>.actual.png` next to the expected file
//! and, when the sizes agree, a red-on-black mask of the differing pixels
//! as `<stem>.diff.png`, and makes the run exit with
//! [`EXIT_STATUS_MISMATCH`] once every other scheduled piece of work has
//! finished. Blessing a new expectation is renaming the `.actual.png`.
//!
//! Only the colour channels are compared; alpha is always opaque in a
//! screenshot and is ignored.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Process exit status when at least one screenshot expectation failed
/// (and nothing with a stronger claim on the status, such as a guest
/// return code, applies; see [`crate::verdict::RunVerdict`]).
pub const EXIT_STATUS_MISMATCH: i32 = 3;

/// How many differing pixels an expectation tolerates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tolerance {
    /// Every pixel must match.
    Exact,
    /// At most this fraction (0.0-1.0) of the pixels may differ.
    Fraction(f64),
    /// At most this many pixels may differ.
    Pixels(u64),
}

impl Tolerance {
    /// Parse the optional TOLERANCE token: a number with a decimal point
    /// or exponent is a fraction of the frame (`0.001`); a plain integer
    /// is an absolute pixel count (`250`). Anything else is not a
    /// tolerance (so a following positional argument is left alone).
    pub fn parse(token: &str) -> Option<Self> {
        let token = token.trim();
        if token.is_empty() || token.starts_with('-') || token.starts_with('+') {
            return None;
        }
        if token.chars().all(|c| c.is_ascii_digit()) {
            return token.parse().ok().map(Tolerance::Pixels);
        }
        if !token
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '-' | '+'))
        {
            return None;
        }
        let value: f64 = token.parse().ok()?;
        (value.is_finite() && (0.0..=1.0).contains(&value)).then_some(Tolerance::Fraction(value))
    }

    /// Whether `differing` of `total` pixels is within this tolerance.
    pub fn accepts(self, differing: u64, total: u64) -> bool {
        match self {
            Tolerance::Exact => differing == 0,
            Tolerance::Pixels(max) => differing <= max,
            Tolerance::Fraction(max) => total == 0 || (differing as f64) / (total as f64) <= max,
        }
    }
}

impl std::fmt::Display for Tolerance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tolerance::Exact => write!(f, "exact"),
            Tolerance::Fraction(v) => write!(f, "{v}"),
            Tolerance::Pixels(n) => write!(f, "{n} px"),
        }
    }
}

/// One `--expect-screenshot` occurrence.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpectShotSpec {
    pub secs: f32,
    pub path: PathBuf,
    pub tolerance: Tolerance,
}

/// Where a failed expectation writes the captured frame.
pub fn actual_path(expected: &Path) -> PathBuf {
    sibling(expected, "actual")
}

/// Where a failed same-size expectation writes the difference mask.
pub fn diff_path(expected: &Path) -> PathBuf {
    sibling(expected, "diff")
}

fn sibling(expected: &Path, tag: &str) -> PathBuf {
    let stem = expected
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "expected".to_string());
    expected.with_file_name(format!("{stem}.{tag}.png"))
}

/// A decoded image in the framebuffer's pixel format (RGBA8 in memory
/// order, one `u32` per pixel).
pub struct Image {
    pub pixels: Vec<u32>,
    pub width: u32,
    pub height: u32,
}

/// Decode a PNG to the framebuffer's pixel format. Palette, greyscale and
/// 16-bit sources are normalised, so a hand-edited expectation is taken
/// as readily as one `--screenshot-after` wrote.
pub fn load_png(path: &Path) -> Result<Image> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .with_context(|| format!("decoding {}", path.display()))?;
    let size = reader
        .output_buffer_size()
        .with_context(|| format!("{}: image dimensions overflow", path.display()))?;
    let mut buf = vec![0u8; size];
    let info = reader
        .next_frame(&mut buf)
        .with_context(|| format!("decoding {}", path.display()))?;
    buf.truncate(info.buffer_size());
    let pack = |r: u8, g: u8, b: u8, a: u8| u32::from_ne_bytes([r, g, b, a]);
    let pixels: Vec<u32> = match info.color_type {
        png::ColorType::Rgba => buf
            .chunks_exact(4)
            .map(|c| pack(c[0], c[1], c[2], c[3]))
            .collect(),
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .map(|c| pack(c[0], c[1], c[2], 255))
            .collect(),
        png::ColorType::Grayscale => buf.iter().map(|&g| pack(g, g, g, 255)).collect(),
        png::ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .map(|c| pack(c[0], c[0], c[0], c[1]))
            .collect(),
        other => anyhow::bail!("{}: unsupported colour type {other:?}", path.display()),
    };
    Ok(Image {
        pixels,
        width: info.width,
        height: info.height,
    })
}

/// The colour channels of a framebuffer pixel (alpha masked off).
#[inline]
fn rgb(px: u32) -> u32 {
    u32::from_ne_bytes({
        let mut b = px.to_ne_bytes();
        b[3] = 0;
        b
    })
}

/// What comparing two same-sized images found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    pub differing: u64,
    pub total: u64,
    /// Inclusive bounding box `(x0, y0, x1, y1)` of the differing pixels,
    /// `None` when nothing differs.
    pub bbox: Option<(u32, u32, u32, u32)>,
    /// Red where the pixels differ, black elsewhere; the diff mask.
    pub mask: Vec<u32>,
}

/// Compare two images of `width` x `height` pixels by colour.
pub fn compare(actual: &[u32], expected: &[u32], width: u32, height: u32) -> Comparison {
    let w = width as usize;
    let total = w * height as usize;
    debug_assert!(actual.len() >= total && expected.len() >= total);
    let red = u32::from_ne_bytes([255, 0, 0, 255]);
    let black = u32::from_ne_bytes([0, 0, 0, 255]);
    let mut mask = vec![black; total];
    let mut differing = 0u64;
    let mut bbox: Option<(u32, u32, u32, u32)> = None;
    for y in 0..height as usize {
        for x in 0..w {
            let i = y * w + x;
            if rgb(actual[i]) == rgb(expected[i]) {
                continue;
            }
            differing += 1;
            mask[i] = red;
            let (x, y) = (x as u32, y as u32);
            bbox = Some(match bbox {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            });
        }
    }
    Comparison {
        differing,
        total: total as u64,
        bbox,
        mask,
    }
}

/// The result of checking one expectation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub passed: bool,
    /// The one-line diagnostic (also printed to stderr on failure).
    pub message: String,
}

/// Check the captured frame `actual` (`width` x `height`) against `spec`,
/// writing the actual image and the diff mask on failure. Failures to
/// write those files are reported in the message but never mask the
/// verdict.
pub fn check(spec: &ExpectShotSpec, actual: &[u32], width: u32, height: u32) -> Outcome {
    let expected_path = &spec.path;
    let shown = expected_path.display();
    let fail = |message: String| {
        eprintln!("expect-screenshot: {message}");
        Outcome {
            passed: false,
            message,
        }
    };
    let write_actual = || -> String {
        let path = actual_path(expected_path);
        match crate::screenshot::save(&path, &actual[..(width * height) as usize], width, height) {
            Ok(()) => format!("wrote {}", path.display()),
            Err(e) => format!("could not write {}: {e:#}", path.display()),
        }
    };
    if !expected_path.is_file() {
        return fail(format!(
            "{shown}: MISSING expected image; {} (rename it to bless)",
            write_actual()
        ));
    }
    let expected = match load_png(expected_path) {
        Ok(img) => img,
        Err(e) => return fail(format!("{shown}: UNREADABLE ({e:#}); {}", write_actual())),
    };
    if expected.width != width || expected.height != height {
        return fail(format!(
            "{shown}: SIZE MISMATCH expected {}x{}, captured {width}x{height}; {}",
            expected.width,
            expected.height,
            write_actual()
        ));
    }
    let cmp = compare(actual, &expected.pixels, width, height);
    if spec.tolerance.accepts(cmp.differing, cmp.total) {
        let message = format!(
            "{shown}: OK ({} of {} pixels differ, tolerance {})",
            cmp.differing, cmp.total, spec.tolerance
        );
        log::info!("expect-screenshot: {message}");
        return Outcome {
            passed: true,
            message,
        };
    }
    let (x0, y0, x1, y1) = cmp.bbox.expect("a mismatch has a bounding box");
    let diff = diff_path(expected_path);
    let wrote_diff = match crate::screenshot::save(&diff, &cmp.mask, width, height) {
        Ok(()) => format!("wrote {}", diff.display()),
        Err(e) => format!("could not write {}: {e:#}", diff.display()),
    };
    fail(format!(
        "{shown}: MISMATCH {} of {} pixels differ ({:.3}%), bounding box ({x0},{y0})-({x1},{y1}), tolerance {}; {}; {wrote_diff}",
        cmp.differing,
        cmp.total,
        100.0 * cmp.differing as f64 / cmp.total.max(1) as f64,
        spec.tolerance,
        write_actual(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lha::tests::temp_dir;

    fn px(r: u8, g: u8, b: u8) -> u32 {
        u32::from_ne_bytes([r, g, b, 255])
    }

    #[test]
    fn tolerance_token_grammar() {
        assert_eq!(Tolerance::parse("0.001"), Some(Tolerance::Fraction(0.001)));
        assert_eq!(Tolerance::parse("1.0"), Some(Tolerance::Fraction(1.0)));
        assert_eq!(Tolerance::parse("1e-3"), Some(Tolerance::Fraction(0.001)));
        assert_eq!(Tolerance::parse("0"), Some(Tolerance::Pixels(0)));
        assert_eq!(Tolerance::parse("250"), Some(Tolerance::Pixels(250)));
        assert_eq!(Tolerance::parse("1.5"), None, "a fraction above 1");
        assert_eq!(Tolerance::parse("-1"), None);
        assert_eq!(Tolerance::parse("KICK13.ROM"), None);
        assert_eq!(Tolerance::parse("--noaudio"), None);
        assert_eq!(Tolerance::parse(""), None);
    }

    #[test]
    fn tolerance_accepts_by_count_or_fraction() {
        assert!(Tolerance::Exact.accepts(0, 100));
        assert!(!Tolerance::Exact.accepts(1, 100));
        assert!(Tolerance::Pixels(5).accepts(5, 100));
        assert!(!Tolerance::Pixels(5).accepts(6, 100));
        assert!(Tolerance::Fraction(0.05).accepts(5, 100));
        assert!(!Tolerance::Fraction(0.05).accepts(6, 100));
        assert!(Tolerance::Fraction(0.0).accepts(0, 0));
    }

    #[test]
    fn sibling_paths_keep_the_stem_beside_the_expectation() {
        let p = Path::new("/tmp/shots/menu.png");
        assert_eq!(actual_path(p), Path::new("/tmp/shots/menu.actual.png"));
        assert_eq!(diff_path(p), Path::new("/tmp/shots/menu.diff.png"));
        assert_eq!(actual_path(Path::new("menu")), Path::new("menu.actual.png"));
    }

    #[test]
    fn compare_counts_colour_differences_and_ignores_alpha() {
        let expected = vec![px(1, 2, 3); 12]; // 4x3
        let mut actual = expected.clone();
        actual[1] = px(9, 2, 3); // (1,0)
        actual[10] = px(1, 2, 0); // (2,2)
        actual[5] = u32::from_ne_bytes([1, 2, 3, 0]); // alpha only: same colour
        let cmp = compare(&actual, &expected, 4, 3);
        assert_eq!(cmp.differing, 2);
        assert_eq!(cmp.total, 12);
        assert_eq!(cmp.bbox, Some((1, 0, 2, 2)));
        assert_eq!(cmp.mask[1], u32::from_ne_bytes([255, 0, 0, 255]));
        assert_eq!(cmp.mask[0], u32::from_ne_bytes([0, 0, 0, 255]));
        assert_eq!(cmp.mask.iter().filter(|&&m| m != cmp.mask[0]).count(), 2);

        let same = compare(&expected, &expected, 4, 3);
        assert_eq!(same.differing, 0);
        assert_eq!(same.bbox, None);
    }

    #[test]
    fn check_passes_within_tolerance_and_writes_nothing() {
        let dir = temp_dir("expect-pass");
        let expected = dir.join("frame.png");
        let pixels = vec![px(10, 20, 30); 6];
        crate::screenshot::save(&expected, &pixels, 3, 2).unwrap();
        let mut actual = pixels.clone();
        actual[0] = px(0, 0, 0);
        let spec = ExpectShotSpec {
            secs: 1.0,
            path: expected.clone(),
            tolerance: Tolerance::Pixels(1),
        };
        let outcome = check(&spec, &actual, 3, 2);
        assert!(outcome.passed, "{}", outcome.message);
        assert!(!actual_path(&expected).exists());
        assert!(!diff_path(&expected).exists());

        // The same frame fails an exact expectation and leaves both files.
        let exact = ExpectShotSpec {
            tolerance: Tolerance::Exact,
            ..spec
        };
        let outcome = check(&exact, &actual, 3, 2);
        assert!(!outcome.passed);
        assert!(outcome.message.contains("MISMATCH 1 of 6 pixels"));
        assert!(outcome.message.contains("(0,0)-(0,0)"));
        let written = load_png(&actual_path(&expected)).unwrap();
        assert_eq!((written.width, written.height), (3, 2));
        assert_eq!(written.pixels, actual);
        let mask = load_png(&diff_path(&expected)).unwrap();
        assert_eq!(mask.pixels[0], px(255, 0, 0));
        assert_eq!(mask.pixels[1], px(0, 0, 0));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn check_reports_missing_and_mismatched_size_and_keeps_the_actual() {
        let dir = temp_dir("expect-missing");
        let expected = dir.join("absent.png");
        let actual = vec![px(1, 1, 1); 4];
        let spec = ExpectShotSpec {
            secs: 0.5,
            path: expected.clone(),
            tolerance: Tolerance::Exact,
        };
        let outcome = check(&spec, &actual, 2, 2);
        assert!(!outcome.passed);
        assert!(outcome.message.contains("MISSING"), "{}", outcome.message);
        assert!(actual_path(&expected).is_file(), "blessable actual written");
        assert!(!diff_path(&expected).exists());

        // Blessing is a rename; a different size then fails clearly.
        std::fs::rename(actual_path(&expected), &expected).unwrap();
        let outcome = check(&spec, &actual, 2, 2);
        assert!(outcome.passed, "{}", outcome.message);
        let wide = vec![px(1, 1, 1); 6];
        let outcome = check(&spec, &wide, 3, 2);
        assert!(!outcome.passed);
        assert!(
            outcome
                .message
                .contains("SIZE MISMATCH expected 2x2, captured 3x2"),
            "{}",
            outcome.message
        );
        assert!(actual_path(&expected).is_file());
        std::fs::remove_dir_all(dir).ok();
    }
}
