// SPDX-License-Identifier: GPL-3.0-or-later

//! The card's part of a save state (`VideoSlotDevice::save_state` and
//! `load_state`): what is kept, how it is taken from the card and the
//! compose pipe, and how a card becomes the one a state describes.

use super::*;

/// The card's emulated state, as a save state keeps it: what the guest
/// reads back through the parallel port (the framestore banks, by way of
/// the header latches) and what decides the next fields' work. Its
/// `Default` is the card at power-on.
///
/// Not in it: the inputs' host files, which play on from where they are;
/// the Program and Preview outputs, which the next frame repaints; and
/// what the Flyer showed since the last field, which the Flyer puts back
/// on the link itself when its own state is loaded.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct SavedToaster {
    /// Both banks ([`FrameStore`]); empty: all zero.
    #[serde(with = "crate::bytes_serde")]
    frame_store: Vec<u8>,
    /// The picture digitised into each bank and not yet encoded into it
    /// (RGBA bytes); empty: none.
    #[serde(with = "crate::bytes_serde")]
    pending_grab_0: Vec<u8>,
    #[serde(with = "crate::bytes_serde")]
    pending_grab_1: Vec<u8>,
    /// The header latches as `[r, g, b, i]`; empty: the power-on values.
    elh: Vec<[u16; 4]>,
    active: bool,
    record_lines: u32,
    /// [`Stats`], in declaration order.
    stats: [u64; 6],
    video: Option<video::SavedVideo>,
    flyer_routed: [bool; 2],
    flyer_sound: Vec<(f32, f32)>,
    /// The level last reported to the Flyer.
    line_peak: (f32, f32),
    scanned: Option<(u64, u32)>,
    /// How many of the newest fields' sound is not yet due
    /// (`submitted - joined`).
    lag: u64,
    /// Sound of finished fields not yet taken.
    results: Vec<SavedResult>,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct SavedResult {
    /// Fields before the newest one handed over.
    age: u64,
    sound: Vec<(f32, f32)>,
    peak: Option<(f32, f32)>,
}

