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
//!   progress, valid for the lines the beam has already passed. A card that
//!   reacts within the same field (a request drawn on line 20, answered on
//!   line 40) uses this.
//!
//! Both views share one coordinate system: framebuffer rows from the
//! field's first visible line, and hi-res framebuffer columns where column
//! 0 is colour clock `$28` (four hi-res pixels per colour clock, times the
//! canvas scale on super-hi-res canvases). [`FieldPixels`] converts beam
//! positions to it.

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

/// Colour clock that lands on framebuffer column 0 (matches the
/// renderer's `COPPER_WAIT_HPOS_FB0`).
pub const FB0_HPOS: u32 = 0x28;

/// A rendered field, as Denise drove the video slot.
///
/// `pixels` is the renderer's framebuffer: RGBA8 packed little-endian
/// (red in the low byte), with the genlock transparency in the alpha byte.
#[derive(Clone, Copy, Debug)]
pub struct FieldPixels<'a> {
    pub pixels: &'a [u32],
    /// Row stride in pixels (`FB_WIDTH * canvas_scale`).
    pub width: usize,
    /// Rows of the field that were rendered.
    pub rows: usize,
    /// Beam line of row 0.
    pub visible_start_vpos: u32,
    /// Horizontal supersample factor (1 normally, 2 on 35 ns canvases).
    pub canvas_scale: usize,
    pub long_frame: bool,
    pub interlaced: bool,
    /// Rows `0..valid_rows` are final. Equal to `rows` for a completed field;
    /// a live field is final only above the beam.
    pub valid_rows: usize,
}

impl<'a> FieldPixels<'a> {
    /// Framebuffer row for a beam line, if it was rendered and is final.
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

    /// Framebuffer column of the first hi-res pixel of colour clock `hpos`
    /// (negative or past the row for clocks outside the captured aperture).
    pub fn column_for_hpos(&self, hpos: u32) -> i64 {
        (i64::from(hpos) - i64::from(FB0_HPOS)) * 4 * self.canvas_scale as i64
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

/// On-demand render of the field the beam is currently in.
///
/// The first call to [`LiveField::pixels`] in a field renders the field so
/// far; later calls reuse that render until the beam has moved onto a line
/// the cached render did not yet have final.
pub struct LiveField<'a> {
    pub(crate) bus: &'a crate::bus::Bus,
    pub(crate) cache: &'a mut LiveFieldCache,
    pub(crate) beam: BeamPos,
}

impl<'a> LiveField<'a> {
    pub fn beam(&self) -> BeamPos {
        self.beam
    }

    /// The in-progress field, final for every line above the beam.
    pub fn pixels(&mut self) -> FieldPixels<'_> {
        let fresh = self.cache.frame == Some(self.beam.frame)
            && self.cache.rendered_through_vpos >= self.beam.vpos;
        if !fresh {
            let (rows, width, start, scale, laced) =
                crate::video::bitplane::render_in_progress(self.bus, &mut self.cache.fb);
            self.cache.rows = rows;
            self.cache.width = width;
            self.cache.visible_start_vpos = start;
            self.cache.canvas_scale = scale;
            self.cache.interlaced = laced;
            self.cache.frame = Some(self.beam.frame);
            self.cache.rendered_through_vpos = self.beam.vpos;
            self.cache.renders = self.cache.renders.saturating_add(1);
        }
        let valid_rows = self
            .cache
            .rendered_through_vpos
            .saturating_sub(self.cache.visible_start_vpos) as usize;
        FieldPixels {
            pixels: &self.cache.fb,
            width: self.cache.width,
            rows: self.cache.rows,
            visible_start_vpos: self.cache.visible_start_vpos,
            canvas_scale: self.cache.canvas_scale,
            long_frame: self.beam.long_frame,
            interlaced: self.cache.interlaced,
            valid_rows,
        }
    }
}

/// Bus-owned scratch for [`LiveField`] renders.
#[derive(Default)]
pub(crate) struct LiveFieldCache {
    fb: Vec<u32>,
    rows: usize,
    width: usize,
    visible_start_vpos: u32,
    canvas_scale: usize,
    interlaced: bool,
    frame: Option<u64>,
    rendered_through_vpos: u32,
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

    /// Whether speculative run-ahead may run with this device attached.
    /// A card holding framestores and reacting to the picture is machine
    /// state run-ahead cannot rewind, so the default is no.
    fn runahead_safe(&self) -> bool {
        false
    }
}
