// SPDX-License-Identifier: GPL-3.0-or-later

//! Audio source fan-out: [`AudioMux`] sits between Paula's mixer and the
//! live/capture sinks in [`crate::audio`], letting every audio producer
//! (Paula, drive sounds, CD audio, MT-32) register as a named source once,
//! so stem capture and future boards (e.g. a Toccata AHI board) need no
//! further host-side plumbing. See docs/internals/audio.md.

use super::{AudioRuntimeStatus, AudioSink};

/// Fan-out point every audio producer pushes through. Owns the master sink
/// (the same [`AudioSink`] the emulator has always used for live playback or
/// `--audio-wav` capture) and, once wired up, optional per-source/
/// per-channel stem writers. In this milestone the mux only forwards to the
/// master sink -- stem capture lands in a later change.
pub struct AudioMux {
    master: Box<dyn AudioSink>,
}

impl AudioMux {
    pub fn new(master: Box<dyn AudioSink>) -> Self {
        Self { master }
    }

    /// Replace the live master sink in place (device hot-swap, output
    /// picker changes, device-loss recovery). Any future stem writers are
    /// unaffected -- a capture in progress keeps writing across a live
    /// output device change.
    pub fn set_master(&mut self, master: Box<dyn AudioSink>) {
        self.master = master;
    }

    /// Push the final post-master-volume/stereo-width stereo frame -- the
    /// same value every mixed-master sink (live or `--audio-wav`) has
    /// always received.
    pub fn push_master(&mut self, left: f32, right: f32) {
        self.master.push(left, right);
    }

    pub fn flush(&mut self) {
        self.master.flush();
    }

    pub fn live_output_lead_seconds(&self) -> f64 {
        self.master.live_output_lead_seconds()
    }

    pub fn runtime_status(&self) -> AudioRuntimeStatus {
        self.master.runtime_status()
    }

    pub fn set_live_output_suspended(&mut self, suspended: bool) {
        self.master.set_live_output_suspended(suspended);
    }

    pub fn reset_live_output_after_timeline_jump(&mut self) {
        self.master.reset_live_output_after_timeline_jump();
    }

    pub fn is_null_sink(&self) -> bool {
        self.master.is_null_sink()
    }

    pub fn device_lost(&self) -> bool {
        self.master.device_lost()
    }

    /// Tap a named source's stereo contribution (e.g. "paula", "cdda",
    /// "mt32", "drivesounds") for stem capture. A no-op until stem writers
    /// exist -- callers push here unconditionally so wiring a new source
    /// needs no further change once capture lands.
    pub fn push_source(&mut self, _source: &'static str, _left: f32, _right: f32) {}

    /// Tap one named sub-channel of a source (e.g. Paula's four physical
    /// channels) for per-channel stem capture. A no-op until stem writers
    /// exist, like [`Self::push_source`].
    pub fn push_source_channel(
        &mut self,
        _source: &'static str,
        _channel: &'static str,
        _sample: f32,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::NullSink;

    struct CollectSink(std::rc::Rc<std::cell::RefCell<Vec<(f32, f32)>>>);
    impl AudioSink for CollectSink {
        fn push(&mut self, left: f32, right: f32) {
            self.0.borrow_mut().push((left, right));
        }
        fn flush(&mut self) {}
    }

    #[test]
    fn push_master_forwards_the_exact_frame_to_the_master_sink() {
        let frames = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut mux = AudioMux::new(Box::new(CollectSink(frames.clone())));
        mux.push_master(0.25, -0.5);
        mux.push_master(1.0, -1.0);
        assert_eq!(*frames.borrow(), vec![(0.25, -0.5), (1.0, -1.0)]);
    }

    #[test]
    fn set_master_swaps_the_sink_without_losing_the_mux() {
        let mut mux = AudioMux::new(Box::new(NullSink));
        assert!(mux.is_null_sink());
        let frames = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        mux.set_master(Box::new(CollectSink(frames.clone())));
        assert!(!mux.is_null_sink());
        mux.push_master(0.1, 0.2);
        assert_eq!(*frames.borrow(), vec![(0.1, 0.2)]);
    }
}
