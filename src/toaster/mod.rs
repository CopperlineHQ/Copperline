// SPDX-License-Identifier: GPL-3.0-or-later

//! NewTek Video Toaster 4000: the video-slot card's interface to the Amiga.
//!
//! The Toaster has no registers on the Amiga's bus. Its switcher software
//! talks to it through the picture: every scan line the Amiga displays can
//! carry a line header in sprites 0 and 1 (attached, at comparator position
//! `$5B`) and, in the bitplanes, a line of data for the card's framestore or
//! a request for data from it. The card answers requests on the parallel
//! port data pins (`$BFE101`), which the switcher reads at beam-timed
//! positions. The protocol here was recovered from NewTek's released
//! switcher source (`cop.a`, `autocal.a`, `vthand.a`, `elh.i`) and their
//! 4.2 binaries; see the project notes for the derivation.
//!
//! The card samples four digital lines per hi-res pixel, each wired to two
//! RGB bits of the video slot (NewTek's names; AGA low-nibble bits):
//!
//! | line | bits      | sprite word (header) | bitplane (data) |
//! |------|-----------|----------------------|-----------------|
//! | R    | R2 or R1  | sprite 0 A           | plane 0         |
//! | G    | B2 or G1  | sprite 0 B           | plane 1         |
//! | B    | R0 or B0  | sprite 1 A           | plane 2         |
//! | I    | G0 or B1  | sprite 1 B           | plane 3         |
//!
//! A data pixel's value is `R | G<<1 | B<<2 | I<<3`. The header's sixteen
//! lo-res sprite pixels give four 16-bit words (bit 15 first); NewTek uses
//! bits 15-8:
//!
//! - `R` bits 15-12: extended-header select (`%1111` = none, else one of
//!   eight codes), or on a read request the pixel-in-quad select.
//! - `G` bit 15: framestore write; bit 14: high nibble; bit 13: the
//!   colour-subcarrier-inverted ("complemented") copy; bit 12: field II/IV.
//! - `B`/`I` bits 15-12: line number (low/high nibble). A line that carries
//!   nothing has `R = %1111` with the write bit clear (the switcher gives it
//!   line number 255).
//!
//! A framestore line is written as four scan lines (normal high and low
//! nibbles, then the complemented pair); interlaced line `2L + 1` for
//! fields I/III, `2L` for II/IV. A read request draws a bitmap in which
//! every quad (four pixels, one colour clock) is labelled with the quad
//! number to return -- the `I` line carries the label's high nibble and `B`
//! its low nibble across the quad's four pixels -- and the card puts
//! framestore byte `4Q + pixel` of the requested line on the port a fixed
//! pipeline after quad `Q` passed its input. The extended line headers
//! (EH0-EH7, EH60-EH6F) hold the card's control state: the input muxes,
//! mattes, keyer, pedestal, gain, hue and the flag bits. EH0 is also the
//! header of every line of the switcher's normal display, so its bits
//! change within a field; the write bank comes from its `OBR` bit (reads
//! use it inverted), taken in beam order.
//!
//! Pinned against the 4.2 switcher's own tests: its bitmaps' first pixel
//! is the card's pixel 12 (quad 3), which puts pixel 0 at lo-res `$5A`,
//! two lo-res pixels before the header sprite; a quad label is the plain
//! quad number on the wire; and ten quads of read pipeline put each of
//! the `HS.default` read positions inside its label run.
//!
//! The card's outputs are composed on a thread of their own
//! ([`Toaster`] hands each finished field to a `toaster-compose` thread as
//! the frame's render input; `docs/internals/video.md`, "The Video
//! Toaster"). Everything the guest can observe -- the extended headers,
//! the framestore, the port -- is taken from the card's shared state
//! after the thread has caught up, so the picture the switcher reads back
//! does not depend on the host's speed.

pub mod composite;
pub mod dve;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod media;
pub mod perf;
pub mod regs;
pub mod video;

use crate::flyer::link::{FieldSource, FlyerLink};
use crate::video::bitplane::RenderInput;
use crate::videoslot::{BeamPos, FieldPixels, LiveField, VideoSlotDevice};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// Pixels per framestore line (the AGA switcher's 768-wide bitmap).
pub const LINE_PIXELS: usize = 768;
/// Interlaced framestore lines: the header's eight-bit line number covers
/// 256 lines per field (0-480 carry the picture; the switcher's read
/// self-test uses line 504, and 510/511 are the "dummy" line number it
/// puts on lines that carry nothing).
pub const LINES: usize = 512;

/// How far right the card stores a write line sent with EH0 `LUST` clear:
/// the switcher scrolls the second pass of its zipper write 16 hi-res
/// pixels left (cop.a, "shift left 16 HIRES pixels").
const ZIPPER_SHIFT: usize = 16;
/// DV1 and DV2.
pub const BANKS: usize = 2;
/// Normal and complemented copies of each line.
const SIGNALS: usize = 2;

/// Comparator position of the line header sprite.
const HEADER_LORES: i32 = 0x5B;
/// Comparator position of the card's pixel 0. Quad 0 is the two lo-res
/// pixels before the header sprite, the sprite's four used lo-res pixels
/// are quads 1-2, and the switcher's bitmaps -- whose first pixel
/// `TestReadScanLine` expects to read back as quad 3, pixel 0 -- start at
/// pixel 12. Measured against the 4.2 switcher: its written ramp's first
/// byte lands at this window's pixel 12.
const PIXEL0_LORES: i32 = 0x5A;
/// Hi-res pixels between a quad label passing the card's input and the
/// labelled byte being on the parallel port. Fitted to the switcher's
/// `HS.default` read positions: each of its ten reads per line must land
/// inside an 18-quad run of one label, and the CIA access (E-clock
/// synchronised) happens up to five clocks after the beam position it
/// waited for; ten quads keeps at least four quads of margin either way.
const READ_PIPELINE_PIXELS: i64 = 40;

/// Fields the compose thread may run behind the machine. The emulation
/// thread waits for a field's work only when the thread is still this
/// many fields behind, and takes a frame's sound only once its field is
/// that far back, so the sound reaches the mixer at the same emulated
/// time on every run, whatever the host's speed.
const PIPELINE_DEPTH: u64 = 2;

/// The extended headers as the switcher sets them up (`VTSetUp`), so the
/// card shows something sensible before it has been told otherwise.
const ELH_DEFAULTS: [(usize, Header); 10] = [
    (
        0,
        Header {
            r: 0x8E00,
            g: 0x0500,
            b: 0x0100,
            i: 0x0F00,
        },
    ),
    (
        1,
        Header {
            r: 0x4800,
            g: 0x0000,
            b: 0x0F00,
            i: 0x0F00,
        },
    ),
    (
        2,
        Header {
            r: 0xA000,
            g: 0x0000,
            b: 0x0000,
            i: 0x0F00,
        },
    ),
    (
        3,
        Header {
            r: 0x6000,
            g: 0x0000,
            b: 0x0000,
            i: 0x0F00,
        },
    ),
    (
        4,
        Header {
            r: 0x9F00,
            g: 0x0F00,
            b: 0x0700,
            i: 0x0F00,
        },
    ),
    (
        5,
        Header {
            r: 0x5800,
            g: 0x0800,
            b: 0x0000,
            i: 0x0F00,
        },
    ),
    (
        ELH_EH6_BASE + 8,
        Header {
            r: 0xB000,
            g: 0x0800,
            b: 0x0800,
            i: 0x0F00,
        },
    ),
    (
        ELH_EH6_BASE + 9,
        Header {
            r: 0xB000,
            g: 0x0800,
            b: 0x0900,
            i: 0x0F00,
        },
    ),
    (
        ELH_EH6_BASE + 0xA,
        Header {
            r: 0xB000,
            g: 0x0800,
            b: 0x0A00,
            i: 0x0F00,
        },
    ),
    (
        ELH_EH6_BASE + 0xB,
        Header {
            r: 0xB000,
            g: 0x0800,
            b: 0x0B00,
            i: 0x0F00,
        },
    ),
];

/// Number of extended line headers: EH0-EH5, EH60-EH6F, EH7.
const ELH_COUNT: usize = 23;
const ELH_EH0: usize = 0;
const ELH_EH6_BASE: usize = 6;
const ELH_EH7: usize = 22;

/// EH0 `R` word bit 8: output/write bank select.
const EH0R_OBR: u16 = 1 << 8;

/// One decoded line header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// An extended header: `index` into the ELH file.
    Elh { index: usize },
    /// A line carrying nothing (`I` nibble `$F`).
    Dummy,
    /// A framestore write.
    Write {
        line: usize,
        high_nibble: bool,
        complemented: bool,
    },
    /// A read request.
    Read {
        line: usize,
        pixel: usize,
        complemented: bool,
    },
}

/// The four header words of a line, bit 15 = the sprite's first pixel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Header {
    pub r: u16,
    pub g: u16,
    pub b: u16,
    pub i: u16,
}

impl Header {
    /// Extended-header select codes (`elh.i` `VTI_E*`), indexed as the ELH
    /// file is.
    fn elh_index(self) -> Option<usize> {
        let base = match self.r >> 12 {
            0b1000 => 0,
            0b0100 => 1,
            0b1010 => 2,
            0b0110 => 3,
            0b1001 => 4,
            0b0101 => 5,
            // EH6's page is in the B word (`VTI_E6x` = x << 8 there).
            0b1011 => ELH_EH6_BASE + usize::from((self.b >> 8) & 0xF),
            0b0111 => ELH_EH7,
            _ => return None,
        };
        // EH6's sixteen pages occupy the file between EH5 and EH7.
        Some(base)
    }

    pub fn kind(self) -> LineKind {
        if let Some(index) = self.elh_index() {
            return LineKind::Elh { index };
        }
        let l = usize::from(((self.i >> 12) << 4) | (self.b >> 12));
        let field_ii = self.g & 0x1000 != 0;
        let complemented = self.g & 0x2000 != 0;
        let line = 2 * l + usize::from(!field_ii);
        if self.g & 0x8000 != 0 {
            return LineKind::Write {
                line,
                high_nibble: self.g & 0x4000 != 0,
                complemented,
            };
        }
        // Pixel-in-quad codes %0000 %0010 %1100 %1110; anything else with
        // no extended header and no write bit carries nothing.
        let pixel = match self.r >> 12 {
            0b0000 => 0,
            0b0010 => 1,
            0b1100 => 2,
            0b1110 => 3,
            _ => return LineKind::Dummy,
        };
        LineKind::Read {
            line,
            pixel,
            complemented,
        }
    }
}