impl Toaster {
    /// The card's state as the bytes a save state carries.
    pub(super) fn state_bytes(&self) -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        match crate::savestate::chunk::encode(&self.saved(), &mut bytes) {
            Ok(()) => Some(bytes),
            Err(e) => {
                log::error!("toaster: the card's state could not be saved: {e}");
                None
            }
        }
    }

    /// Become the card of a loaded state: `state` is what
    /// [`Self::state_bytes`] gave, or `None` for the card at power-on.
    pub(super) fn load_state_bytes(&mut self, state: Option<&[u8]>) {
        let saved = match state.map(crate::savestate::chunk::decode::<SavedToaster>) {
            Some(Ok(saved)) => saved,
            Some(Err(e)) => {
                log::warn!(
                    "toaster: the state's card could not be read ({e:#}); \
                     the card starts as at power-on"
                );
                SavedToaster::default()
            }
            None => SavedToaster::default(),
        };
        self.restore(saved);
    }

    /// [`Self::settle`] from a shared borrow, for saving a state. A
    /// compose thread that has died is not waited for: the fields it left
    /// queued are run here, as [`Self::worker_alive`] runs them at the
    /// card's next use, so what is saved is the card the session goes on
    /// with.
    fn settle_shared(&self) {
        if self.worker.as_ref().is_some_and(|w| !w.is_finished()) {
            if self.joined < self.submitted {
                self.pipe.drain(self.submitted);
            }
            return;
        }
        let left: Vec<Job> = self.pipe.lock().queue.drain(..).collect();
        for job in left {
            self.run_inline(job);
        }
    }

    /// The card's state for a save state: see [`SavedToaster`].
    fn saved(&self) -> SavedToaster {
        // The fields handed over are part of the instant being saved.
        self.settle_shared();
        let card = self.card.lock().unwrap_or_else(|e| e.into_inner());
        let st = self.pipe.lock();
        let grab = |bank: usize| {
            card.pending_grab[bank]
                .as_deref()
                .map(video::picture_bytes)
                .unwrap_or_default()
        };
        let s = card.stats;
        SavedToaster {
            // A card nobody has written to (fitted but not driven) saves
            // no banks: empty reads back as all zero.
            frame_store: if card.frame_store.data.iter().all(|&b| b == 0) {
                Vec::new()
            } else {
                card.frame_store.data.clone()
            },
            pending_grab_0: grab(0),
            pending_grab_1: grab(1),
            elh: card.elh.iter().map(|h| [h.r, h.g, h.b, h.i]).collect(),
            active: card.active,
            record_lines: card.record_lines,
            stats: [
                s.fields,
                s.elh_lines,
                s.write_lines,
                s.read_requests,
                s.port_bytes,
                s.grabs_encoded,
            ],
            video: Some(card.video.saved()),
            flyer_routed: card.flyer_routed,
            flyer_sound: card.flyer_sound.clone(),
            line_peak: self.flyer.as_ref().map_or((0.0, 0.0), |l| l.line_peak()),
            scanned: self.scanned,
            lag: self.submitted - self.joined,
            results: st
                .results
                .iter()
                .map(|r| SavedResult {
                    age: self.submitted.saturating_sub(r.id),
                    sound: r.sound.clone(),
                    peak: r.peak,
                })
                .collect(),
        }
    }

    /// Become the card `saved` describes.
    fn restore(&mut self, saved: SavedToaster) {
        // Nothing handed over on the timeline that is being left may land
        // after this.
        self.settle();
        let mut card = self.card.lock().unwrap_or_else(|e| e.into_inner());
        card.frame_store = FrameStore::new();
        if saved.frame_store.len() == card.frame_store.data.len() {
            card.frame_store.data = saved.frame_store;
        }
        let grab = |bytes: &[u8]| {
            (bytes.len() == video::PICTURE_W * video::PICTURE_H * 4)
                .then(|| video::picture_of_bytes(bytes))
        };
        card.pending_grab = [grab(&saved.pending_grab_0), grab(&saved.pending_grab_1)];
        card.elh = [Header::default(); ELH_COUNT];
        if saved.elh.len() == ELH_COUNT {
            for (latch, &[r, g, b, i]) in card.elh.iter_mut().zip(&saved.elh) {
                *latch = Header { r, g, b, i };
            }
        } else {
            for (index, header) in ELH_DEFAULTS {
                card.elh[index] = header;
            }
        }
        card.active = saved.active;
        card.record_lines = saved.record_lines;
        let [fields, elh_lines, write_lines, read_requests, port_bytes, grabs_encoded] =
            saved.stats;
        card.stats = Stats {
            fields,
            elh_lines,
            write_lines,
            read_requests,
            port_bytes,
            grabs_encoded,
        };
        card.video.restore(saved.video.as_ref());
        card.flyer_routed = saved.flyer_routed;
        card.flyer_shown = [Vec::new(), Vec::new()];
        card.flyer_sound = saved.flyer_sound;
        card.dve_on = false;
        drop(card);
        if let Some(link) = &self.flyer {
            link.set_line_peak(saved.line_peak);
        }
        // The pipe: nothing queued, and the sound not yet taken back at
        // its distance from the newest field, counted from a new origin.
        let newest = saved
            .results
            .iter()
            .map(|r| r.age)
            .max()
            .unwrap_or(0)
            .max(saved.lag);
        let mut results: Vec<JobResult> = saved
            .results
            .into_iter()
            .map(|r| JobResult {
                id: newest - r.age,
                sound: r.sound,
                peak: r.peak,
            })
            .collect();
        results.sort_by_key(|r| r.id);
        let mut st = self.pipe.lock();
        st.queue.clear();
        st.completed = newest;
        st.results = results.into();
        drop(st);
        self.submitted = newest;
        self.joined = newest - saved.lag;
        self.scanned = saved.scanned;
    }
}
