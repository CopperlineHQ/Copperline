// SPDX-License-Identifier: GPL-3.0-or-later

//! The MT-32 as one of the devices Paula's MIDI output can be pointed at.
//!
//! A host endpoint makes its noise on the host; this one makes it here, so
//! the device is both where the bytes go and where the mixer pulls frames
//! from. It exists only while it is the selected output: nothing is loaded,
//! allocated or rendered until then, and picking another device drops it.

use super::Mt32Synth;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// How many frames the engine is asked for at a time.
const BLOCK_FRAMES: usize = 256;

/// An MT-32 attached to the MIDI output.
pub struct Mt32Device {
    synth: Mt32Synth,
    /// The block last rendered, and how much of it the mixer has taken.
    block: Vec<(f32, f32)>,
    played: usize,
    /// Bytes written since the last block. The engine's own parser wants
    /// them in order and copes with running status and split messages, so
    /// they are handed over whole rather than framed here.
    pending: Vec<u8>,
    /// A song out of the control ROM, playing itself into the engine.
    demo: Option<super::demo::Player>,
}

impl Mt32Device {
    /// Fit an MT-32 with the given ROM pair.
    pub fn open(control_rom: &Path, pcm_rom: &Path) -> Result<Self> {
        let synth = Mt32Synth::open(control_rom, pcm_rom, crate::audio::MIX_SAMPLE_RATE)?;
        log::info!(
            "midi: Munt MT-32 attached (engine {}, {} Hz)",
            super::engine_version(),
            synth.sample_rate()
        );
        Ok(Self {
            synth,
            block: vec![(0.0, 0.0); BLOCK_FRAMES],
            played: BLOCK_FRAMES,
            pending: Vec::new(),
            demo: None,
        })
    }

    /// Take a byte off the serial line.
    pub fn write_byte(&mut self, b: u8) {
        self.pending.push(b);
    }

    /// The next frame, rendering a fresh block when the last one runs out.
    ///
    /// Crossing into the engine once a sample would cost more than the
    /// synthesis, so it is asked for a block at a time. Everything written
    /// since the last block lands before the next is rendered, and both
    /// sides advance on emulated time, so a run is reproducible: the same
    /// bytes land on the same sample.
    pub fn next_frame(&mut self) -> (f32, f32) {
        if self.played == self.block.len() {
            // A demo plays by rendered frame, so it keeps emulated time.
            if let Some(demo) = &mut self.demo {
                demo.advance(BLOCK_FRAMES, &mut self.pending);
                if demo.finished() {
                    self.demo = None;
                }
            }
            if !self.pending.is_empty() {
                let Self { synth, pending, .. } = self;
                synth.parse(pending);
                pending.clear();
            }
            self.synth.render(&mut self.block);
            self.played = 0;
        }
        let frame = self.block[self.played];
        self.played += 1;
        frame
    }

    /// Play one of the ROM's own songs, or stop.
    pub fn play_demo(&mut self, song: Option<super::demo::Song>) {
        self.demo = song.map(|s| super::demo::Player::new(s, self.synth.sample_rate()));
    }

    /// Whether a song is still running, so the chain knows to move on.
    pub fn demo_playing(&self) -> bool {
        self.demo.is_some()
    }

    /// The synth, for the front panel and the display.
    pub fn synth(&self) -> &Mt32Synth {
        &self.synth
    }

    pub fn synth_mut(&mut self) -> &mut Mt32Synth {
        &mut self.synth
    }
}

impl std::fmt::Debug for Mt32Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mt32Device")
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

/// Where the MT-32's ROM pair lives, carried so the device can be attached
/// and dropped at runtime without going back to the configuration.
#[derive(Debug, Clone, Default)]
pub struct Mt32Roms {
    pub control: Option<PathBuf>,
    pub pcm: Option<PathBuf>,
}

impl Mt32Roms {
    /// Both images, when both are configured. One on its own is not enough
    /// to fit an MT-32 -- the engine needs the pair.
    pub fn pair(&self) -> Option<(&Path, &Path)> {
        Some((self.control.as_deref()?, self.pcm.as_deref()?))
    }

    /// Whether an MT-32 could be attached, so a picker knows to offer one.
    pub fn configured(&self) -> bool {
        self.pair().is_some()
    }
}