/// The four-bit value the card's input lines read from one RGBA8 pixel.
#[inline]
pub fn lines_of(pixel: u32) -> u8 {
    let [r, g, b, _] = pixel.to_le_bytes();
    let line_r = (r & 0x06) != 0;
    let line_g = (b & 0x04) != 0 || (g & 0x02) != 0;
    let line_b = (r & 0x01) != 0 || (b & 0x01) != 0;
    let line_i = (g & 0x01) != 0 || (b & 0x02) != 0;
    u8::from(line_r) | (u8::from(line_g) << 1) | (u8::from(line_b) << 2) | (u8::from(line_i) << 3)
}

/// Per-field activity, for diagnostics and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub fields: u64,
    pub elh_lines: u64,
    pub write_lines: u64,
    pub read_requests: u64,
    pub port_bytes: u64,
    /// Pictures the card digitised (`CERB` outside a write) and later
    /// encoded into the framestore because something read the bank.
    pub grabs_encoded: u64,
}

/// The card's two framestore banks: `[bank][copy][line][pixel]` composite
/// samples, the normal and the complemented copy of every line.
pub struct FrameStore {
    data: Vec<u8>,
}

impl FrameStore {
    fn new() -> Self {
        Self {
            data: vec![0; BANKS * SIGNALS * LINES * LINE_PIXELS],
        }
    }

    fn index(bank: usize, complemented: bool, line: usize, pixel: usize) -> usize {
        ((bank * SIGNALS + usize::from(complemented)) * LINES + line) * LINE_PIXELS + pixel
    }

    /// One line of one copy of one bank.
    pub fn line(&self, bank: usize, complemented: bool, line: usize) -> &[u8] {
        let i = Self::index(bank & 1, complemented, line % LINES, 0);
        &self.data[i..i + LINE_PIXELS]
    }

    pub fn line_mut(&mut self, bank: usize, complemented: bool, line: usize) -> &mut [u8] {
        let i = Self::index(bank & 1, complemented, line % LINES, 0);
        &mut self.data[i..i + LINE_PIXELS]
    }
}

/// A rendered field the card owns: a [`FieldPixels`] copied for a job.
pub struct OwnedField {
    pixels: Vec<u32>,
    width: usize,
    rows: usize,
    visible_start_vpos: u32,
    first_cck: i32,
    canvas_scale: usize,
    long_frame: bool,
    interlaced: bool,
    valid_rows: usize,
}

impl OwnedField {
    pub fn copy_of(field: &FieldPixels<'_>) -> Self {
        let n = (field.width * field.rows).min(field.pixels.len());
        Self {
            pixels: field.pixels[..n].to_vec(),
            width: field.width,
            rows: field.rows,
            visible_start_vpos: field.visible_start_vpos,
            first_cck: field.first_cck,
            canvas_scale: field.canvas_scale,
            long_frame: field.long_frame,
            interlaced: field.interlaced,
            valid_rows: field.valid_rows,
        }
    }

    pub fn field(&self) -> FieldPixels<'_> {
        FieldPixels {
            pixels: &self.pixels,
            width: self.width,
            rows: self.rows,
            visible_start_vpos: self.visible_start_vpos,
            first_cck: self.first_cck,
            canvas_scale: self.canvas_scale,
            long_frame: self.long_frame,
            interlaced: self.interlaced,
            valid_rows: self.valid_rows,
        }
    }
}

/// What a field reaches the compose thread as.
pub enum FieldInput {
    /// The frame's render input: the thread paints the field itself.
    Render(Box<RenderInput>),
    /// A field already painted.
    Owned(OwnedField),
}

/// What the Flyer did during a field, taken on the emulation thread when
/// the field is handed over: see [`crate::flyer::link`].
struct FlyerState {
    routed: [bool; 2],
    shown: [Vec<FieldSource>; 2],
    sound: Vec<(f32, f32)>,
}

/// One finished field, for the compose thread.
struct Job {
    id: u64,
    input: FieldInput,
    at: BeamPos,
    /// Lines of the field the live scan already took in.
    first_unscanned: u32,
    flyer: Option<FlyerState>,
}

/// What a field's work produced for the host.
struct JobResult {
    id: u64,
    /// Program's sound, when the field completed a frame.
    sound: Vec<(f32, f32)>,
    /// Peak of that sound, for the Flyer's meters.
    peak: Option<(f32, f32)>,
}

#[derive(Default)]
struct PipeState {
    queue: VecDeque<Job>,
    /// The id of the last job finished (ids count from 1).
    completed: u64,
    results: VecDeque<JobResult>,
    /// Render inputs the thread is done with, for the bus to refill.
    spent: Vec<Box<RenderInput>>,
    stop: bool,
}

/// The queue between the emulation thread and the compose thread.
#[derive(Default)]
struct Pipe {
    state: Mutex<PipeState>,
    /// Signalled when a job is queued and when one is finished.
    changed: Condvar,
}

impl Pipe {
    fn lock(&self) -> MutexGuard<'_, PipeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queue `job`; back-pressure: wait until the thread is at most
    /// [`PIPELINE_DEPTH`] fields behind.
    fn submit(&self, job: Job) {
        let mut st = self.lock();
        let wait = perf::start();
        while st.completed + PIPELINE_DEPTH < job.id && !st.stop {
            st = self.changed.wait(st).unwrap_or_else(|e| e.into_inner());
        }
        wait.stop(perf::COMPOSE_WAIT);
        st.queue.push_back(job);
        drop(st);
        self.changed.notify_all();
    }

    /// Wait until every queued job is done.
    fn drain(&self, submitted: u64) {
        let mut st = self.lock();
        let wait = perf::start();
        while st.completed < submitted && !st.stop {
            st = self.changed.wait(st).unwrap_or_else(|e| e.into_inner());
        }
        wait.stop(perf::COMPOSE_WAIT);
    }

    /// The compose thread: fields in order, each painted (when it comes as
    /// a render input) and taken in by the card.
    fn work(&self, card: &Mutex<Card>) {
        let mut fb = Vec::new();
        let mut scratch = Vec::new();
        loop {
            let job = {
                let mut st = self.lock();
                loop {
                    if st.stop {
                        return;
                    }
                    if let Some(job) = st.queue.pop_front() {
                        break job;
                    }
                    st = self.changed.wait(st).unwrap_or_else(|e| e.into_inner());
                }
            };
            let (result, spent) = run_job(card, job, &mut fb, &mut scratch);
            let mut st = self.lock();
            st.completed = result.id;
            st.results.push_back(result);
            if let Some(input) = spent {
                st.spent.push(input);
            }
            drop(st);
            self.changed.notify_all();
        }
    }
}

/// Do one job: paint the field if it came as a render input, then hand it
/// to the card. Returns the result and the render input, now spent.
fn run_job(
    card: &Mutex<Card>,
    job: Job,
    fb: &mut Vec<u32>,
    scratch: &mut Vec<u32>,
) -> (JobResult, Option<Box<RenderInput>>) {
    let Job {
        id,
        input,
        at,
        first_unscanned,
        flyer,
    } = job;
    let mut card = card.lock().unwrap_or_else(|e| e.into_inner());
    match input {
        FieldInput::Owned(owned) => {
            let result = card.take_field(&owned.field(), at, first_unscanned, flyer, id);
            (result, None)
        }
        FieldInput::Render(mut input) => {
            let timer = perf::start();
            let wide = crate::video::bitplane::render_wide(
                &mut input,
                crate::videoslot::SLOT_FIRST_CCK,
                crate::videoslot::SLOT_LINE_CCK,
                usize::MAX,
                fb,
                scratch,
            );
            timer.stop(perf::FIELD_RENDER);
            // LOF has already toggled for the new field on a laced
            // screen; the completed field was the other one.
            let long_frame = if wide.interlaced {
                !at.long_frame
            } else {
                at.long_frame
            };
            let field = FieldPixels {
                pixels: fb,
                width: wide.stride,
                rows: wide.rows,
                visible_start_vpos: wide.visible_start_vpos,
                first_cck: crate::videoslot::SLOT_FIRST_CCK,
                canvas_scale: wide.canvas_scale,
                long_frame,
                interlaced: wide.interlaced,
                valid_rows: wide.rows,
            };
            let result = card.take_field(&field, at, first_unscanned, flyer, id);
            // The frame's buffers go back to the machine as soon as the
            // field is painted.
            input.release_shared_buffers();
            (result, Some(input))
        }
    }
}

/// The card proper: its framestore, registers, video side and the state
/// of a frame being composed. Shared between [`Toaster`] (the host's and
/// the guest's side) and the compose thread, which holds it while it
/// works on a field.
pub struct Card {
    frame_store: FrameStore,
    /// A picture the card digitised into a bank (the `IS` input, live or a
    /// grab) that is not yet encoded into it: encoding costs milliseconds
    /// a field and a live input is replaced every field, so it is done
    /// when something reads the bank (`settle`). What the bank holds is
    /// the same either way.
    pending_grab: [Option<Vec<u32>>; 2],
    video: video::Video,
    /// Lines this field that asked the card to record its input (`CERB`
    /// outside a framestore write).
    record_lines: u32,
    /// Raw extended line headers, last value written.
    elh: [Header; ELH_COUNT],
    /// Seen a valid extended header: the switcher is driving the card.
    active: bool,
    stats: Stats,
    diag: bool,
    diag_lines_logged: u32,
    diag_fields: bool,
    diag_outputs: String,
    /// Show the switcher's interface over Preview when it asks to
    /// (`USERON`, the Toaster's "preview overlay" for one-monitor setups).
    preview_overlay: bool,
    /// The digital effect's addressing for the frame being composed, and
    /// whether the frame has one.
    dve: dve::DveMap,
    dve_on: bool,
    /// The Video Flyer beside the card: it can take over inputs 3 and 4,
    /// and its sound is the system's.
    flyer: Option<Arc<FlyerLink>>,
    /// What the Flyer did, as the fields' jobs reported it: the routing,
    /// what each channel showed since the last frame, and its sound.
    flyer_routed: [bool; 2],
    flyer_shown: [Vec<FieldSource>; 2],
    flyer_sound: Vec<(f32, f32)>,
    /// Per-pixel scratch, reused from frame to frame.
    key_buf: Vec<u8>,
    codes_buf: Vec<Option<u8>>,
}

/// The NewTek Video Toaster 4000 in the video slot: the host's handle on
/// the [`Card`] and the thread that composes its outputs.
pub struct Toaster {
    card: Arc<Mutex<Card>>,
    pipe: Arc<Pipe>,
    worker: Option<std::thread::JoinHandle<()>>,
    outputs: Vec<Arc<crate::videoslot::CardOutput>>,
    flyer: Option<Arc<FlyerLink>>,
    /// Lines of the field in progress already taken in through the live
    /// scan: `(field number, next line to take)`.
    scanned: Option<(u64, u32)>,
    /// Jobs handed over, and the last one known finished.
    submitted: u64,
    joined: u64,
}

