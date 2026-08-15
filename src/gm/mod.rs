// SPDX-License-Identifier: GPL-3.0-or-later

//! The General MIDI synthesizer as one of the devices Paula's MIDI output
//! can be pointed at.
//!
//! Everything that makes sound lives in the `copperline-gm` engine (a
//! soundfont player with an MT-32 translation layer in front); this module
//! is the glue that finds a soundfont, runs the engine at the mixer's
//! rate, and hands frames to the same line-mix seam the MT-32 uses. Like
//! the MT-32 it exists only while it is the selected output: nothing is
//! loaded or rendered until then, and picking another device drops it.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
pub use copperline_gm::mt32::translator::Mt32Mode;

/// How many frames the engine is asked for at a time.
const BLOCK_FRAMES: usize = 256;

/// The soundfont's canonical filename, for the search path and messages.
pub const SOUNDFONT_NAME: &str = "GeneralUser-GS.sf2";

/// The `[gm]` settings the device is fitted with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GmOptions {
    /// An explicit soundfont; unset means the search path below.
    pub soundfont: Option<PathBuf>,
    /// MT-32 translation: auto (default), on, off.
    pub mt32_translation: Option<String>,
}

impl GmOptions {
    pub fn mode(&self) -> Result<Mt32Mode> {
        match self.mt32_translation.as_deref().map(str::trim) {
            None => Ok(Mt32Mode::Auto),
            Some(s) if s.eq_ignore_ascii_case("auto") => Ok(Mt32Mode::Auto),
            Some(s) if s.eq_ignore_ascii_case("on") => Ok(Mt32Mode::On),
            Some(s) if s.eq_ignore_ascii_case("off") => Ok(Mt32Mode::Off),
            Some(other) => Err(anyhow!(
                "[gm] mt32_translation must be \"auto\", \"on\", or \"off\", got {other:?}"
            )),
        }
    }
}

/// Where the soundfont is looked for when `[gm] soundfont` does not say:
/// `COPPERLINE_GM_SOUNDFONT`, then beside the executable, then the
/// `share/copperline` layout an installed package uses -- the same shape
/// the bundled AROS ROM resolves by.
pub fn find_soundfont(explicit: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(anyhow!("[gm] soundfont {} does not exist", path.display()));
    }
    if let Some(dir) = crate::envcfg::var_os("COPPERLINE_GM_SOUNDFONT") {
        let p = PathBuf::from(dir);
        // The variable may name the file itself or a directory holding it.
        let p = if p.is_dir() {
            p.join(SOUNDFONT_NAME)
        } else {
            p
        };
        if p.is_file() {
            return Ok(p);
        }
        return Err(anyhow!(
            "COPPERLINE_GM_SOUNDFONT names {}, which does not exist",
            p.display()
        ));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in [
                dir.join(SOUNDFONT_NAME),
                dir.join("share").join("copperline").join(SOUNDFONT_NAME),
                dir.join("..")
                    .join("share")
                    .join("copperline")
                    .join(SOUNDFONT_NAME),
            ] {
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }
    Err(anyhow!(
        "no {SOUNDFONT_NAME} found: set [gm] soundfont, COPPERLINE_GM_SOUNDFONT, \
         or put the file beside the executable (tools/fetch-gm-soundfont.sh)"
    ))
}

/// A General MIDI synthesizer attached to the MIDI output.
pub struct GmDevice {
    engine: copperline_gm::engine::GmEngine,
    /// The block last rendered, and how much of it the mixer has taken.
    block: Vec<(f32, f32)>,
    played: usize,
    /// Whether translation was reported active, for the one-line log when
    /// auto mode identifies MT-32 traffic.
    translating_logged: bool,
}

impl GmDevice {
    /// Fit the synthesizer with the given options.
    pub fn open(options: &GmOptions) -> Result<Self> {
        let mode = options.mode()?;
        let soundfont = find_soundfont(options.soundfont.as_deref())?;
        let engine =
            copperline_gm::engine::GmEngine::open(&soundfont, crate::audio::MIX_SAMPLE_RATE, mode)
                .map_err(|e| anyhow!("{e}"))?;
        log::info!(
            "midi: General MIDI attached ({}, {}, mt32 translation {})",
            copperline_gm::version(),
            soundfont.display(),
            match mode {
                Mt32Mode::Auto => "auto",
                Mt32Mode::On => "on",
                Mt32Mode::Off => "off",
            }
        );
        Ok(Self {
            engine,
            block: vec![(0.0, 0.0); BLOCK_FRAMES],
            played: BLOCK_FRAMES,
            translating_logged: mode == Mt32Mode::On,
        })
    }

    /// Take a byte off the serial line.
    pub fn write_byte(&mut self, b: u8) {
        self.engine.write_byte(b);
        if !self.translating_logged && self.engine.translating() {
            self.translating_logged = true;
            log::info!("midi: MT-32 traffic identified; translating to General MIDI");
        }
    }

    /// The next rendered frame, in emulated time.
    pub fn next_frame(&mut self) -> (f32, f32) {
        if self.played == self.block.len() {
            self.engine.render(&mut self.block);
            self.played = 0;
        }
        let frame = self.block[self.played];
        self.played += 1;
        frame
    }

    /// Display lines the guest wrote to the "MT-32", oldest first.
    pub fn take_display(&mut self) -> Vec<String> {
        self.engine.take_display()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn soundfont() -> Option<PathBuf> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(SOUNDFONT_NAME);
        p.is_file().then_some(p)
    }

    /// The whole audible path: bytes in off the wire, frames out of the
    /// mixer seam, sound actually present, and the same bytes twice
    /// rendering byte-identically. Skips without the local soundfont,
    /// like the ignored suites.
    #[test]
    fn a_note_makes_a_deterministic_sound() {
        let Some(sf) = soundfont() else { return };
        let options = GmOptions {
            soundfont: Some(sf),
            mt32_translation: Some("off".to_string()),
        };
        let render = || {
            let mut device = GmDevice::open(&options).expect("device opens");
            for b in [0xC0u8, 0x00, 0x90, 60, 100] {
                device.write_byte(b);
            }
            let mut frames = Vec::with_capacity(4410);
            for _ in 0..4410 {
                frames.push(device.next_frame());
            }
            frames
        };
        let a = render();
        let rms = (a.iter().map(|(l, r)| l * l + r * r).sum::<f32>() / a.len() as f32).sqrt();
        assert!(rms > 0.001, "a note-on must make sound, rms {rms}");
        let b = render();
        assert_eq!(a, b, "the audible path must be deterministic");
    }

    /// MT-32 translation in front of the same synth: an MT-32 patch
    /// number arrives as its GM instrument.
    #[test]
    fn the_translation_layer_sits_in_the_path() {
        let Some(sf) = soundfont() else { return };
        let options = GmOptions {
            soundfont: Some(sf),
            mt32_translation: Some("on".to_string()),
        };
        let mut device = GmDevice::open(&options).expect("device opens");
        // MT-32 patch 12 (Pipe Org 1) plus a note; the church organ that
        // comes out is the translation working end to end.
        for b in [0xC0u8, 12, 0x90, 48, 100] {
            device.write_byte(b);
        }
        let mut loud = 0f32;
        for _ in 0..4410 {
            let (l, r) = device.next_frame();
            loud += l * l + r * r;
        }
        assert!(loud > 0.0, "the translated note must sound");
    }
}
