// SPDX-License-Identifier: GPL-3.0-or-later

//! Audio source fan-out: [`crate::audio::mux::AudioMux`] sits between Paula's
//! mixer and the live/capture sinks in [`crate::audio`], letting every audio
//! producer (Paula, drive sounds, CD audio, MT-32) register as a named
//! source once, so stem capture and future boards (e.g. a Toccata AHI board)
//! need no further host-side plumbing. See docs/internals/audio.md.
