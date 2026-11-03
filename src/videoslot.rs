// SPDX-License-Identifier: GPL-3.0-or-later

//! Video-slot peripheral boundary.
//!
//! The A2000/A3000/A4000 video slot carries Denise's digital RGB output and
//! the beam syncs. A card that sits there (the NewTek Video Toaster is the
//! reason this exists) watches the picture the Amiga is displaying and acts
//! on it. Some such cards also answer the host: the Toaster's switcher
//! reads framestore and calibration data back over the parallel port
//! (`$BFE101`, CIA-A port B), polling it at beam-timed positions while the
//! card drives the data pins.
//!
//! Copperline does not paint pixels while the chipset runs (see
//! `docs/internals/video.md`): register writes are recorded with their beam
//! positions and the frame is replayed afterwards. A video-slot device
//! therefore sees the picture in two ways:
//!
//! - [`VideoSlotDevice::field_complete`] receives every finished field,
//!   rendered through the same painter the display uses, at the frame wrap.
//! - [`VideoSlotDevice::parallel_read`] runs on every guest read of
//!   `$BFE101` and gets a [`LiveField`] that can render the field still in
//!   progress, down to the line the beam is on. A card that reacts within
//!   the same field (a request drawn on line 20, answered on line 40, or a
//!   byte put on the port while its pixel is being displayed) uses this.
//!
//! Both views paint a line wider than the display framebuffer: colour
//! clocks [`SLOT_FIRST_CCK`] onwards for [`SLOT_LINE_CCK`] clocks, which
//! covers the whole active line a video card samples (the Toaster's 768
//! pixels start left of the display capture and run past its right edge).
//! Rows start at the field's first captured line, which a fitted card moves
//! up to the first line sprite DMA can run on (`Bus::capture_start_vpos`).
//! [`FieldPixels`] converts beam positions to the canvas.

/// Beam position of an event, plus the deterministic power-on colour-clock
/// timestamp.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BeamPos {
    pub vpos: u32,
    pub hpos: u32,
    pub cck: u64,
    /// Long-frame flag of the field the beam is in (LOF).
    pub long_frame: bool,
    /// Emulated frame counter of the field the beam is in.
    pub frame: u64,
}

/// First colour clock of the slot canvas. Lo-res position `$5A` on the
/// Denise comparator: the Toaster's line header sprite sits at `$5B`.
pub const SLOT_FIRST_CCK: i32 = 0x24;
/// Colour clocks the slot canvas spans: through clock `$F7`, past the end
/// of the Toaster's 768-pixel line (`$5C` to `$1DC` lo-res) and short of
/// the line's own end (227).
pub const SLOT_LINE_CCK: usize = 212;

/// A rendered field, as Denise drove the video slot.
///
/// `pixels` is a painter canvas: RGBA8 packed little-endian (red in the low
/// byte), with the genlock transparency in the alpha byte. Column 0 is
/// colour clock `first_cck`; each clock is four hi-res pixels (times
/// `canvas_scale`).
#[derive(Clone, Copy, Debug)]
pub struct FieldPixels<'a> {
    pub pixels: &'a [u32],
    /// Row stride in pixels.
    pub width: usize,
    /// Rows of the field that were rendered.
    pub rows: usize,
    /// Beam line of row 0.
    pub visible_start_vpos: u32,
    /// Colour clock of column 0.
    pub first_cck: i32,
    /// Horizontal supersample factor (1 normally, 2 on 35 ns canvases).
    pub canvas_scale: usize,
    pub long_frame: bool,
    pub interlaced: bool,
    /// Rows `0..valid_rows` are final. Equal to `rows` for a completed field;
    /// a live field is final above the beam, and on the beam's own line to
    /// the left of the beam.
    pub valid_rows: usize,
}

impl<'a> FieldPixels<'a> {
    /// Canvas row for a beam line, if it was rendered and is final.
    pub fn row_index(&self, vpos: u32) -> Option<usize> {
        let row = vpos.checked_sub(self.visible_start_vpos)? as usize;
        (row < self.valid_rows.min(self.rows)).then_some(row)
    }

    /// One rendered row, by beam line.
    pub fn row(&self, vpos: u32) -> Option<&'a [u32]> {
        let row = self.row_index(vpos)?;
        let start = row * self.width;
        self.pixels.get(start..start + self.width)
    }

    /// Canvas column of the first hi-res pixel of colour clock `hpos`
    /// (negative or past the row for clocks outside the canvas).
    pub fn column_for_hpos(&self, hpos: u32) -> i64 {
        (i64::from(hpos) - i64::from(self.first_cck)) * 4 * self.canvas_scale as i64
    }

    /// Canvas column of a Denise lo-res comparator position (the DIW and
    /// sprite coordinate), left edge of its two hi-res pixels.
    pub fn column_for_lores(&self, lores: i32) -> i64 {
        // Lo-res position L is colour clock (L - $12) / 2; see the
        // renderer's DIW_HSTART_FB0 / COPPER_WAIT_HPOS_FB0 anchors.
        (i64::from(lores) - 0x12 - 2 * i64::from(self.first_cck)) * 2 * self.canvas_scale as i64
    }

    /// The RGBA8 pixel the beam was emitting at (`vpos`, `hpos`) plus `sub`
    /// hi-res pixels (0-3 within the colour clock).
    pub fn pixel_at_beam(&self, vpos: u32, hpos: u32, sub: u32) -> Option<u32> {
        let row = self.row(vpos)?;
        let col = self.column_for_hpos(hpos) + i64::from(sub) * self.canvas_scale as i64;
        usize::try_from(col).ok().and_then(|c| row.get(c).copied())
    }

    /// `pixel_at_beam` as 12-bit Amiga RGB (`$0RGB`), the four bits per gun
    /// an ECS-era video-slot card latches.
    pub fn rgb12_at_beam(&self, vpos: u32, hpos: u32, sub: u32) -> Option<u16> {
        self.pixel_at_beam(vpos, hpos, sub).map(rgba8_to_rgb12)
    }
}

