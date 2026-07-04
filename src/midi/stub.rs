// SPDX-License-Identifier: GPL-3.0-or-later

//! Fallback MIDI backend for platforms without a native implementation yet.
//! Enumerates nothing and refuses to open, so `[serial] mode = "midi"` fails
//! with a clear message rather than silently doing nothing. macOS (CoreMIDI),
//! Linux (ALSA sequencer), and Windows (WinMM) have native backends; any other
//! target lands here.

use anyhow::{bail, Result};

use super::{InputProducer, MidiBackend, MidiEndpoints};

pub fn enumerate() -> MidiEndpoints {
    MidiEndpoints::default()
}

pub fn open(
    _midi_out: Option<&str>,
    _midi_in: Option<&str>,
    _input: InputProducer,
) -> Result<Box<dyn MidiBackend>> {
    bail!("MIDI is not supported on this platform yet");
}