impl Default for Toaster {
    fn default() -> Self {
        Self::new()
    }
}

impl Toaster {
    /// A card with no host folder: no inputs, outputs not recorded to a
    /// folder of their own.
    pub fn new() -> Self {
        Self::with_media(None)
    }

    /// A card whose connectors are the folders under `media_root`
    /// (created if missing): see [`media`].
    pub fn with_media(media_root: Option<std::path::PathBuf>) -> Self {
        let card = Card::new(media_root);
        let outputs = card.video.outputs();
        let card = Arc::new(Mutex::new(card));
        let pipe = Arc::new(Pipe::default());
        let worker = {
            let pipe = Arc::clone(&pipe);
            let card = Arc::clone(&card);
            std::thread::Builder::new()
                .name("toaster-compose".into())
                .spawn(move || pipe.work(&card))
                .ok()
        };
        if worker.is_none() {
            log::warn!("toaster: no compose thread; composing outputs on the emulation thread");
        }
        Self {
            card,
            pipe,
            worker,
            outputs,
            flyer: None,
            scanned: None,
            submitted: 0,
            joined: 0,
        }
    }

    /// Fit a Video Flyer beside the card: inputs 3 and 4 show its channels
    /// A and B when it routes them there, and its sound joins Program's.
    pub fn set_flyer_link(&mut self, link: Arc<FlyerLink>) {
        self.flyer = Some(Arc::clone(&link));
        self.card().flyer = Some(link);
    }

    /// The card, once the compose thread has caught up: the state the
    /// guest sees. Not to be held across a field.
    pub(crate) fn card(&mut self) -> MutexGuard<'_, Card> {
        self.drain();
        self.card.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn drain(&mut self) {
        if self.joined < self.submitted {
            if self.worker_alive() {
                self.pipe.drain(self.submitted);
            }
            self.joined = self.submitted;
        }
    }

    /// The card for the host's side (a status query, a capture, a control
    /// command), once the compose thread has caught up. Unlike
    /// [`Toaster::card`] this is not an event of the emulated timeline, so
    /// it waits without recording that it did: `joined` decides at which
    /// field a frame's sound is taken, and that must be the same field
    /// whether or not anyone looked.
    fn host_card(&mut self) -> MutexGuard<'_, Card> {
        self.settle();
        self.card.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn settle(&mut self) {
        if self.joined < self.submitted && self.worker_alive() {
            self.pipe.drain(self.submitted);
        }
    }

    pub fn stats(&mut self) -> Stats {
        self.card().stats
    }

    pub fn elh(&mut self, index: usize) -> Header {
        self.card().elh[index]
    }

    /// Show the switcher's interface over the Preview output when it asks
    /// for that (its "preview overlay", meant for a single composite
    /// monitor). Off by default: the interface has a window of its own.
    pub fn set_preview_overlay(&mut self, on: bool) {
        self.card().preview_overlay = on;
    }

    /// One framestore byte.
    pub fn frame_store_byte(
        &mut self,
        bank: usize,
        complemented: bool,
        line: usize,
        pixel: usize,
    ) -> u8 {
        self.card()
            .frame_store_byte(bank, complemented, line, pixel)
    }

    /// Hand a finished field to the compose thread. Jobs run in order; a
    /// job is waited for only when the thread is [`PIPELINE_DEPTH`] fields
    /// behind. Without a thread the job runs here.
    fn submit(&mut self, input: FieldInput, at: BeamPos) {
        let timer = perf::start();
        // Lines the live scan already took in this field are done.
        let first_unscanned = match self.scanned.take() {
            Some((frame, next)) if frame == at.frame => next,
            _ => 0,
        };
        // What the Flyer did since the last field, taken now so the
        // frame composed later sees the same whatever the thread's pace.
        let flyer = self.flyer.as_ref().map(|link| FlyerState {
            routed: link.routing(),
            shown: [link.take_shown(0), link.take_shown(1)],
            sound: link.take_sound(),
        });
        self.submitted += 1;
        let job = Job {
            id: self.submitted,
            input,
            at,
            first_unscanned,
            flyer,
        };
        if self.worker_alive() {
            self.pipe.submit(job);
            self.joined = self.joined.max(job_id_joined(self.submitted));
        } else {
            self.run_inline(job);
            self.joined = self.submitted;
        }
        timer.stop(perf::SUBMIT);
    }

    /// Whether the compose thread is there to take jobs. One that died
    /// (a panic) is reported once; from then on jobs run here.
    fn worker_alive(&mut self) -> bool {
        match &self.worker {
            Some(worker) if !worker.is_finished() => true,
            Some(_) => {
                if let Some(worker) = self.worker.take() {
                    if worker.join().is_err() {
                        log::error!(
                            "toaster: the compose thread died; composing on the emulation thread"
                        );
                    }
                }
                // Jobs it left behind are run here, in order.
                let left: Vec<Job> = self.pipe.lock().queue.drain(..).collect();
                for job in left {
                    self.run_inline(job);
                }
                false
            }
            None => false,
        }
    }

    /// Do a job on this thread.
    fn run_inline(&mut self, job: Job) {
        let mut fb = Vec::new();
        let mut scratch = Vec::new();
        let (result, spent) = run_job(&self.card, job, &mut fb, &mut scratch);
        let mut st = self.pipe.lock();
        st.completed = result.id;
        st.results.push_back(result);
        st.spent.extend(spent);
    }

    /// Scan the lines of the field in progress that the beam has passed
    /// since the last scan, so a read on line N sees the headers (bank
    /// select, above all) of lines 0..N of its own field, as the card
    /// does. The compose thread has caught up first: the field before is
    /// in.
    fn scan_to_beam(&mut self, live: &mut LiveField<'_>) {
        let beam = live.beam();
        let mut next = match self.scanned {
            Some((frame, next)) if frame == beam.frame => next,
            _ => live.first_vpos(),
        };
        let timer = perf::start();
        let mut lines = 0;
        let mut card = self.card();
        while next < beam.vpos {
            if let Some(field) = live.line(next) {
                lines += 1;
                if let Some(header) = Card::header(&field, next) {
                    card.take_line(&field, next, header);
                }
            }
            next += 1;
        }
        drop(card);
        perf::count(perf::SCAN, lines);
        timer.stop(perf::SCAN);
        self.scanned = Some((beam.frame, next));
    }
}

/// The job an emulation thread that just submitted job `submitted` knows
/// to be finished.
fn job_id_joined(submitted: u64) -> u64 {
    submitted.saturating_sub(PIPELINE_DEPTH)
}

