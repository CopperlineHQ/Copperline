// SPDX-License-Identifier: GPL-3.0-or-later

//! Memory heat map: which parts of the address space are being touched,
//! by what, and how recently.
//!
//! A slot map answers "what owned the chip bus at this colour clock". It
//! does not answer "where in memory is anything happening", which is the
//! question you have when a display is drawn from the wrong buffer, a
//! decruncher is writing somewhere unexpected, or a DMA channel is
//! pointed at the wrong bank. This paints the address space instead of
//! the beam: one cell per block of memory, coloured by what last touched
//! it and faded by how long ago.
//!
//! The grid is 256x256 cells over a window of the address space, so a
//! 16 MiB window gives one cell per 256 bytes. The window is movable, so
//! the RAM banks a 32-bit CPU sees above the 24-bit space can be looked
//! at too, rather than the map silently stopping at 16 MiB.
//!
//! Everything here is derived from bus activity: nothing knows what
//! program is running, only which engine touched which address.

use crate::debugger::WatchSource;

pub const GRID: usize = 256;
pub const CELLS: usize = GRID * GRID;

/// Default window: the 24-bit space a 68000 and Agnus share, which is
/// where chip RAM, slow RAM, the custom registers, the CIAs and the
/// Zorro II space all live.
pub const DEFAULT_SPAN: u32 = 0x0100_0000;

/// Frames a cell takes to fade from its freshest colour to black. The
/// eye reads a subsecond tail (about two thirds of a second at 50 Hz) as
/// "recent" without smearing everything together.
pub const DECAY_FRAMES: u32 = 32;

/// What last touched a cell. Kept as a small code so the grid is one
/// byte of source plus one frame stamp per cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toucher {
    None,
    CpuRead,
    CpuWrite,
    Blitter,
    Copper,
    Disk,
    Bitplane,
    Sprite,
    Audio,
}

impl Toucher {
    /// The cell's colour at full brightness, as 0xAARRGGBB. Chosen so
    /// the read-side display channels sit in the blues and greens, the
    /// CPU in red/yellow, and the engines that write memory stand out.
    pub fn colour(self) -> u32 {
        match self {
            Toucher::None => 0xFF00_0000,
            Toucher::CpuRead => 0xFF80_4040,
            Toucher::CpuWrite => 0xFFFF_4040,
            Toucher::Blitter => 0xFFFF_A000,
            Toucher::Copper => 0xFFFF_FF40,
            Toucher::Disk => 0xFFFF_40FF,
            Toucher::Bitplane => 0xFF40_A0FF,
            Toucher::Sprite => 0xFF40_FFA0,
            Toucher::Audio => 0xFF40_FFFF,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Toucher::None => "none",
            Toucher::CpuRead => "cpu-read",
            Toucher::CpuWrite => "cpu-write",
            Toucher::Blitter => "blitter",
            Toucher::Copper => "copper",
            Toucher::Disk => "disk",
            Toucher::Bitplane => "bitplane",
            Toucher::Sprite => "sprite",
            Toucher::Audio => "audio",
        }
    }

    pub fn from_watch_source(source: WatchSource) -> Self {
        match source {
            WatchSource::Cpu => Toucher::CpuWrite,
            WatchSource::Blitter => Toucher::Blitter,
            WatchSource::Disk => Toucher::Disk,
            WatchSource::Bitplane(_) => Toucher::Bitplane,
            WatchSource::Sprite(_) => Toucher::Sprite,
            WatchSource::Audio(_) => Toucher::Audio,
            WatchSource::Copper => Toucher::Copper,
        }
    }

    fn code(self) -> u8 {
        self as u8
    }

    fn from_code(code: u8) -> Self {
        match code {
            1 => Toucher::CpuRead,
            2 => Toucher::CpuWrite,
            3 => Toucher::Blitter,
            4 => Toucher::Copper,
            5 => Toucher::Disk,
            6 => Toucher::Bitplane,
            7 => Toucher::Sprite,
            8 => Toucher::Audio,
            _ => Toucher::None,
        }
    }
}

/// The span a requested one becomes: rounded up to a whole number of
/// bytes per cell, and clamped so the grid stays inside a u32.
///
/// Public because callers deciding whether a window has changed have to
/// compare what the request *becomes*, not what was asked for. Without
/// that, repeating a request whose span is not a multiple of the grid
/// looks like a different window every time and silently wipes the map.
pub fn rounded_span(span: u32) -> u32 {
    let cells = CELLS as u32;
    // A span shorter than the grid would give less than one byte per
    // cell, and the window's last addresses would index past its end.
    span.max(cells).div_ceil(cells).clamp(1, u32::MAX / cells) * cells
}

/// The live map.
pub struct HeatMap {
    /// First address the grid covers, and how much of the address space
    /// it spans. `span` is rounded up so every cell covers whole bytes.
    base: u32,
    span: u32,
    source: Box<[u8; CELLS]>,
    /// Frame each cell was last touched on.
    stamp: Box<[u32; CELLS]>,
}

