// SPDX-License-Identifier: GPL-3.0-or-later

//! Coppersynth as one of the devices Paula's MIDI output
//! can be pointed at.
//!
//! Everything that makes sound lives in the Coppersynth engine (a
//! soundfont player with an MT-32 translation layer in front); this module
//! is the glue that finds a soundfont, runs the engine at the mixer's
//! rate, and hands frames to the same line-mix seam the MT-32 uses. Like
//! the MT-32 it exists only while it is the selected output: nothing is
//! loaded or rendered until then, and picking another device drops it.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
pub use coppersynth::mt32::translator::Mt32Mode;
pub use coppersynth::panel::{Button, FrontPanel, Pair, PanelRequest, Screen};

/// How many frames the engine is asked for at a time.
const BLOCK_FRAMES: usize = 256;

/// The soundfont's canonical filename, for the search path and messages.
pub const SOUNDFONT_NAME: &str = "GeneralUser-GS.sf2";

/// The zipped spelling of a beside-the-executable override, read with
/// the archive support Copperline already carries. The default bank
/// needs no file at all: Coppersynth embeds its own.
pub const SOUNDFONT_ZIP_NAME: &str = "GeneralUser-GS.zip";

/// The `[serial] coppersynth_*` settings the device is fitted with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CsynthOptions {
    /// An explicit soundfont; unset means the search path below.
    pub soundfont: Option<PathBuf>,
    /// MT-32 mode: auto (default), on, off.
    pub mt32_mode: Option<String>,
}

impl CsynthOptions {
    pub fn mode(&self) -> Result<Mt32Mode> {
        match self.mt32_mode.as_deref().map(str::trim) {
            None => Ok(Mt32Mode::Auto),
            Some(s) if s.eq_ignore_ascii_case("auto") => Ok(Mt32Mode::Auto),
            Some(s) if s.eq_ignore_ascii_case("on") => Ok(Mt32Mode::On),
            Some(s) if s.eq_ignore_ascii_case("off") => Ok(Mt32Mode::Off),
            Some(other) => Err(anyhow!(
                "[serial] coppersynth_mt32_mode must be \"auto\", \"on\", or \"off\", got {other:?}"
            )),
        }
    }
}

/// Which soundfont overrides the bundled bank, if any: `[serial] coppersynth_soundfont`
/// first, then `COPPERLINE_SOUNDFONT`, then a file placed beside the
/// executable or in the `share/copperline` layout an installed package
/// uses. `None` means Coppersynth's own bundled GeneralUser GS -- there
/// is always a bank to play.
pub fn find_soundfont(explicit: Option<&std::path::Path>) -> Result<Option<PathBuf>> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(Some(path.to_path_buf()));
        }
        return Err(anyhow!(
            "[serial] coppersynth_soundfont {} does not exist",
            path.display()
        ));
    }
    if let Some(dir) = crate::envcfg::var_os("COPPERLINE_SOUNDFONT") {
        let p = PathBuf::from(dir);
        // The variable may name the file itself or a directory holding it.
        let p = if p.is_dir() {
            p.join(SOUNDFONT_NAME)
        } else {
            p
        };
        if p.is_file() {
            return Ok(Some(p));
        }
        return Err(anyhow!(
            "COPPERLINE_SOUNDFONT names {}, which does not exist",
            p.display()
        ));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // The zipped spelling wins when both are present; either
            // works in every location.
            for dir in [
                dir.to_path_buf(),
                dir.join("share").join("copperline"),
                dir.join("..").join("share").join("copperline"),
            ] {
                for name in [SOUNDFONT_ZIP_NAME, SOUNDFONT_NAME] {
                    let candidate = dir.join(name);
                    if candidate.is_file() {
                        return Ok(Some(candidate));
                    }
                }
            }
        }
    }
    Ok(None)
}

