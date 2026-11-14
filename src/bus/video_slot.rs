// SPDX-License-Identifier: GPL-3.0-or-later

//! The video slot: fitting and removing a card (`crate::videoslot`), the
//! genlock sync a card or `[chipset] genlock` supplies to Agnus, handing
//! completed fields and their sound to the card at the frame wrap, and
//! the card's drive of the parallel data pins. Split out of `bus.rs` for
//! size; this is the same `Bus`, with full access to its private state.

use super::*;

impl Bus {
    /// Fit a card in the video slot, replacing any fitted one.
    pub fn attach_video_slot(&mut self, device: Box<dyn crate::videoslot::VideoSlotDevice>) {
        if device.supplies_sync() {
            self.agnus.set_genlock_sync(true);
        }
        self.video_slot = Some(device);
        self.video_slot_live = Default::default();
    }

    /// Remove the video-slot card, returning it. Its sync goes with it
    /// unless a genlock is configured independently.
    pub fn detach_video_slot(&mut self) -> Option<Box<dyn crate::videoslot::VideoSlotDevice>> {
        let device = self.video_slot.take();
        if device.as_ref().is_some_and(|d| d.supplies_sync()) && !self.genlock_configured {
            self.agnus.set_genlock_sync(false);
        }
        device
    }

    /// Fit or remove a stand-alone genlock (`[chipset] genlock`).
    pub fn set_genlock(&mut self, present: bool) {
        self.genlock_configured = present;
        self.apply_genlock_sync();
    }

    /// Give Agnus the sync actually present: a stand-alone genlock, or a
    /// video-slot card that supplies it. Called wherever Agnus or the card
    /// changes under the other (a reset's new Agnus, a state load that
    /// keeps the live card), and then reconciles the ERSY latch with
    /// BPLCON0 as it stands: without sync, ERSY set freezes the counters
    /// from here, as a write of it would have.
    pub(super) fn apply_genlock_sync(&mut self) {
        let card_sync = self.video_slot.as_ref().is_some_and(|d| d.supplies_sync());
        self.agnus
            .set_genlock_sync(self.genlock_configured || card_sync);
        self.agnus.set_ersy(self.denise.bplcon0 & 0x0002 != 0);
    }

    pub fn video_slot(&self) -> Option<&dyn crate::videoslot::VideoSlotDevice> {
        self.video_slot.as_deref()
    }

    pub fn video_slot_mut(
        &mut self,
    ) -> Option<&mut (dyn crate::videoslot::VideoSlotDevice + 'static)> {
        self.video_slot.as_deref_mut()
    }

    /// Have the fitted card finish the fields it was handed, so its
    /// outputs are those of this emulated instant
    /// ([`crate::videoslot::VideoSlotDevice::settle_outputs`]).
    pub fn settle_video_slot_outputs(&mut self) {
        if let Some(card) = self.video_slot.as_mut() {
            card.settle_outputs();
        }
    }

    /// The fitted video-slot card's own video outputs (none without a
    /// card, or for a card that has none).
    pub fn video_slot_outputs(&self) -> Vec<std::sync::Arc<crate::videoslot::CardOutput>> {
        self.video_slot
            .as_ref()
            .map(|card| card.video_outputs())
            .unwrap_or_default()
    }

    /// Live in-progress renders the video-slot tap has performed.
    pub fn video_slot_live_renders(&self) -> u64 {
        self.video_slot_live.renders
    }

    /// First beam line the frame capture keeps on a standard scan: the
    /// deep-overscan minimum, or with a video-slot card fitted the first
    /// line sprite DMA can run on (NTSC `$14`, PAL `$19`), since a card
    /// reads the line headers software puts on those lines (the Toaster's
    /// sprite table starts at line `$15`).
    pub(crate) fn capture_start_vpos(&self) -> u32 {
        if self.video_slot.is_some() {
            sprite_dma_first_active_vpos(self.agnus.video_standard())
                .min(RENDER_MIN_OVERSCAN_START_VPOS)
        } else {
            RENDER_MIN_OVERSCAN_START_VPOS
        }
    }