impl Default for HeatMap {
    fn default() -> Self {
        Self::new(0, DEFAULT_SPAN)
    }
}

impl HeatMap {
    pub fn new(base: u32, span: u32) -> Self {
        Self {
            base,
            span: rounded_span(span),
            source: Box::new([0; CELLS]),
            stamp: Box::new([0; CELLS]),
        }
    }

    pub fn base(&self) -> u32 {
        self.base
    }

    pub fn span(&self) -> u32 {
        self.span
    }

    /// Bytes of the address space one cell covers.
    pub fn bytes_per_cell(&self) -> u32 {
        self.span / CELLS as u32
    }

    /// Move the window, clearing what the old one had recorded: the cells
    /// would otherwise carry activity from addresses they no longer name.
    pub fn set_window(&mut self, base: u32, span: u32) {
        *self = Self::new(base, span);
    }

    /// Record `len` bytes at `addr` touched by `by` on `frame`.
    pub fn touch(&mut self, addr: u32, len: u32, by: Toucher, frame: u64) {
        let Some(first) = self.cell_of(addr) else {
            return;
        };
        let end = addr.wrapping_add(len.max(1)).wrapping_sub(1);
        let last = match self.cell_of(end) {
            Some(cell) => cell,
            // Past the top of the window: mark what the transfer does
            // cover rather than only its first cell, which would
            // under-report every access near the window's edge. A
            // transfer that wrapped the address space (end below its own
            // start) is recorded at its first cell instead of sweeping.
            None if end > addr => CELLS - 1,
            None => first,
        };
        let stamp = frame as u32;
        for cell in first..=last.max(first) {
            self.source[cell] = by.code();
            self.stamp[cell] = stamp;
        }
    }

    fn cell_of(&self, addr: u32) -> Option<usize> {
        let offset = addr.checked_sub(self.base)?;
        if offset >= self.span {
            return None;
        }
        Some((offset / self.bytes_per_cell()) as usize)
    }

    /// Paint the grid into a 256x256 ARGB image as of `frame`, fading
    /// each cell by how long ago it was touched.
    pub fn render(&self, frame: u64, out: &mut [u32]) {
        let now = frame as u32;
        for (cell, pixel) in out.iter_mut().enumerate().take(CELLS) {
            let source = Toucher::from_code(self.source[cell]);
            if source == Toucher::None {
                *pixel = 0xFF00_0000;
                continue;
            }
            let age = now.saturating_sub(self.stamp[cell]);
            if age >= DECAY_FRAMES {
                *pixel = 0xFF00_0000;
                continue;
            }
            *pixel = fade(source.colour(), DECAY_FRAMES - age, DECAY_FRAMES);
        }
    }

    /// A census of the window: how many cells each toucher currently
    /// holds, for a caller that wants numbers rather than pixels.
    pub fn census(&self, frame: u64) -> Vec<(Toucher, usize)> {
        let now = frame as u32;
        let mut counts = [0usize; 9];
        for cell in 0..CELLS {
            let source = Toucher::from_code(self.source[cell]);
            if source == Toucher::None || now.saturating_sub(self.stamp[cell]) >= DECAY_FRAMES {
                continue;
            }
            counts[source.code() as usize] += 1;
        }
        (1..9)
            .map(|code| (Toucher::from_code(code as u8), counts[code]))
            .filter(|(_, n)| *n > 0)
            .collect()
    }

    /// What one cell records: the toucher that last claimed it and the
    /// frame stamp of that touch (the frame counter's low 32 bits, so an
    /// age is `frame - stamp`). `None` for a cell outside the grid or one
    /// nothing has touched since the window was set.
    ///
    /// Unlike [`HeatMap::render`], the stamp is returned raw rather than
    /// faded, so a caller can report an age past the decay window instead
    /// of only seeing black.
    pub fn cell(&self, cell: usize) -> Option<(Toucher, u32)> {
        let source = Toucher::from_code(*self.source.get(cell)?);
        (source != Toucher::None).then(|| (source, self.stamp[cell]))
    }
}