/// Build the engine from a soundfont file, unpacking a zipped one on the
/// way in: the first `.sf2` entry is the bank, whatever it is called.
fn open_engine(path: &std::path::Path, mode: Mt32Mode) -> Result<coppersynth::engine::Engine> {
    let zipped = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"));
    if zipped {
        let file = std::fs::File::open(path).map_err(|e| anyhow!("{}: {e}", path.display()))?;
        let mut archive =
            zip::ZipArchive::new(file).map_err(|e| anyhow!("{}: {e}", path.display()))?;
        let entry = (0..archive.len())
            .find(|&i| {
                archive
                    .by_index(i)
                    .is_ok_and(|f| f.name().to_ascii_lowercase().ends_with(".sf2"))
            })
            .ok_or_else(|| anyhow!("{}: no .sf2 inside", path.display()))?;
        let mut reader = archive
            .by_index(entry)
            .map_err(|e| anyhow!("{}: {e}", path.display()))?;
        return coppersynth::engine::Engine::open_reader(
            &mut reader,
            crate::audio::MIX_SAMPLE_RATE,
            mode,
        )
        .map_err(|e| anyhow!("{}: {e}", path.display()));
    }
    coppersynth::engine::Engine::open(path, crate::audio::MIX_SAMPLE_RATE, mode)
        .map_err(|e| anyhow!("{e}"))
}

/// Coppersynth attached to the MIDI output.
pub struct CsynthDevice {
    engine: coppersynth::engine::Engine,
    /// The block last rendered, and how much of it the mixer has taken.
    block: Vec<(f32, f32)>,
    played: usize,
    /// Whether translation was reported active, for the one-line log when
    /// auto mode identifies MT-32 traffic.
    translating_logged: bool,
    /// A raw tap of every byte the guest sends, when
    /// `COPPERLINE_COPPERSYNTH_CAPTURE` names a file: the capture replays offline
    /// through the translation layer, which is how a play-through becomes
    /// a regression corpus. Flushed often enough to survive a force-quit.
    capture: Option<(std::io::BufWriter<std::fs::File>, usize)>,
    /// The front panel's own state machine, which is Coppersynth's: the
    /// window draws glass and forwards presses, and every character
    /// shown is composed by the library.
    panel: FrontPanel,
}