    fn video_slot_beam(&self) -> crate::videoslot::BeamPos {
        crate::videoslot::BeamPos {
            vpos: self.agnus.vpos,
            hpos: self.agnus.hpos,
            cck: self.emulated_cck,
            long_frame: self.agnus.lof,
            frame: self.emulated_frames,
        }
    }

    /// Frame-wrap hook: hand the just-completed field to the video-slot
    /// card. Called after `begin_new_beam_frame` promoted the capture, so
    /// the `frame_*` accessors describe that field.
    pub(super) fn notify_video_slot_field_complete(&mut self) {
        let Some(mut device) = self.video_slot.take() else {
            return;
        };
        if device.takes_render_input() && self.last_frame_render_base.is_some() {
            // The card paints the field itself (on its own thread): hand
            // it the frame's render input, in a bundle it used before
            // where there is one, so the vectors are not reallocated.
            device.take_spent_render_inputs(&mut self.video_slot_inputs);
            self.video_slot_inputs.truncate(4);
            let mut input = match self.video_slot_inputs.pop() {
                Some(mut input) => {
                    input.refill_from_bus(self);
                    input
                }
                None => Box::new(crate::video::bitplane::RenderInput::from_bus(self)),
            };
            // The debugger's layer toggles are for the host's display.
            input.show_all_layers();
            // The counter already moved on to the new field.
            let mut at = self.video_slot_beam();
            at.frame = at.frame.saturating_sub(1);
            device.field_render_input(input, at);
        } else if device.wants_completed_fields() && self.last_frame_render_base.is_some() {
            let mut fb = std::mem::take(&mut self.video_slot_fb);
            let mut scratch = std::mem::take(&mut self.video_slot_scratch);
            let mut input = crate::video::bitplane::RenderInput::from_bus(self);
            input.show_all_layers();
            let wide = crate::video::bitplane::render_wide(
                &mut input,
                crate::videoslot::SLOT_FIRST_CCK,
                crate::videoslot::SLOT_LINE_CCK,
                usize::MAX,
                &mut fb,
                &mut scratch,
            );
            // LOF has already toggled for the new field on a laced screen;
            // the completed field was the other one.
            let long_frame = if wide.interlaced {
                !self.agnus.lof
            } else {
                self.agnus.lof
            };
            let field = crate::videoslot::FieldPixels {
                pixels: &fb,
                width: wide.stride,
                rows: wide.rows,
                visible_start_vpos: wide.visible_start_vpos,
                first_cck: crate::videoslot::SLOT_FIRST_CCK,
                canvas_scale: wide.canvas_scale,
                long_frame,
                interlaced: wide.interlaced,
                valid_rows: wide.rows,
            };
            // The counter already moved on to the new field.
            let mut at = self.video_slot_beam();
            at.frame = at.frame.saturating_sub(1);
            device.field_complete(&field, at);
            self.video_slot_fb = fb;
            self.video_slot_scratch = scratch;
        }
        // The card's sound for the frame it may just have finished.
        let mut sound = std::mem::take(&mut self.video_slot_audio);
        device.take_audio(&mut sound);
        self.paula.video_slot_audio_mut().push_chunk(&sound);
        sound.clear();
        self.video_slot_audio = sound;
        self.video_slot = Some(device);
    }

    /// `$BFE101` read hook: let the video-slot card drive the data pins.
    pub(super) fn video_slot_parallel_read(&mut self) -> Option<u8> {
        let mut device = self.video_slot.take()?;
        let mut cache = std::mem::take(&mut self.video_slot_live);
        let ddrb = self.cia_a.port_b_ddr();
        let beam = self.video_slot_beam();
        let byte = {
            let mut live = crate::videoslot::LiveField {
                bus: self,
                cache: &mut cache,
                beam,
            };
            device.parallel_read(&mut live, ddrb)
        };
        self.video_slot_live = cache;
        self.video_slot = Some(device);
        // Output pins stay CIA-driven whatever the card does.
        byte.map(|b| (b & !ddrb) | (self.cia_a.port_b_pins() & ddrb))
    }
}