/// RGBA8 framebuffer pixel to its high nibbles as `$0RGB`.
pub fn rgba8_to_rgb12(c: u32) -> u16 {
    let r = (c >> 4) & 0xF;
    let g = (c >> 12) & 0xF;
    let b = (c >> 20) & 0xF;
    ((r << 8) | (g << 4) | b) as u16
}

/// RGBA8 framebuffer pixel to 24-bit `$RRGGBB`.
pub fn rgba8_to_rgb24(c: u32) -> u32 {
    let [r, g, b, _] = c.to_le_bytes();
    (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}

/// On-demand render of the field the beam is currently in.
///
/// [`LiveField::pixels`] renders the field down to the beam's line; the
/// render is reused while the beam stays put.
pub struct LiveField<'a> {
    pub(crate) bus: &'a crate::bus::Bus,
    pub(crate) cache: &'a mut LiveFieldCache,
    pub(crate) beam: BeamPos,
}

impl<'a> LiveField<'a> {
    pub fn beam(&self) -> BeamPos {
        self.beam
    }

    /// The in-progress field: final above the beam, and on the beam's line
    /// to the left of it.
    pub fn pixels(&mut self) -> FieldPixels<'_> {
        let key = (self.beam.frame, self.beam.vpos, self.beam.hpos);
        if self.cache.rendered_at != Some(key) {
            let start = self.bus.in_progress_render_parts().visible_start_vpos;
            let rows_limit = self.beam.vpos.saturating_sub(start) as usize + 1;
            let mut input = crate::video::bitplane::RenderInput::from_bus_in_progress(self.bus);
            let wide = crate::video::bitplane::render_wide(
                &mut input,
                SLOT_FIRST_CCK,
                SLOT_LINE_CCK,
                rows_limit,
                &mut self.cache.fb,
                &mut self.cache.scratch,
            );
            self.cache.wide = wide;
            self.cache.rendered_at = Some(key);
            self.cache.renders = self.cache.renders.saturating_add(1);
        }
        let wide = self.cache.wide;
        FieldPixels {
            pixels: &self.cache.fb,
            width: wide.stride,
            rows: wide.rows,
            visible_start_vpos: wide.visible_start_vpos,
            first_cck: SLOT_FIRST_CCK,
            canvas_scale: wide.canvas_scale,
            long_frame: self.beam.long_frame,
            interlaced: wide.interlaced,
            valid_rows: wide.rows,
        }
    }
}

/// Bus-owned scratch for [`LiveField`] renders.
#[derive(Default)]
pub(crate) struct LiveFieldCache {
    fb: Vec<u32>,
    scratch: Vec<u32>,
    wide: crate::video::bitplane::WideRender,
    rendered_at: Option<(u64, u32, u32)>,
    /// Live renders performed (diagnostics and tests).
    pub(crate) renders: u64,
}

pub trait VideoSlotDevice: Send {
    /// Short name for logs and the UI.
    fn name(&self) -> &'static str;

    /// Whether the device wants [`Self::field_complete`]. Rendering every
    /// field a second time costs a full replay, so a device that only
    /// answers parallel reads can opt out.
    fn wants_completed_fields(&self) -> bool {
        true
    }

    /// A field just finished. `at` is the beam at the wrap (line 0 of the
    /// next field).
    fn field_complete(&mut self, _field: &FieldPixels<'_>, _at: BeamPos) {}

    /// Drive the parallel data pins for one guest read of `$BFE101`.
    /// `Some(byte)` replaces what the guest reads (before the parallel-port
    /// peripheral and joystick adapter are consulted); `None` leaves the
    /// pins to them. `ddrb` is CIA-A's port-B direction register: a card
    /// can only drive pins the guest has made inputs.
    fn parallel_read(&mut self, _live: &mut LiveField<'_>, _ddrb: u8) -> Option<u8> {
        None
    }

    /// Whether the card supplies genlock sync to the Amiga (the Toaster
    /// does). While fitted, `BPLCON0.ERSY` slaves the beam to it instead
    /// of stopping the counters.
    fn supplies_sync(&self) -> bool {
        false
    }

    /// Whether speculative run-ahead may run with this device attached.
    /// A card holding framestores and reacting to the picture is machine
    /// state run-ahead cannot rewind, so the default is no.
    fn runahead_safe(&self) -> bool {
        false
    }
}