impl CsynthDevice {
    /// Fit the synthesizer with the given options.
    pub fn open(options: &CsynthOptions) -> Result<Self> {
        let mode = options.mode()?;
        let soundfont = find_soundfont(options.soundfont.as_deref())?;
        let engine = match &soundfont {
            Some(path) => open_engine(path, mode)?,
            // Nothing configured: the bank Coppersynth carries itself.
            None => coppersynth::engine::Engine::open_bundled(crate::audio::MIX_SAMPLE_RATE, mode)
                .map_err(|e| anyhow!("{e}"))?,
        };
        let (mended, dropped) = engine.bank_repairs();
        let source = soundfont
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| format!("bundled {}", engine.bank_name()));
        if mended + dropped > 0 {
            // The bank arrived bruised; say so once, since it plays on.
            log::warn!(
                "midi: {source} needed repair: {mended} loops defused, {dropped} regions dropped"
            );
        }
        log::info!(
            "midi: Coppersynth attached ({}, {source}, MT-32 mode {})",
            coppersynth::version(),
            match mode {
                Mt32Mode::Auto => "auto",
                Mt32Mode::On => "on",
                Mt32Mode::Off => "off",
            }
        );
        let capture = crate::envcfg::var_os("COPPERLINE_COPPERSYNTH_CAPTURE").and_then(|path| {
            let path = PathBuf::from(path);
            match std::fs::File::create(&path) {
                Ok(file) => {
                    log::info!("midi: capturing GM bytes to {}", path.display());
                    Some((std::io::BufWriter::new(file), 0))
                }
                Err(e) => {
                    log::warn!(
                        "midi: COPPERLINE_COPPERSYNTH_CAPTURE {}: {e}",
                        path.display()
                    );
                    None
                }
            }
        });
        Ok(Self {
            engine,
            block: vec![(0.0, 0.0); BLOCK_FRAMES],
            played: BLOCK_FRAMES,
            translating_logged: mode == Mt32Mode::On,
            capture,
            panel: FrontPanel::default(),
        })
    }

    /// Take a byte off the serial line.
    pub fn write_byte(&mut self, b: u8) {
        if let Some((w, since_flush)) = &mut self.capture {
            use std::io::Write;
            let _ = w.write_all(&[b]);
            *since_flush += 1;
            // MIDI runs at 31250 baud, so this is at most ~12 flushes a
            // second and usually far fewer; cheap insurance against a
            // capture lost to a force-quit.
            if *since_flush >= 256 {
                *since_flush = 0;
                let _ = w.flush();
            }
        }
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

    // --- the front panel -------------------------------------------------

    /// Buttons held through the power-on, read as the unit reads its
    /// fascia at start-up.
    pub fn panel_power_on_held(&mut self, held: &[Button]) {
        self.panel.power_on_held(held);
    }

    /// A semantic press from the window's panel.
    pub fn panel_button(&mut self, button: Button) -> Option<PanelRequest> {
        self.feed_panel();
        self.panel.button(&mut self.engine, button)
    }

    /// The VOLUME knob, 0..=1.
    pub fn panel_volume(&mut self, value: f32) {
        self.panel.volume(&mut self.engine, value);
    }

    /// Where the knob stands, for drawing it.
    pub fn panel_volume_value(&self) -> f32 {
        self.engine.output_gain()
    }

    /// The glass, composed by the library.
    pub fn panel_screen(&mut self, now_ms: u64) -> Screen {
        self.feed_panel();
        self.panel.screen(&mut self.engine, now_ms)
    }

    /// Whether the monitor is on, for the blinking MUTE lamp.
    pub fn panel_monitoring(&self) -> bool {
        self.engine.monitor() != coppersynth::engine::Monitor::Off
    }

    /// Switch MT-32 mode live -- the menu's route to what the fascia's
    /// own prompt does. The panel reads the engine, so it follows.
    pub fn set_mt32_mode(&mut self, mode: Mt32Mode) {
        self.engine.set_mt32_mode(mode);
        self.translating_logged = mode == Mt32Mode::On;
    }

    /// Letters and pictures the engine took off the wire go to the
    /// panel before it is asked anything.
    fn feed_panel(&mut self) {
        for feed in self.engine.take_panel_feed() {
            self.panel.feed(feed);
        }
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
        let options = CsynthOptions {
            soundfont: Some(sf),
            mt32_mode: Some("off".to_string()),
        };
        let render = || {
            let mut device = CsynthDevice::open(&options).expect("device opens");
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

    /// A zipped bank is the same instrument: the archive path renders
    /// byte-identically to the flat file it wraps.
    #[test]
    fn a_zipped_soundfont_opens() {
        let Some(sf) = soundfont() else { return };
        let zip_path = sf.with_file_name("test-GeneralUser-GS.zip");
        {
            let file = std::fs::File::create(&zip_path).expect("zip creates");
            let mut writer = zip::ZipWriter::new(file);
            // Stored, not deflated: the test exercises the archive path,
            // not the compressor, and skips a multi-second deflate.
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored)
                .large_file(true);
            writer.start_file(SOUNDFONT_NAME, options).expect("entry");
            std::io::copy(
                &mut std::fs::File::open(&sf).expect("soundfont opens"),
                &mut writer,
            )
            .expect("entry written");
            writer.finish().expect("zip finishes");
        }
        let render = |path: PathBuf| {
            let options = CsynthOptions {
                soundfont: Some(path),
                mt32_mode: Some("off".to_string()),
            };
            let mut device = CsynthDevice::open(&options).expect("device opens");
            for b in [0xC0u8, 0x00, 0x90, 60, 100] {
                device.write_byte(b);
            }
            let mut frames = Vec::with_capacity(4410);
            for _ in 0..4410 {
                frames.push(device.next_frame());
            }
            frames
        };
        assert_eq!(render(zip_path), render(sf));
    }

    /// MT-32 translation in front of the same synth: an MT-32 patch
    /// number arrives as its GM instrument.
    #[test]
    fn the_translation_layer_sits_in_the_path() {
        let Some(sf) = soundfont() else { return };
        let options = CsynthOptions {
            soundfont: Some(sf),
            mt32_mode: Some("on".to_string()),
        };
        let mut device = CsynthDevice::open(&options).expect("device opens");
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