impl Drop for Toaster {
    fn drop(&mut self) {
        self.pipe.lock().stop = true;
        self.pipe.changed.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Card {
    fn new(media_root: Option<std::path::PathBuf>) -> Self {
        let mut elh = [Header::default(); ELH_COUNT];
        for (index, header) in ELH_DEFAULTS {
            elh[index] = header;
        }
        Self {
            frame_store: FrameStore::new(),
            pending_grab: [None, None],
            video: video::Video::new(media_root),
            record_lines: 0,
            elh,
            active: false,
            stats: Stats::default(),
            diag: crate::envcfg::flag("COPPERLINE_DIAG_TOASTER"),
            diag_lines_logged: 0,
            diag_fields: crate::envcfg::var_os("COPPERLINE_DIAG_TOASTER")
                .is_some_and(|v| v == "fields"),
            diag_outputs: String::new(),
            preview_overlay: false,
            dve: dve::DveMap::empty(),
            dve_on: false,
            flyer: None,
            flyer_routed: [false; 2],
            flyer_shown: [Vec::new(), Vec::new()],
            flyer_sound: Vec::new(),
            key_buf: Vec::new(),
            codes_buf: Vec::new(),
        }
    }

    /// The Flyer channel on input `n` (0-based), when the Flyer has it:
    /// channel A on input 3, B on input 4.
    fn flyer_channel(&self, n: usize) -> Option<usize> {
        let ch = n.checked_sub(2).filter(|&c| c < 2)?;
        (self.flyer.is_some() && self.flyer_routed[ch]).then_some(ch)
    }

    /// Input `n`'s picture into `out`: the Flyer's channel where it has
    /// the input, else the input's host folder. False when nothing is
    /// plugged in.
    fn input_into(&mut self, n: usize, out: &mut [u32]) -> bool {
        if let (Some(ch), Some(link)) = (self.flyer_channel(n), &self.flyer) {
            let shown = std::mem::take(&mut self.flyer_shown[ch]);
            link.weave_shown(ch, &shown, out);
            return true;
        }
        match self.video.input_picture(n) {
            Some(p) => {
                out.copy_from_slice(&p[..out.len()]);
                true
            }
            None => false,
        }
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    pub fn elh(&self, index: usize) -> Header {
        self.elh[index]
    }

    /// The latched control registers.
    pub fn regs(&self) -> regs::Regs<'_> {
        regs::Regs { elh: &self.elh }
    }

    /// One framestore byte.
    pub fn frame_store_byte(
        &mut self,
        bank: usize,
        complemented: bool,
        line: usize,
        pixel: usize,
    ) -> u8 {
        self.settle(bank);
        self.frame_store.line(bank, complemented, line)[pixel]
    }

    /// Encode the picture digitised into `bank`, if one is waiting, before
    /// the bank is read or written.
    fn settle(&mut self, bank: usize) {
        if let Some(pic) = self.pending_grab[bank & 1].take() {
            let timer = perf::start();
            video::Video::grab(&mut self.frame_store, bank, &pic);
            self.video.give_back(pic);
            self.stats.grabs_encoded += 1;
            timer.stop(perf::GRAB_ENCODE);
        }
    }

    /// Bank a write goes to: EH0 `OBR`.
    fn write_bank(&self) -> usize {
        usize::from(self.elh[ELH_EH0].r & EH0R_OBR != 0)
    }

    /// Bank a read comes from: the switcher sets `OBR` for bank 0.
    fn read_bank(&self) -> usize {
        usize::from(self.elh[ELH_EH0].r & EH0R_OBR == 0)
    }

    /// Sample the line header sprite of row `vpos`.
    fn header(field: &FieldPixels<'_>, vpos: u32) -> Option<Header> {
        let row = field.row(vpos)?;
        // The serializer's first pixel lands one lo-res pixel after the
        // comparator match.
        let x0 = field.column_for_lores(HEADER_LORES + crate::bus::SPRITE_OUTPUT_DELAY_LORES);
        let step = 2 * field.canvas_scale as i64;
        let mut h = Header::default();
        for k in 0..16u32 {
            let col = usize::try_from(x0 + i64::from(k) * step).ok()?;
            let v = lines_of(*row.get(col)?);
            let bit = 15 - k;
            h.r |= u16::from(v & 1) << bit;
            h.g |= u16::from((v >> 1) & 1) << bit;
            h.b |= u16::from((v >> 2) & 1) << bit;
            h.i |= u16::from((v >> 3) & 1) << bit;
        }
        Some(h)
    }

    /// Canvas column of the card's pixel 0.
    fn pixel0_column(field: &FieldPixels<'_>) -> i64 {
        field.column_for_lores(PIXEL0_LORES)
    }

    fn store_write_line(
        &mut self,
        field: &FieldPixels<'_>,
        vpos: u32,
        line: usize,
        high_nibble: bool,
        complemented: bool,
    ) {
        let Some(row) = field.row(vpos) else {
            return;
        };
        if line >= LINES {
            return;
        }
        let timer = perf::start();
        let bank = self.write_bank();
        self.settle(bank);
        self.video.invalidate(bank);
        let x0 = Self::pixel0_column(field);
        let step = field.canvas_scale as i64;
        // The second pass of a zipper write (LUST clear) lands 16 samples
        // right of where its pixels are on the Amiga line; the header
        // pixels left of the bitmap stay out of the picture.
        let shift = if self.regs().lust() { 0 } else { ZIPPER_SHIFT };
        let store = self.frame_store.line_mut(bank, complemented, line);
        for (p, byte) in store.iter_mut().enumerate() {
            let Some(src) = p.checked_sub(shift) else {
                continue;
            };
            if shift != 0 && src < video::PICTURE_X0 {
                continue;
            }
            let Ok(col) = usize::try_from(x0 + src as i64 * step) else {
                continue;
            };
            let Some(&pixel) = row.get(col) else {
                break;
            };
            let v = lines_of(pixel);
            *byte = if high_nibble {
                (*byte & 0x0F) | (v << 4)
            } else {
                (*byte & 0xF0) | v
            };
        }
        self.stats.write_lines += 1;
        timer.stop(perf::WRITE_LINES);
        if self.diag && self.diag_lines_logged < 400 {
            self.diag_lines_logged += 1;
            let bytes: Vec<u8> = self.frame_store.line(bank, complemented, line)[8..56].to_vec();
            log::info!(
                "toaster: write vpos={vpos} line={line} hi={high_nibble} comp={complemented} bank={bank} pixels 8.. {bytes:02X?}"
            );
        }
    }

    /// Act on one line's header: latch an extended header, store a written
    /// line, count a read request.
    fn take_line(&mut self, field: &FieldPixels<'_>, vpos: u32, header: Header) {
        match header.kind() {
            LineKind::Elh { index } => {
                if !self.active {
                    log::info!(
                        "toaster: first extended header on line {vpos}: the switcher is driving the card"
                    );
                    self.video.start_inputs();
                }
                self.active = true;
                if self.diag && self.elh[index] != header {
                    log::info!(
                        "toaster: ELH {index} <- R={:04X} G={:04X} B={:04X} I={:04X} (vpos {vpos})",
                        header.r,
                        header.g,
                        header.b,
                        header.i
                    );
                }
                self.elh[index] = header;
                self.stats.elh_lines += 1;
            }
            // `G` bit 15 is CERB, "record this line". In write mode (EH0
            // WERB set, ABSE clear) what is recorded is the Amiga's data on
            // the line; otherwise it is the input the IS mux selects: live
            // video, or a grab.
            LineKind::Write {
                line,
                high_nibble,
                complemented,
            } if self.active && self.regs().write_mode() => {
                self.store_write_line(field, vpos, line, high_nibble, complemented);
            }
            LineKind::Write { .. } if self.active => {
                self.record_lines += 1;
            }
            LineKind::Read { .. } if self.active => {
                self.stats.read_requests += 1;
            }
            _ => {}
        }
    }

    /// The quad label drawn at canvas column `col` on `row`: the `I` line
    /// across the quad's four pixels is the label's high nibble, the `B`
    /// line its low nibble, first pixel = bit 3. (The switcher's source
    /// talks of complementing the label data, but that cancels against its
    /// inverted read palette: on the wire the label is the quad number.)
    fn quad_label(row: &[u32], quad_col: i64, scale: i64) -> Option<u8> {
        let mut hi = 0u8;
        let mut lo = 0u8;
        for j in 0..4i64 {
            let col = usize::try_from(quad_col + j * scale).ok()?;
            let v = lines_of(*row.get(col)?);
            hi |= ((v >> 3) & 1) << (3 - j);
            lo |= ((v >> 2) & 1) << (3 - j);
        }
        Some((hi << 4) | lo)
    }

    /// Take in a finished field (job `id`): its line headers from
    /// `first_unscanned` on, then, on the second field of a frame, the
    /// outputs. `at` is the beam at the wrap.
    fn take_field(
        &mut self,
        field: &FieldPixels<'_>,
        at: BeamPos,
        first_unscanned: u32,
        flyer: Option<FlyerState>,
        id: u64,
    ) -> JobResult {
        let timer = perf::start();
        self.stats.fields += 1;
        if let Some(flyer) = flyer {
            self.flyer_routed = flyer.routed;
            for (ch, shown) in flyer.shown.into_iter().enumerate() {
                for source in shown {
                    crate::flyer::link::push_shown(&mut self.flyer_shown[ch], source);
                }
            }
            self.flyer_sound.extend_from_slice(&flyer.sound);
        }
        // `COPPERLINE_DIAG_TOASTER=fields`: dump every line header of one
        // field in forty.
        let dump = self.diag_fields && self.active && self.stats.fields.is_multiple_of(40);
        let scan = perf::start();
        for row in 0..field.rows {
            let vpos = field.visible_start_vpos + row as u32;
            if vpos < first_unscanned {
                continue;
            }
            let Some(header) = Self::header(field, vpos) else {
                continue;
            };
            if dump {
                log::info!(
                    "toaster: field {} line {vpos}: R={:04X} G={:04X} B={:04X} I={:04X} {:?}",
                    self.stats.fields,
                    header.r,
                    header.g,
                    header.b,
                    header.i,
                    header.kind()
                );
            }
            self.take_line(field, vpos, header);
        }
        scan.stop(perf::HEADER_SCAN);
        // A field with CERB lines outside a framestore write digitised the
        // IS input into the recording bank: live video, or a grab.
        let record = std::mem::take(&mut self.record_lines) > 0;
        self.video.set_live(record && self.regs().abse());
        let frame_done = self.video.begin_field();
        let mut result = JobResult {
            id,
            sound: Vec::new(),
            peak: None,
        };
        if frame_done {
            let outputs = perf::start();
            if self.active {
                self.produce_outputs(field, &mut result);
            } else {
                self.produce_idle_outputs(&mut result);
            }
            outputs.stop(perf::OUTPUTS);
        }
        if record {
            let rec = perf::start();
            self.record_input(field);
            rec.stop(perf::RECORD_INPUT);
        }
        timer.stop(perf::FIELD_COMPLETE);
        if perf::on() && self.stats.fields.is_multiple_of(60) {
            let flyer = self
                .flyer
                .as_ref()
                .map(|l| l.decode_stats())
                .unwrap_or_default();
            log::info!(
                "toaster perf: field {} {} flyer decoded={} waited={} at {:?}",
                self.stats.fields,
                perf::report(),
                flyer.0,
                flyer.1,
                at.frame
            );
        }
        result
    }
}

impl VideoSlotDevice for Toaster {
    fn name(&self) -> &'static str {
        "Video Toaster 4000"
    }

    fn supplies_sync(&self) -> bool {
        true
    }

    fn takes_render_input(&self) -> bool {
        true
    }

    fn settle_outputs(&mut self) {
        self.settle();
    }

    fn field_render_input(&mut self, input: Box<RenderInput>, at: BeamPos) {
        self.submit(FieldInput::Render(input), at);
    }

    fn take_spent_render_inputs(&mut self, pool: &mut Vec<Box<RenderInput>>) {
        let mut st = self.pipe.lock();
        pool.append(&mut st.spent);
    }

    /// A field already painted: taken in on the compose thread like any
    /// other, and waited for, so what follows sees its effect.
    fn field_complete(&mut self, field: &FieldPixels<'_>, at: BeamPos) {
        self.submit(FieldInput::Owned(OwnedField::copy_of(field)), at);
        self.drain();
    }

    fn video_outputs(&self) -> Vec<Arc<crate::videoslot::CardOutput>> {
        self.outputs.clone()
    }

    fn control_status(&mut self) -> serde_json::Value {
        self.host_card().status_value()
    }

    /// Program's sound of the frames whose fields are known finished: a
    /// frame's sound is taken [`PIPELINE_DEPTH`] fields after its own, on
    /// every run.
    fn take_audio(&mut self, out: &mut Vec<(f32, f32)>) {
        let mut st = self.pipe.lock();
        while st.results.front().is_some_and(|r| r.id <= self.joined) {
            let Some(result) = st.results.pop_front() else {
                break;
            };
            out.extend_from_slice(&result.sound);
            if let (Some(peak), Some(link)) = (result.peak, &self.flyer) {
                link.set_line_peak(peak);
            }
        }
    }

    fn control(
        &mut self,
        command: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        match command {
            "input" => {
                let n = params
                    .get("input")
                    .and_then(serde_json::Value::as_u64)
                    .filter(|n| (1..=media_inputs()).contains(&(*n as usize)))
                    .ok_or_else(|| format!("input must be 1-{}", media_inputs()))?
                    as usize;
                let file = match params.get("file") {
                    None | Some(serde_json::Value::Null) => None,
                    Some(serde_json::Value::String(f)) if f.is_empty() => None,
                    Some(serde_json::Value::String(f)) => {
                        let path = std::path::PathBuf::from(f);
                        if !path.is_file() {
                            return Err(format!("{f}: no such file"));
                        }
                        Some(path)
                    }
                    Some(_) => return Err("file must be a path string or null".into()),
                };
                let card = self.host_card();
                card.video.pin_input(n - 1, file)?;
                Ok(serde_json::json!({ "inputs": card.video.input_status() }))
            }
            other => Err(format!("the toaster has no {other} control")),
        }
    }

    fn parallel_read(&mut self, live: &mut LiveField<'_>, _ddrb: u8) -> Option<u8> {
        let timer = perf::start();
        self.scan_to_beam(live);
        let mut card = self.card();
        if !card.active {
            drop(card);
            timer.stop(perf::PAR_READ);
            return None;
        }
        let beam = live.beam();
        let port = perf::start();
        let field = live.pixels();
        let byte = card.port_byte(&field, beam);
        drop(card);
        port.stop(perf::PORT_BYTE);
        timer.stop(perf::PAR_READ);
        byte
    }
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn media_inputs() -> usize {
    media::INPUTS
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn media_inputs() -> usize {
    4
}

/// How much of each video input an output shows (0-1 each): the sound
/// follows the picture.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Inputs([f32; 4]);

impl Inputs {
    fn add(&mut self, other: &Inputs, weight: f32) {
        for (a, b) in self.0.iter_mut().zip(other.0) {
            *a += b * weight;
        }
    }
}

/// The fader's mix for a frame: one value, or the per-pixel map in
/// `Card::key_buf`.
enum FaderKey {
    Flat(u8),
    Map,
}

/// The folder, under the card's host folder, through which the host hands
/// ARexx jobs to the Amiga (S:Copperline-Link.rexx runs them).
pub const LINK_FOLDER: &str = ".copperline";

impl Card {
    /// The card's state as the control protocol reports it.
    fn status_value(&self) -> serde_json::Value {
        use regs::{Cds, Mux};
        let r = self.regs();
        let mux = |m: Mux, seven: &str| -> String {
            match m {
                Mux::Vid(n) => format!("Input {}", n + 1),
                Mux::Dac(b) => format!("DV{}", b + 1),
                Mux::Encoder if r.matte() => "Matte".to_string(),
                Mux::Encoder => "Amiga".to_string(),
                Mux::Seven => seven.to_string(),
            }
        };
        let cds = match r.cds() {
            Cds::Quaf => "digital effect (QUAF)",
            Cds::PixelSw => "Amiga transparency (PIXELSW)",
            Cds::TransQuad => "digital effect, drop shadow (TRANSQUAD)",
            Cds::LumKey => "luminance key (LUMKEY)",
            Cds::Dibgr => "Amiga 4-bit key (DIBGR)",
            Cds::Dib => "Amiga 2-bit wipe (DIB)",
            Cds::RgQuad => "RGQUAD",
            Cds::Cd => "mix (CD)",
        };
        let (ma, mb, mp) = r.matte_code();
        let outputs: Vec<serde_json::Value> = self
            .video
            .outputs()
            .iter()
            .map(|o| {
                serde_json::json!({
                    "name": o.name(),
                    "recording": o.recording_path().map(|p| p.display().to_string()),
                })
            })
            .collect();
        let root = self.video.media_root();
        serde_json::json!({
            "card": "NewTek Video Toaster 4000",
            "switcher_active": self.active,
            "program": {
                "a": if r.amwipe() && r.am() == Mux::Seven {
                    "per pixel (latch effect)".to_string()
                } else {
                    mux(r.am(), "Mono")
                },
                "b": if r.bmwipe() && r.bm() == Mux::Seven {
                    "per pixel (latch effect)".to_string()
                } else {
                    mux(r.bm(), "Mono")
                },
                "fader": r.cd(),
                "fader_note": "0 = all A, 255 = all B; the T-bar and AUTO move it",
                "mode": cds,
            },
            "preview": mux(r.pv(), "Program"),
            "framestore_input": mux(r.is(), "Program"),
            "key_source": mux(r.lk(), "External"),
            "luma_key_level": r.lka(),
            "matte": {"a": ma, "b": mb, "hue": mp},
            "live_framestore": r.abse(),
            "outputs": outputs,
            "inputs": self.video.input_status(),
            "flyer": self.flyer.as_ref().map(|_| serde_json::json!({
                "input3": self.flyer_channel(2).is_some(),
                "input4": self.flyer_channel(3).is_some(),
            })),
            "media_root": root.map(|p| p.display().to_string()),
            "link_folder": root.map(|p| p.join(LINK_FOLDER).display().to_string()),
        })
    }

    /// What mux position `mux` carries, as a picture, into `out`.
    /// `role` says which mux it is: position 7 means a different thing on
    /// each.
    fn source_picture(
        &mut self,
        mux: regs::Mux,
        role: video::Role,
        field: &FieldPixels<'_>,
        out: &mut [u32],
    ) {
        use regs::Mux;
        match (mux, role) {
            (Mux::Vid(n), _) => {
                let timer = perf::start();
                let flyer = self.flyer_channel(n).is_some();
                if !self.input_into(n, out) {
                    out.fill(video::black());
                }
                timer.stop(if flyer {
                    perf::SRC_FLYER
                } else {
                    perf::SRC_INPUT
                });
            }
            // On the IS mux, 4 is the Program output fed back and 5 the
            // external input (DAC0).
            (Mux::Dac(0), video::Role::Is) => self.video.last_program(out),
            (Mux::Dac(1), video::Role::Is) => {
                let timer = perf::start();
                self.dac_picture(0, false, out);
                timer.stop(perf::SRC_DAC);
            }
            (Mux::Dac(bank), _) => {
                let timer = perf::start();
                self.dac_picture(bank, true, out);
                timer.stop(perf::SRC_DAC);
            }
            (Mux::Encoder, _) => {
                if self.regs().matte() {
                    let (ma, mb, mp) = self.regs().matte_code();
                    if self.regs().snow() {
                        self.video.snow(out);
                    } else {
                        out.fill(video::matte_colour(ma, mb, mp));
                    }
                } else {
                    let timer = perf::start();
                    video::amiga_picture(field, Self::pixel0_column(field), out);
                    timer.stop(perf::SRC_AMIGA);
                }
            }
            // MONO: the luminance of the LK source.
            (Mux::Seven, video::Role::Fader | video::Role::Is) => {
                let lk = self.regs().lk();
                if lk == Mux::Seven {
                    out.fill(video::black());
                } else {
                    self.source_picture(lk, video::Role::Key, field, out);
                    video::to_mono(out);
                }
            }
            (Mux::Seven, _) => out.fill(video::black()),
        }
    }

    /// Framestore bank playback; bank 0 shows the live input while the
    /// card is digitising it.
    /// With `mapped`, a digital effect in progress readdresses the playback
    /// (see [`dve`]).
    fn dac_picture(&mut self, bank: usize, mapped: bool, out: &mut [u32]) {
        let live = bank == 0 && self.video.live();
        if live {
            if let regs::Mux::Vid(n) = self.regs().is() {
                let mut src = self.video.take_picture();
                if !self.input_into(n, &mut src) {
                    src.fill(video::black());
                }
                if mapped && self.dve_on {
                    self.dve.apply(&src, out);
                } else {
                    out.copy_from_slice(&src[..out.len()]);
                }
                self.video.give_back(src);
                return;
            }
        }
        self.settle(bank);
        if mapped && self.dve_on {
            let below = self.dve.reaches_below();
            let pic = self
                .video
                .framestore_picture(&self.frame_store, bank, below);
            self.dve.apply(pic, out);
        } else {
            let pic = self
                .video
                .framestore_picture(&self.frame_store, bank, false);
            out.copy_from_slice(&pic[..out.len()]);
        }
    }

    /// What a fader bus carries: its mux position, or with its WIPE bit
    /// and position 7, a source per pixel chosen by the Amiga picture.
    /// Returns how much of each video input the bus shows, for the sound.
    fn bus_picture(
        &mut self,
        mux: regs::Mux,
        latched: bool,
        field: &FieldPixels<'_>,
        out: &mut [u32],
    ) -> Inputs {
        if !(latched && mux == regs::Mux::Seven) {
            self.source_picture(mux, video::Role::Fader, field, out);
            return self.source_inputs(mux, video::Role::Fader);
        }
        let timer = perf::start();
        let mut codes = std::mem::take(&mut self.codes_buf);
        video::latch_codes_into(field, Self::pixel0_column(field), &mut codes);
        timer.stop(perf::LATCH);
        let mut count = [0usize; 8];
        for c in codes.iter() {
            count[usize::from(c.unwrap_or(7))] += 1;
        }
        let mut shares = Inputs::default();
        for code in 0..8u8 {
            let part = count[usize::from(code)] as f32 / codes.len().max(1) as f32;
            if part > 0.0 {
                shares.add(
                    &self.source_inputs(regs::Mux::from_code(code), video::Role::Fader),
                    part,
                );
            }
        }
        let mut src = self.video.take_picture();
        for code in (0..8u8).filter(|&c| count[usize::from(c)] > 0) {
            self.source_picture(
                regs::Mux::from_code(code),
                video::Role::Fader,
                field,
                &mut src,
            );
            for ((o, s), c) in out.iter_mut().zip(&src).zip(&codes) {
                if c.unwrap_or(7) == code {
                    *o = *s;
                }
            }
        }
        self.video.give_back(src);
        self.codes_buf = codes;
        shares
    }

    /// Which video inputs a mux position carries (the sound follows them):
    /// an input itself, the live framestore passing its input through, or
    /// for MONO the key source. Framestores, mattes and the Amiga are
    /// silent.
    fn source_inputs(&self, mux: regs::Mux, role: video::Role) -> Inputs {
        use regs::Mux;
        let mut shares = Inputs::default();
        match (mux, role) {
            (Mux::Vid(n), _) => shares.0[n] = 1.0,
            (Mux::Dac(0), video::Role::Fader | video::Role::Preview) if self.video.live() => {
                if let Mux::Vid(n) = self.regs().is() {
                    shares.0[n] = 1.0;
                }
            }
            (Mux::Seven, video::Role::Fader) => {
                let lk = self.regs().lk();
                if lk != Mux::Seven {
                    return self.source_inputs(lk, video::Role::Key);
                }
            }
            _ => {}
        }
        shares
    }

    /// The sound of `shares` for this frame: each input's frame of sound,
    /// scaled by how much of it is on the output. An input the Flyer has
    /// is silent here: the Flyer's sound is its own output.
    fn mix_sound(&self, shares: &Inputs, len: usize) -> Vec<(f32, f32)> {
        let mut out = vec![(0.0f32, 0.0f32); len];
        for (n, &w) in shares.0.iter().enumerate() {
            if w < 0.001 || self.flyer_channel(n).is_some() {
                continue;
            }
            let Some(sound) = self.video.input_sound(n) else {
                continue;
            };
            // A source frame's sound (1001/30 ms) fitted to the Amiga's.
            for (o, (l, r)) in out.iter_mut().zip(crate::videoslot::resample(sound, len)) {
                o.0 += l * w;
                o.1 += r * w;
            }
        }
        out
    }

    /// Compose and publish one frame of the Program and Preview outputs.
    fn produce_outputs(&mut self, field: &FieldPixels<'_>, result: &mut JobResult) {
        let (am, bm, pv) = (self.regs().am(), self.regs().bm(), self.regs().pv());
        let (amwipe, bmwipe) = (self.regs().amwipe(), self.regs().bmwipe());
        // A digital effect: the framestore playback is addressed line by
        // line and quad by quad from the Amiga's picture.
        let timer = perf::start();
        self.dve_on = !self.regs().linp() && !self.regs().lut();
        if self.dve_on {
            self.dve.read_field(field, Self::pixel0_column(field));
        }
        timer.stop(perf::DVE_MAP);
        let mut a = self.video.take_picture();
        let timer = perf::start();
        let a_inputs = self.bus_picture(am, amwipe, field, &mut a);
        timer.stop(perf::BUS_A);
        let mut program = self.video.take_picture();
        let program_inputs = if bm == am && bmwipe == amwipe {
            program.copy_from_slice(&a);
            a_inputs
        } else {
            let mut b = self.video.take_picture();
            let timer = perf::start();
            let b_inputs = self.bus_picture(bm, bmwipe, field, &mut b);
            timer.stop(perf::BUS_B);
            let timer = perf::start();
            let key = match self.fader_key(field) {
                FaderKey::Flat(k) => video::Key::Flat(k),
                FaderKey::Map => video::Key::Map(&self.key_buf),
            };
            timer.stop(perf::KEY);
            let timer = perf::start();
            video::mix(&a, &b, &key, &mut program);
            timer.stop(perf::MIX);
            let k = key.mean() / 255.0;
            let mut shares = Inputs::default();
            shares.add(&a_inputs, 1.0 - k);
            shares.add(&b_inputs, k);
            self.video.give_back(b);
            shares
        };
        let timer = perf::start();
        let mut preview = self.video.take_picture();
        let preview_inputs = if pv == regs::Mux::Seven {
            preview.copy_from_slice(&program);
            program_inputs
        } else {
            self.source_picture(pv, video::Role::Preview, field, &mut preview);
            self.source_inputs(pv, video::Role::Preview)
        };
        timer.stop(perf::PREVIEW);
        let samples = self.video.frame_samples();
        let timer = perf::start();
        let mut program_sound = self.mix_sound(&program_inputs, samples);
        let preview_sound = self.mix_sound(&preview_inputs, samples);
        timer.stop(perf::SOUND);
        let timer = perf::start();
        result.peak = self.add_flyer_sound(&mut program_sound);
        timer.stop(perf::FLYER_SOUND);
        if self.preview_overlay && self.regs().useron() {
            video::overlay_interface(field, Self::pixel0_column(field), &mut preview);
        }
        if self.diag {
            let r = self.regs();
            let state = format!(
                "AM={:?}{} BM={:?}{} PV={:?} IS={:?} LK={:?} CDS={:?} CD={} matte={} {:?} live={} OBR={}",
                r.am(),
                if r.amwipe() { "+wipe" } else { "" },
                r.bm(),
                if r.bmwipe() { "+wipe" } else { "" },
                r.pv(),
                r.is(),
                r.lk(),
                r.cds(),
                r.cd(),
                r.matte(),
                r.matte_code(),
                self.video.live(),
                r.obr()
            );
            if state != self.diag_outputs {
                log::info!("toaster: outputs {state}");
                self.diag_outputs = state;
            }
        }
        let timer = perf::start();
        self.video
            .publish(&program, &preview, &program_sound, &preview_sound);
        timer.stop(perf::PUBLISH);
        result.sound = program_sound;
        self.dve_on = false;
        self.video.give_back(a);
        self.video.give_back(program);
        self.video.give_back(preview);
    }

    /// Until the switcher first drives the card, its outputs are black and
    /// silent: the power-on mux settings would put inputs nobody chose on
    /// Program and Preview while the machine boots.
    fn produce_idle_outputs(&mut self, result: &mut JobResult) {
        let mut black = self.video.take_picture();
        black.fill(video::black());
        let silence = vec![(0.0f32, 0.0f32); self.video.frame_samples()];
        self.flyer_sound.clear();
        self.video.publish(&black, &black, &silence, &silence);
        self.video.give_back(black);
        result.sound = silence;
    }

    /// The Flyer's audio output is the system's: its fields of sound since
    /// the last frame, stretched to this frame, join Program's. The input
    /// files' sound (standing in for what reaches the Flyer's line inputs)
    /// sets its record meters: that peak is returned for the host to pass
    /// on.
    fn add_flyer_sound(&mut self, program: &mut [(f32, f32)]) -> Option<(f32, f32)> {
        self.flyer.as_ref()?;
        let peak = program.iter().fold((0.0f32, 0.0f32), |p, &(l, r)| {
            (p.0.max(l.abs()), p.1.max(r.abs()))
        });
        let sound = std::mem::take(&mut self.flyer_sound);
        if !sound.is_empty() {
            let len = program.len();
            for (o, (l, r)) in program
                .iter_mut()
                .zip(crate::videoslot::resample(&sound, len))
            {
                o.0 += l;
                o.1 += r;
            }
        }
        Some(peak)
    }

    /// The fader's mix, per picture pixel (0 = A, 255 = B): one value, or
    /// a map left in `key_buf`.
    fn fader_key(&mut self, field: &FieldPixels<'_>) -> FaderKey {
        let cd = self.regs().cd();
        match self.regs().cds() {
            // During a digital effect the quad codes say where the DVE
            // picture (B) shows over the background (A).
            mode @ (regs::Cds::Quaf | regs::Cds::TransQuad) if self.dve_on => {
                self.dve
                    .key_into(mode == regs::Cds::TransQuad, cd, &mut self.key_buf);
                FaderKey::Map
            }
            regs::Cds::Cd | regs::Cds::Quaf | regs::Cds::TransQuad | regs::Cds::RgQuad => {
                FaderKey::Flat(cd)
            }
            regs::Cds::LumKey => {
                let lk = self.regs().lk();
                let mut src = self.video.take_picture();
                self.source_picture(lk, video::Role::Key, field, &mut src);
                video::luma_key_into(
                    &src,
                    self.regs().lka(),
                    self.regs().no_key_invert(),
                    &mut self.key_buf,
                );
                self.video.give_back(src);
                FaderKey::Map
            }
            mode => {
                video::amiga_key_into(
                    field,
                    Self::pixel0_column(field),
                    mode,
                    cd,
                    &mut self.key_buf,
                );
                FaderKey::Map
            }
        }
    }

    /// Digitise the IS input into the recording bank (OBR): the picture
    /// is kept, and encoded when the bank is next read (`settle`).
    fn record_input(&mut self, field: &FieldPixels<'_>) {
        let is = self.regs().is();
        let bank = usize::from(self.regs().obr());
        let mut pic = match self.pending_grab[bank].take() {
            Some(old) => old,
            None => self.video.take_picture(),
        };
        self.source_picture(is, video::Role::Is, field, &mut pic);
        self.pending_grab[bank] = Some(pic);
        self.video.invalidate(bank);
    }

    /// The byte on the parallel port while the beam is at `beam`, from the
    /// line's rendered pixels: on a read-request line, the framestore byte
    /// the quad label under the beam (less the read pipeline) asks for.
    fn port_byte(&mut self, field: &FieldPixels<'_>, beam: BeamPos) -> Option<u8> {
        let header = Self::header(field, beam.vpos)?;
        let LineKind::Read {
            line,
            pixel,
            complemented,
        } = header.kind()
        else {
            return None;
        };
        if line >= LINES {
            return None;
        }
        let row = field.row(beam.vpos)?;
        let scale = field.canvas_scale as i64;
        let x0 = Self::pixel0_column(field);
        let beam_col = field.column_for_hpos(beam.hpos);
        let card_pixel = (beam_col - x0) / scale - READ_PIPELINE_PIXELS;
        if card_pixel < 0 {
            return None;
        }
        let quad = card_pixel / 4;
        let label = Self::quad_label(row, x0 + quad * 4 * scale, scale)?;
        let p = usize::from(label) * 4 + pixel;
        let byte = if p < LINE_PIXELS {
            self.frame_store_byte(self.read_bank(), complemented, line, p)
        } else {
            0
        };
        self.stats.port_bytes += 1;
        if self.diag && self.stats.port_bytes <= 600 {
            log::info!(
                "toaster: read vpos={} hpos={} quad {quad} label {label} line {line} px {pixel} comp={complemented} -> {byte:02X}",
                beam.vpos,
                beam.hpos
            );
        }
        Some(byte)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::videoslot::{SLOT_FIRST_CCK, SLOT_LINE_CCK};

    const WIDTH: usize = SLOT_LINE_CCK * 4;

    /// A one-line slot canvas for beam line `vpos`.
    struct Line {
        pixels: Vec<u32>,
        vpos: u32,
    }

    #[test]
    fn the_sound_follows_how_much_of_each_input_program_shows() {
        let mut shares = Inputs::default();
        let mut a = Inputs::default();
        a.0[0] = 1.0;
        let mut b = Inputs::default();
        b.0[1] = 1.0;
        let k = video::Key::Flat(64).mean() / 255.0;
        shares.add(&a, 1.0 - k);
        shares.add(&b, k);
        assert!((shares.0[0] - 0.749).abs() < 0.01 && (shares.0[1] - 0.251).abs() < 0.01);
        let map = video::Key::Map(&[0, 255, 255, 255]);
        assert!((map.mean() - 191.25).abs() < 0.01);
        // An input pinned to nothing is silent: no sound without frames.
        let mut card = Toaster::new();
        assert!(card
            .card()
            .mix_sound(&shares, 1471)
            .iter()
            .all(|&(l, r)| l == 0.0 && r == 0.0));
    }

    /// A capture at an emulated instant waits for the compose thread:
    /// after `settle_outputs` the outputs hold the fields handed over,
    /// with no call that would advance or read the card.
    #[test]
    fn settling_the_outputs_finishes_the_fields_handed_over() {
        let mut card = Toaster::new();
        let blank = Line::new(30);
        for _ in 0..2 {
            card.field_complete(&blank.field(), BeamPos::default());
        }
        VideoSlotDevice::settle_outputs(&mut card);
        let outputs = card.video_outputs();
        assert!(outputs.iter().all(|o| o.snapshot().is_some()));
    }

    #[test]
    fn a_routed_flyer_channel_is_input_3_and_its_sound_joins_program() {
        let link = FlyerLink::new();
        let mut card = Toaster::new();
        card.set_flyer_link(Arc::clone(&link));
        let white = u32::from_le_bytes([255, 255, 255, 255]);
        link.show(0, FieldSource::Matte(white));
        let line = Line::new(30);
        let mut out = vec![0u32; video::PICTURE_W * video::PICTURE_H];
        // What the Flyer did reaches the card with the next field.
        let picture = |card: &mut Toaster, n: usize, out: &mut [u32]| {
            card.field_complete(&line.field(), BeamPos::default());
            card.card()
                .source_picture(regs::Mux::Vid(n), video::Role::Fader, &line.field(), out);
        };
        // Not routed: input 3 is the card's own (here unplugged) input.
        picture(&mut card, 2, &mut out);
        assert!(out.iter().all(|&p| p == video::black()));
        link.set_routing(true, false);
        picture(&mut card, 2, &mut out);
        assert!(out.iter().all(|&p| p == white));
        picture(&mut card, 3, &mut out);
        assert!(out.iter().all(|&p| p == video::black()));
        // The Flyer's two fields of sound reach the card with the next
        // field (the fourth here completes a frame of idle outputs, which
        // drop what came before) and fill the frame's.
        card.field_complete(&line.field(), BeamPos::default());
        link.push_sound(&[(0.5, 0.25); 1470]);
        card.field_complete(&line.field(), BeamPos::default());
        assert!(link.take_sound().is_empty());
        assert_eq!(card.card().flyer_sound.len(), 1470);
        let mut program = vec![(0.0f32, 0.0f32); 1485];
        let peak = card.card().add_flyer_sound(&mut program);
        assert_eq!(peak, Some((0.0, 0.0)));
        assert!(program.iter().all(|&(l, r)| l == 0.5 && r == 0.25));
        assert!(card.card().flyer_sound.is_empty());
        assert_eq!(card.card().status_value()["flyer"]["input3"], true);
    }

    #[test]
    fn the_outputs_are_black_and_silent_until_the_switcher_drives_the_card() {
        let link = FlyerLink::new();
        let mut card = Toaster::new();
        card.set_flyer_link(Arc::clone(&link));
        let white = u32::from_le_bytes([255, 255, 255, 255]);
        link.set_routing(true, false);
        link.show(0, FieldSource::Matte(white));
        // Both buses on input 3, which the Flyer shows white.
        card.card().elh[ELH_EH0 + 2].r = 0xA200;
        card.card().elh[ELH_EH0 + 2].b = 0x0200;
        let program = |card: &Toaster| {
            let outputs = card.video_outputs();
            let (_, _, pixels) = outputs[1].snapshot().expect("a frame");
            pixels
        };
        let frame = |card: &mut Toaster| {
            let blank = Line::new(30);
            card.field_complete(&blank.field(), BeamPos::default());
            card.field_complete(&blank.field(), BeamPos::default());
            let mut sound = Vec::new();
            card.take_audio(&mut sound);
            sound
        };
        link.push_sound(&[(0.5, 0.25); 1470]);
        let sound = frame(&mut card);
        assert!(!card.card().active);
        assert!(program(&card).iter().all(|&p| p == video::black()));
        assert!(!sound.is_empty() && sound.iter().all(|&(l, r)| l == 0.0 && r == 0.0));
        // The Flyer's sound from before is dropped, not played late.
        assert!(link.take_sound().is_empty());
        // The switcher's first header: from then on the outputs follow it.
        let mut eh0 = Line::new(23);
        eh0.header(Header {
            r: 0x8000,
            g: 0x0100,
            b: 0xF000,
            i: 0x0F00,
        });
        card.field_complete(&eh0.field(), BeamPos::default());
        assert!(card.card().active);
        card.card().elh[ELH_EH0 + 2].r = 0xA200;
        card.card().elh[ELH_EH0 + 2].b = 0x0200;
        link.push_sound(&[(0.5, 0.25); 1470]);
        let blank = Line::new(30);
        card.field_complete(&blank.field(), BeamPos::default());
        let mut sound = Vec::new();
        card.take_audio(&mut sound);
        assert!(program(&card).iter().all(|&p| p == white));
        assert!(sound.iter().any(|&(l, _)| l != 0.0));
    }

    #[test]
    fn a_latch_effect_picks_each_pixel_source_from_the_amiga_colour() {
        // Picture row 1 is Amiga line AMIGA_LINE0 + 1.
        let mut line = Line::new(video::AMIGA_LINE0 + 1);
        let x0 = Card::pixel0_column(&line.field());
        // Picture column c is card pixel PICTURE_X0 + c.
        let at = |c: usize| x0 as usize + video::PICTURE_X0 + c;
        // Transparent (T) pixels latch: DV1 (code 4, r1) from picture
        // column 10, input 3 (code 2, r2) from 100; opaque pixels keep it.
        let latch = |r: u8, b: u8| u32::from_le_bytes([r, 0, b, 0]);
        line.pixels[at(10)] = latch(0x02, 0);
        line.pixels[at(100)] = latch(0x04, 0);
        line.pixels[at(200)] = latch(0x00, 0x04);
        let codes = video::latch_codes(&line.field(), x0);
        let row = &codes[video::PICTURE_W..2 * video::PICTURE_W];
        assert_eq!(row[5], None);
        assert_eq!(row[10], Some(4));
        assert_eq!(row[99], Some(4));
        assert_eq!(row[100], Some(2));
        assert_eq!(row[200], Some(1));
        assert_eq!(row[700], Some(1));
    }

    /// The fields of a short switcher session: a header line, framestore
    /// writes, a bank flip and blank fields, each as a one-line canvas.
    fn session_fields() -> Vec<Line> {
        let mut fields = Vec::new();
        let mut eh0 = Line::new(23);
        eh0.header(Header {
            r: 0x8000,
            g: 0x0100,
            b: 0xF000,
            i: 0x0F00,
        });
        fields.push(eh0);
        let bytes: Vec<u8> = (0..300).map(|k| 71 + (k % 129) as u8).collect();
        for (vpos, g) in [(24, 0xC000u16), (25, 0x8000), (26, 0xE000), (27, 0xA000)] {
            let mut data = Line::new(vpos);
            data.header(Header {
                r: 0xF000,
                g,
                b: (3u16 & 0xF) << 12,
                i: 0,
            });
            for (k, &byte) in bytes.iter().enumerate() {
                let nibble = if g & 0x4000 != 0 {
                    byte >> 4
                } else {
                    byte & 0xF
                };
                data.card_pixel(12 + k, nibble);
            }
            fields.push(data);
        }
        for vpos in [30, 31, 40, 41] {
            fields.push(Line::new(vpos));
        }
        fields
    }

    /// A host looking at the card (a capture, a status query) waits for
    /// the compose thread, but it is no part of the emulated timeline: the
    /// frames' sound still reaches Paula at the fields it does on a run
    /// nobody looked at.
    #[test]
    fn a_host_query_does_not_move_when_the_sound_is_taken() {
        let fields = session_fields();
        let mut quiet = Toaster::new();
        let mut watched = Toaster::new();
        for (k, line) in fields.iter().enumerate() {
            let at = BeamPos {
                frame: k as u64,
                ..BeamPos::default()
            };
            let (mut unseen, mut seen) = (Vec::new(), Vec::new());
            quiet.submit(FieldInput::Owned(OwnedField::copy_of(&line.field())), at);
            quiet.take_audio(&mut unseen);
            watched.submit(FieldInput::Owned(OwnedField::copy_of(&line.field())), at);
            watched.take_audio(&mut seen);
            assert_eq!(seen.len(), unseen.len(), "field {k}");
            watched.settle_outputs();
            let _ = watched.control_status();
        }
    }

    #[test]
    fn fields_handed_to_the_compose_thread_give_the_outputs_of_fields_taken_at_once() {
        let fields = session_fields();
        // One card takes each field in before the next; the other is
        // handed them all with the thread free to run behind.
        let mut direct = Toaster::new();
        let mut piped = Toaster::new();
        let mut direct_sound = Vec::new();
        let mut piped_sound = Vec::new();
        for (k, line) in fields.iter().enumerate() {
            let at = BeamPos {
                frame: k as u64,
                ..BeamPos::default()
            };
            direct.field_complete(&line.field(), at);
            direct.take_audio(&mut direct_sound);
            piped.submit(FieldInput::Owned(OwnedField::copy_of(&line.field())), at);
            piped.take_audio(&mut piped_sound);
        }
        // Only frames whose fields are known finished have been taken.
        assert!(piped_sound.len() < direct_sound.len());
        piped.drain();
        piped.take_audio(&mut piped_sound);
        assert_eq!(direct_sound, piped_sound);
        assert_eq!(direct.stats(), piped.stats());
        assert_eq!(direct.card().elh, piped.card().elh);
        assert_eq!(
            direct.card().frame_store.data,
            piped.card().frame_store.data
        );
        let program = |card: &Toaster| card.video_outputs()[1].snapshot().expect("a frame");
        assert_eq!(program(&direct), program(&piped));
        // The written line is in both.
        assert_eq!(direct.frame_store_byte(0, false, 7, 12), 71);
        assert_eq!(piped.frame_store_byte(0, false, 7, 12), 71);
    }

    #[test]
    fn a_digitised_picture_is_encoded_when_the_bank_is_read() {
        let mut toaster = Toaster::new();
        let line = Line::new(30);
        let mut card = toaster.card();
        // IS = input 1 (unplugged: black), OBR clear: bank 0 records.
        card.elh[ELH_EH0 + 1].r = 0x4000;
        card.elh[ELH_EH0].r = 0x8000;
        card.record_input(&line.field());
        card.record_input(&line.field());
        assert_eq!(card.stats.grabs_encoded, 0);
        // Reading the bank encodes the latest picture, once.
        let byte = card.frame_store_byte(0, false, 1, 12);
        assert_eq!(card.stats.grabs_encoded, 1);
        let mut n = [0u8; 752];
        let mut c = [0u8; 752];
        composite::encode_line(&[video::black(); 752], 0, &mut n, &mut c);
        assert_eq!(byte, n[0]);
        assert_eq!(card.frame_store_byte(0, true, 480, 700), c[688]);
        assert_eq!(card.stats.grabs_encoded, 1);
        // The other bank was not touched.
        assert_eq!(card.frame_store_byte(1, false, 1, 12), 0);
        // A write to the bank lands over the digitised picture, as it would
        // have had the picture been encoded at once: the written line is
        // the Amiga's, the others the picture's.
        card.record_input(&line.field());
        drop(card);
        write_line(&mut toaster, false, 1, &[9, 8, 7]);
        assert_eq!(toaster.frame_store_byte(0, false, 1, 12), 9);
        assert_eq!(toaster.frame_store_byte(0, false, 3, 15), n[3]);
        assert_eq!(toaster.stats().grabs_encoded, 2);
    }

    impl Line {
        fn new(vpos: u32) -> Self {
            Self {
                pixels: vec![u32::from_le_bytes([0, 0, 0, 0xFF]); WIDTH],
                vpos,
            }
        }

        fn field(&self) -> FieldPixels<'_> {
            FieldPixels {
                pixels: &self.pixels,
                width: WIDTH,
                rows: 1,
                visible_start_vpos: self.vpos,
                first_cck: SLOT_FIRST_CCK,
                canvas_scale: 1,
                long_frame: true,
                interlaced: false,
                valid_rows: 1,
            }
        }

        /// A pixel the card's four lines read as `v` (R, G<<1, B<<2, I<<3).
        fn wire(v: u8) -> u32 {
            let r = if v & 1 != 0 { 0x02 } else { 0 } | if v & 4 != 0 { 0x01 } else { 0 };
            let g = if v & 2 != 0 { 0x02 } else { 0 } | if v & 8 != 0 { 0x01 } else { 0 };
            u32::from_le_bytes([r, g, 0, 0xFF])
        }

        /// Draw the header sprite: sixteen lo-res pixels from `$5C`.
        fn header(&mut self, h: Header) {
            let x0 = self
                .field()
                .column_for_lores(HEADER_LORES + crate::bus::SPRITE_OUTPUT_DELAY_LORES);
            for k in 0..16 {
                let bit = 15 - k;
                let v = ((h.r >> bit) & 1)
                    | ((h.g >> bit) & 1) << 1
                    | ((h.b >> bit) & 1) << 2
                    | ((h.i >> bit) & 1) << 3;
                let col = (x0 + 2 * k) as usize;
                self.pixels[col] = Self::wire(v as u8);
                self.pixels[col + 1] = Self::wire(v as u8);
            }
        }

        /// Put nibble `v` on card pixel `p`.
        fn card_pixel(&mut self, p: usize, v: u8) {
            let col = (Card::pixel0_column(&self.field()) + p as i64) as usize;
            self.pixels[col] = Self::wire(v);
        }

        /// The read bitmap: quads from 3 labelled `label`, 18 quads each.
        fn labels(&mut self, first_label: u8) {
            for quad in 3..192usize {
                let label = first_label + ((quad - 3) / 18) as u8;
                for j in 0..4 {
                    let bit = 3 - j;
                    let v = ((label >> 4 >> bit) & 1) << 3 | ((label >> bit) & 1) << 2;
                    self.card_pixel(quad * 4 + j, v);
                }
            }
        }
    }

    /// Framestore line `line` written with `bytes` from card pixel 12 by
    /// the four passes the switcher makes, with `OBR` as given.
    fn write_line(card: &mut Toaster, obr: bool, line: usize, bytes: &[u8]) {
        write_line_pass(card, obr, line, bytes, true, 0);
    }

    /// One write of `line`, with EH0's `LUST` as given and the bitmap
    /// drawn `left` pixels left of card pixel 12 (a zipper's second pass).
    fn write_line_pass(
        card: &mut Toaster,
        obr: bool,
        line: usize,
        bytes: &[u8],
        lust: bool,
        left: usize,
    ) {
        let mut eh0 = Line::new(23);
        eh0.header(Header {
            r: 0x8000 | if obr { EH0R_OBR } else { 0 },
            g: 0x0100,
            b: 0xF000,
            i: if lust { 0x0F00 } else { 0x0E00 },
        });
        card.field_complete(&eh0.field(), BeamPos::default());
        let l = (line / 2) as u16;
        let field_ii = line.is_multiple_of(2);
        for (vpos, g) in [(24, 0xC000u16), (25, 0x8000), (26, 0xE000), (27, 0xA000)] {
            let mut data = Line::new(vpos);
            data.header(Header {
                r: 0xF000,
                g: g | if field_ii { 0x1000 } else { 0 },
                b: (l & 0xF) << 12,
                i: (l >> 4) << 12,
            });
            for (k, &byte) in bytes.iter().enumerate().skip(left) {
                let nibble = if g & 0x4000 != 0 {
                    byte >> 4
                } else {
                    byte & 0xF
                };
                data.card_pixel(12 + k - left, nibble);
            }
            card.field_complete(&data.field(), BeamPos::default());
        }
    }

    fn read_request(line: usize, pixel: u8, complemented: bool) -> Header {
        let l = (line / 2) as u16;
        let field_ii = line.is_multiple_of(2);
        Header {
            r: [0x0000, 0x2000, 0xC000, 0xE000][usize::from(pixel)],
            g: if complemented { 0x2000 } else { 0 } | if field_ii { 0x1000 } else { 0 },
            b: (l & 0xF) << 12,
            i: (l >> 4) << 12,
        }
    }

    #[test]
    fn a_zipper_writes_second_pass_lands_where_the_first_did() {
        // The switcher's AA framestore load: the whole line with LUST set,
        // then again with LUST clear and the bitmap scrolled 16 pixels
        // left. Both passes must agree on every sample.
        let mut card = Toaster::new();
        let bytes: Vec<u8> = (0..756).map(|k| 71 + (k % 129) as u8).collect();
        write_line_pass(&mut card, false, 100, &bytes, true, 0);
        write_line_pass(&mut card, false, 100, &bytes, false, 16);
        let card = card.card();
        let stored = card.frame_store.line(0, false, 100);
        for k in [0usize, 4, 15, 16, 300, 731, 739] {
            assert_eq!(stored[12 + k], bytes[k], "sample {}", 12 + k);
        }
    }

    #[test]
    fn a_written_line_reads_back_by_quad_label() {
        let mut card = Toaster::new();
        let ramp: Vec<u8> = (71..=200).collect();
        write_line(&mut card, false, 504, &ramp);
        assert!(card.card().active);
        assert_eq!(card.frame_store_byte(0, false, 504, 12), 71);
        assert_eq!(card.frame_store_byte(0, true, 504, 13), 72);

        // The switcher reads bank 0 with OBR set.
        let mut eh0 = Line::new(23);
        eh0.header(Header {
            r: 0x8000 | EH0R_OBR,
            g: 0x0100,
            b: 0xF000,
            i: 0x0F00,
        });
        card.field_complete(&eh0.field(), BeamPos::default());
        let mut read = Line::new(24);
        read.header(read_request(504, 0, false));
        read.labels(3);
        let at = |hpos| BeamPos {
            vpos: 24,
            hpos,
            ..BeamPos::default()
        };
        // HS.default's ten read positions land in labels 3..=12.
        let expected: Vec<u8> = (0..10).map(|i| 71 + 4 * i).collect();
        let got: Vec<u8> = [58, 75, 93, 111, 129, 147, 166, 183, 201, 216]
            .into_iter()
            .map(|h| card.card().port_byte(&read.field(), at(h)).unwrap())
            .collect();
        assert_eq!(got, expected);
        // Five clocks later (the CIA access's E-clock slip) still reads
        // the same labels.
        let late: Vec<u8> = [63, 80, 98, 116, 134, 152, 171, 188, 206, 221]
            .into_iter()
            .map(|h| card.card().port_byte(&read.field(), at(h)).unwrap())
            .collect();
        assert_eq!(late, expected);
        // Pixel 1 of each quad, from the complemented copy.
        let mut read = Line::new(25);
        read.header(read_request(504, 1, true));
        read.labels(3);
        assert_eq!(
            card.card().port_byte(
                &read.field(),
                BeamPos {
                    vpos: 25,
                    hpos: 75,
                    ..BeamPos::default()
                }
            ),
            Some(72 + 4)
        );
    }

    #[test]
    fn banks_follow_the_obr_bit() {
        let mut card = Toaster::new();
        write_line(&mut card, true, 100, &[1, 2, 3]);
        assert_eq!(card.frame_store_byte(1, false, 100, 12), 1);
        assert_eq!(card.frame_store_byte(0, false, 100, 12), 0);
        write_line(&mut card, false, 100, &[9]);
        assert_eq!(card.frame_store_byte(0, false, 100, 12), 9);
        // Reads come from the other bank than OBR names: the last EH0 above
        // cleared OBR, so bank 1 is read.
        let mut read = Line::new(24);
        read.header(read_request(100, 0, false));
        read.labels(3);
        assert_eq!(
            card.card().port_byte(
                &read.field(),
                BeamPos {
                    vpos: 24,
                    hpos: 58,
                    ..BeamPos::default()
                }
            ),
            Some(1)
        );
    }

    #[test]
    fn input_lines_follow_the_wired_rgb_bits() {
        let px = |r: u8, g: u8, b: u8| u32::from_le_bytes([r, g, b, 0xFF]);
        // The switcher's write palette: one line per bitplane.
        assert_eq!(lines_of(px(0x04, 0, 0)), 1);
        assert_eq!(lines_of(px(0, 0, 0x04)), 2);
        assert_eq!(lines_of(px(0x01, 0, 0)), 4);
        assert_eq!(lines_of(px(0, 0x01, 0)), 8);
        // The alternate bits of each pair, and the sprite palette's pairs.
        assert_eq!(lines_of(px(0x02, 0, 0)), 1);
        assert_eq!(lines_of(px(0, 0x02, 0)), 2);
        assert_eq!(lines_of(px(0, 0, 0x01)), 4);
        assert_eq!(lines_of(px(0, 0, 0x02)), 8);
        assert_eq!(lines_of(px(0x66, 0, 0)), 1);
        assert_eq!(lines_of(px(0, 0x22, 0x44)), 2);
        // High bits of the guns carry nothing.
        assert_eq!(lines_of(px(0xF8, 0xFC, 0xF8)), 0);
    }

    #[test]
    fn header_kinds_decode() {
        let h = |r, g, b, i| Header { r, g, b, i };
        // DoSafeWrite's filler: no ELH, no write bit, line number 255.
        assert_eq!(h(0xF000, 0x0000, 0xF000, 0xF000).kind(), LineKind::Dummy);
        // The read self-test's line 504 (L = 252) decodes like any other.
        assert_eq!(
            h(0xF000, 0xD000, 0xC000, 0xF000).kind(),
            LineKind::Write {
                line: 504,
                high_nibble: true,
                complemented: false
            }
        );
        // tblI/tblII write codes for toaster line L = $21: field I line 67.
        assert_eq!(
            h(0xF000, 0xC000, 0x1000, 0x2000).kind(),
            LineKind::Write {
                line: 67,
                high_nibble: true,
                complemented: false
            }
        );
        assert_eq!(
            h(0xF000, 0xB000, 0x1000, 0x2000).kind(),
            LineKind::Write {
                line: 66,
                high_nibble: false,
                complemented: true
            }
        );
        // Read request, pixel 2 of each quad, complemented, field II.
        assert_eq!(
            h(0xC000, 0x3000, 0x1000, 0x2000).kind(),
            LineKind::Read {
                line: 66,
                pixel: 2,
                complemented: true
            }
        );
        assert_eq!(
            h(0x8100, 0x0500, 0xFF00, 0xFF00).kind(),
            LineKind::Elh { index: 0 }
        );
        // EH6's page is in the B word: EH63.
        assert_eq!(h(0xB000, 0, 0x0300, 0).kind(), LineKind::Elh { index: 9 });
        assert_eq!(h(0x7000, 0, 0, 0).kind(), LineKind::Elh { index: ELH_EH7 });
    }
}