/// Scale a colour's channels by `num`/`den`, keeping it opaque.
fn fade(colour: u32, num: u32, den: u32) -> u32 {
    let scale = |shift: u32| {
        let channel = (colour >> shift) & 0xFF;
        ((channel * num / den) & 0xFF) << shift
    };
    0xFF00_0000 | scale(16) | scale(8) | scale(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_that_is_not_a_whole_number_of_cells_is_rounded_up() {
        // A span one byte past the grid size would otherwise give one
        // byte per cell and let the window's top address index off the
        // end of the grid.
        let mut map = HeatMap::new(0, CELLS as u32 + 1);
        assert_eq!(map.bytes_per_cell(), 2);
        assert_eq!(map.span(), CELLS as u32 * 2);
        map.touch(map.span() - 1, 1, Toucher::CpuWrite, 0);
        assert_eq!(map.census(0), vec![(Toucher::CpuWrite, 1)]);
        // The largest span the grid can describe stays inside u32.
        let wide = HeatMap::new(0, u32::MAX);
        assert_eq!(wide.bytes_per_cell(), u32::MAX / CELLS as u32);
    }

    #[test]
    fn re_requesting_the_same_window_is_recognised_after_rounding() {
        // A span that is not a whole number of cells becomes one; a
        // caller repeating its own request must be seen as asking for
        // the window it already has, or the map is wiped each time.
        let asked = DEFAULT_SPAN + 1;
        let map = HeatMap::new(0, asked);
        assert_eq!(map.span(), rounded_span(asked));
        assert_eq!(rounded_span(rounded_span(asked)), rounded_span(asked));
    }

    #[test]
    fn a_touch_lands_in_the_cell_covering_its_address() {
        let mut map = HeatMap::new(0, DEFAULT_SPAN);
        assert_eq!(map.bytes_per_cell(), 256);
        map.touch(0x20000, 2, Toucher::Blitter, 10);
        let mut image = vec![0u32; CELLS];
        map.render(10, &mut image);
        let cell = 0x20000 / 256;
        assert_eq!(image[cell], Toucher::Blitter.colour());
        assert_eq!(image[cell + 1], 0xFF00_0000, "neighbours stay cold");
    }

    #[test]
    fn a_cell_fades_to_black_over_the_decay_window() {
        let mut map = HeatMap::new(0, DEFAULT_SPAN);
        map.touch(0x1000, 2, Toucher::CpuWrite, 0);
        let cell = 0x1000 / 256;
        let mut image = vec![0u32; CELLS];
        map.render(0, &mut image);
        let fresh = image[cell];
        map.render(u64::from(DECAY_FRAMES / 2), &mut image);
        let half = image[cell];
        assert!(half < fresh && half > 0xFF00_0000, "{half:08X}");
        map.render(u64::from(DECAY_FRAMES), &mut image);
        assert_eq!(image[cell], 0xFF00_0000, "a stale cell is black");
    }

    #[test]
    fn addresses_outside_the_window_are_ignored_not_wrapped() {
        let mut map = HeatMap::new(0, DEFAULT_SPAN);
        // A 32-bit machine's accelerator RAM is far above the window.
        map.touch(0x0800_0000, 4, Toucher::CpuWrite, 1);
        assert!(map.census(1).is_empty());
        // Move the window there and it is visible, with the old window's
        // record cleared rather than reinterpreted.
        map.set_window(0x0800_0000, 0x0100_0000);
        map.touch(0x0800_0000, 4, Toucher::CpuWrite, 1);
        assert_eq!(map.census(1), vec![(Toucher::CpuWrite, 1)]);
    }

    #[test]
    fn a_transfer_marks_every_cell_it_spans() {
        let mut map = HeatMap::new(0, DEFAULT_SPAN);
        map.touch(0x1000, 1024, Toucher::Disk, 5);
        assert_eq!(map.census(5), vec![(Toucher::Disk, 4)]);
    }

    #[test]
    fn a_transfer_running_past_the_window_marks_the_part_inside_it() {
        let mut map = HeatMap::new(0, DEFAULT_SPAN);
        let per_cell = map.bytes_per_cell();
        // Starts two cells from the top and runs well past the end.
        map.touch(
            map.span() - 2 * per_cell,
            16 * per_cell,
            Toucher::Blitter,
            0,
        );
        assert_eq!(map.census(0), vec![(Toucher::Blitter, 2)]);
    }

    #[test]
    fn a_cell_reports_its_toucher_and_the_frame_it_was_touched_on() {
        let mut map = HeatMap::new(0, DEFAULT_SPAN);
        map.touch(0x2000, 2, Toucher::Copper, 7);
        let cell = 0x2000 / map.bytes_per_cell() as usize;
        assert_eq!(map.cell(cell), Some((Toucher::Copper, 7)));
        assert_eq!(map.cell(cell + 1), None, "an untouched cell has no record");
        assert_eq!(map.cell(CELLS), None, "past the end of the grid");
        // The record outlives the fade: a cell that renders black still
        // reports what touched it, so a caller can name an age past the
        // decay window instead of only seeing black.
        let mut image = vec![0u32; CELLS];
        map.render(u64::from(7 + DECAY_FRAMES), &mut image);
        assert_eq!(image[cell], 0xFF00_0000);
        assert_eq!(map.cell(cell), Some((Toucher::Copper, 7)));
    }

    #[test]
    fn the_census_only_counts_cells_still_inside_the_decay_window() {
        let mut map = HeatMap::new(0, DEFAULT_SPAN);
        map.touch(0x1000, 2, Toucher::Copper, 0);
        assert_eq!(map.census(0), vec![(Toucher::Copper, 1)]);
        assert!(map.census(u64::from(DECAY_FRAMES)).is_empty());
    }
}
