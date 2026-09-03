//! WinUAE-compatible "uaelib" trap: a guest-callable emulator service at
//! `$F0FF60`.
//!
//! WinUAE's boot ROM ("rtarea", `autoconf.cpp`) carries a trap word at
//! `rtarea_base + 0xFF60` that `uaelib_demux` (`uaelib.cpp`) services: guest
//! code calls it like a C function with the function number as the first
//! stack argument (`uae-configuration`, `uaequit`, and the vscode-amiga-debug
//! template's `warpmode()`, `KPrintF()` and `debug_*()` helpers all go through
//! it). Copperline answers the same ABI at the same address so that code
//! works unchanged:
//!
//! - **82** `cfgfile_uaelib_modify(index, parms, size, out, outsize)`: a
//!   `"key value"` configuration line. The only key that does anything here
//!   is `warp` (`true`/`yes`/`false`/`no`), which latches a warp request for
//!   the frontend; the template's `cpu_speed` and `*_cycle_exact` keys are
//!   accepted and ignored because the core is always cycle-exact.
//! - **86** `write_log("DBG: %s")`: a debug line, echoed to the host
//!   console like serial output and queued for control-protocol
//!   subscribers.
//! - **88** the template's `debug_cmd` multiplexer: resource registration
//!   (bitmaps, palettes, copper lists) and idle markers are recorded for the
//!   control protocol; overlay drawing is presented by the window. File
//!   load/save is disabled unless `[emulation] uaelib_files = true`, and is
//!   then confined to the `--run` program directory.
//! - everything else returns 0 with no side effects (Copperline does not
//!   impersonate WinUAE's version through function 0).
//!
//! Nothing here models Amiga hardware. The trap is a 32-byte ROM-like
//! region decoded by the CPU bus adapter after every real device; the only
//! writable word is a doorbell the stub rings with the caller's stack
//! pointer, and the call is serviced synchronously inside that write, the
//! same way the services board handles a DosPacket (`src/filesys.rs`).
//! Unlike WinUAE, which intercepts an A-line opcode in the CPU core, no
//! CPU-side hook exists: the stub is ordinary 68k code.
//!
//! ```text
//! +00  4EB9 00F0FF68   JSR ($00F0FF68).L        first word 0x4EB9: the template's detection
//! +06  4E75            RTS                      back to the C caller
//! +08  23CF 00F0FF7C   MOVE.L A7,($00F0FF7C).L  doorbell = A7 (ARGn at A7+8+4n: two returns deep)
//! +0E  2039 00F0FF78   MOVE.L ($00F0FF78).L,D0  result the host latched during the doorbell write
//! +14  4E75            RTS
//! +16  0000            pad (the 68000 prefetches two words past an RTS)
//! +18  result latch    (host-written, guest-readable)
//! +1C  doorbell latch  (guest-written; a 68000 lands the high word first)
//! ```
//!
//! The guest sees D0 (the result) and the CCR clobbered, which is what a C
//! caller expects of a function; WinUAE preserves more. The result latch is
//! global, so a uaelib call made from an interrupt handler between the
//! doorbell write and the result read would clobber the interrupted
//! call's D0. A CDTV extended ROM at `$F00000` decodes ahead of this region
//! and hides it, as WinUAE relocates its rtarea on that machine.

use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};

#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use cap_std::ambient_authority;
#[cfg(not(target_arch = "wasm32"))]
use cap_std::fs::Dir;

use crate::memory::Memory;
use crate::zorro_device::{dma_read_byte, dma_write_byte};

/// Where the trap lives: WinUAE's default rtarea (`$F00000`) + `0xFF60`.
pub const UAELIB_BASE: u32 = 0x00F0_FF60;
/// The decoded region: the stub, its pad, and the two latches.
pub const UAELIB_SIZE: u32 = 0x20;
/// The first word of the stub, `JSR (xxx).L`; guest code tests for it
/// (or WinUAE's A-line form `0xA00E`) before calling.
pub const DETECTION_WORD: u16 = 0x4EB9;

const OFF_INNER: u32 = 0x08;
const OFF_RESULT: u32 = 0x18;
const OFF_DOORBELL: u32 = 0x1C;
/// Stack offset of ARG0 when the doorbell captures A7: the inner routine's
/// return address (to `+06`) sits at A7+0 and the C caller's at A7+4.
const ARG_BASE: u32 = 8;

/// Bound of the debug-event queue awaiting a control-protocol subscriber;
/// the oldest event is dropped and counted beyond it.
pub const DEBUG_EVENT_CAPACITY: usize = 256;
/// Bound of the resource registry.
pub const RESOURCE_MAX: usize = 256;
/// Bound of the console mirror of the debug log (matches the debugger
/// console pane's scrollback; uaelib must not depend on the video layer).
pub const CONSOLE_LINE_CAPACITY: usize = 500;
/// WinUAE's `cfgfile_uaelib_modify` refuses an `outsize` this large and
/// measures an unsized parameter string no further than this.
const CFG_STRING_MAX: usize = 32_768;
/// Sanity cap on a function-86 string.
const DEBUG_TEXT_MAX: usize = 4_096;
/// Bound both guest-provided file names and the allocation used to copy one.
const DEBUG_FILE_NAME_MAX: usize = 4_096;
/// Hard cap for either direction of the opt-in guest/host file bridge.
pub const DEBUG_FILE_MAX: usize = 16 * 1024 * 1024;
/// `sizeof(struct debug_resource)` in the template.
const RESOURCE_STRUCT_SIZE: usize = 50;

/// Host-only authority for the opt-in file bridge. Keeping the directory
/// handle open is the security boundary: every guest path is resolved by
/// `cap_std` relative to this handle with beneath/no-follow protections, so a
/// concurrent symlink swap cannot redirect an already-authorized operation.
#[derive(Clone, Debug)]
pub(crate) struct FileAuthority {
    root: PathBuf,
    #[cfg(not(target_arch = "wasm32"))]
    dir: Arc<Dir>,
}

impl FileAuthority {
    #[cfg(not(target_arch = "wasm32"))]
    fn open(&self, relative: &Path) -> std::io::Result<cap_std::fs::File> {
        self.dir.open(relative)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn create(&self, relative: &Path) -> std::io::Result<cap_std::fs::File> {
        self.dir.create(relative)
    }
}

pub const FN_GET_VERSION: u32 = 0;
pub const FN_CFG_READ: u32 = 81;
pub const FN_CFG_MODIFY: u32 = 82;
pub const FN_DEBUG_LOG: u32 = 86;
pub const FN_DEBUG_CMD: u32 = 88;

pub const CMD_CLEAR: u32 = 0;
pub const CMD_RECT: u32 = 1;
pub const CMD_FILLED_RECT: u32 = 2;
pub const CMD_TEXT: u32 = 3;
pub const CMD_REGISTER_RESOURCE: u32 = 4;
pub const CMD_SET_IDLE: u32 = 5;
pub const CMD_UNREGISTER_RESOURCE: u32 = 6;
pub const CMD_LOAD: u32 = 7;
pub const CMD_SAVE: u32 = 8;

const fn put_long(img: &mut [u8; 32], at: usize, value: u32) {
    img[at] = (value >> 24) as u8;
    img[at + 1] = (value >> 16) as u8;
    img[at + 2] = (value >> 8) as u8;
    img[at + 3] = value as u8;
}

/// The stub image, assembled from `UAELIB_BASE` so the absolute operands
/// cannot drift from the address the region is decoded at.
const fn build_image() -> [u8; 32] {
    let mut img = [0u8; 32];
    // +00 JSR (inner).L
    img[0] = 0x4E;
    img[1] = 0xB9;
    put_long(&mut img, 2, UAELIB_BASE + OFF_INNER);
    // +06 RTS
    img[6] = 0x4E;
    img[7] = 0x75;
    // +08 MOVE.L A7,(doorbell).L
    img[8] = 0x23;
    img[9] = 0xCF;
    put_long(&mut img, 10, UAELIB_BASE + OFF_DOORBELL);
    // +0E MOVE.L (result).L,D0
    img[14] = 0x20;
    img[15] = 0x39;
    put_long(&mut img, 16, UAELIB_BASE + OFF_RESULT);
    // +14 RTS
    img[20] = 0x4E;
    img[21] = 0x75;
    img
}

const IMAGE: [u8; 32] = build_image();

/// What a registered `struct debug_resource` describes.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ResourceKind {
    Bitmap {
        width: u16,
        height: u16,
        planes: u16,
    },
    Palette {
        entries: u16,
    },
    Copperlist,
    Unknown(u16),
}

/// The overlay display list: the guest's coordinate space (PAL hires
/// interlace, as Bartman's WinUAE fork composites it), the list bound,
/// and the cap on a text command's string.
pub const OVERLAY_WIDTH: u32 = 768;
pub const OVERLAY_HEIGHT: u32 = 576;
pub const OVERLAY_CAP: usize = 2048;
pub const OVERLAY_TEXT_MAX: usize = 256;

/// One fn-88 overlay command, clamped into the 768x576 space. Colours are
/// the guest's 0x00RRGGBB. Hash feeds the window's rasterization cache
/// key.
#[derive(Clone, Debug, Hash, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OverlayCmd {
    Rect {
        l: u16,
        t: u16,
        r: u16,
        b: u16,
        colour: u32,
    },
    FilledRect {
        l: u16,
        t: u16,
        r: u16,
        b: u16,
        colour: u32,
    },
    Text {
        l: u16,
        t: u16,
        text: String,
        colour: u32,
    },
}

/// `debug_resource_flags` in the template.
pub const RESOURCE_FLAG_INTERLEAVED: u16 = 1 << 0;
pub const RESOURCE_FLAG_MASKED: u16 = 1 << 1;
pub const RESOURCE_FLAG_HAM: u16 = 1 << 2;

/// A memory range the guest described through `debug_register_*`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DebugResource {
    pub address: u32,
    pub size: u32,
    pub name: String,
    pub kind: ResourceKind,
    pub flags: u16,
    /// Emulated frame counter when the guest registered it.
    pub registered_frame: u64,
}

impl DebugResource {
    pub fn kind_name(&self) -> &'static str {
        match self.kind {
            ResourceKind::Bitmap { .. } => "bitmap",
            ResourceKind::Palette { .. } => "palette",
            ResourceKind::Copperlist => "copperlist",
            ResourceKind::Unknown(_) => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ResourceAction {
    Registered,
    Replaced,
    Unregistered,
    Cleared,
}

impl ResourceAction {
    pub fn name(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Replaced => "replaced",
            Self::Unregistered => "unregistered",
            Self::Cleared => "cleared",
        }
    }
}

/// One item of the control-protocol `debug` stream.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DebugEvent {
    /// A function-86 line.
    Log(String),
    /// A function-88 registry change.
    Resource {
        action: ResourceAction,
        resource: DebugResource,
    },
}

/// The guest's `debug_start_idle()` / `debug_stop_idle()` markers turned
/// into per-frame colour-clock accounting, so a profile can tell how much
/// of each frame the program spent waiting.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IdleAccounting {
    idle: bool,
    since_cck: u64,
    frame_start_cck: u64,
    accum_cck: u64,
    used: bool,
    /// `(idle cck, frame length in cck)` of the last completed frame.
    last_frame: Option<(u64, u64)>,
}

impl IdleAccounting {
    fn set(&mut self, on: bool, cck: u64) {
        self.used = true;
        if self.idle == on {
            return;
        }
        if self.idle {
            self.accum_cck += cck.saturating_sub(self.since_cck);
        }
        self.idle = on;
        self.since_cck = cck;
    }

    fn note_frame_start(&mut self, cck: u64) {
        if self.idle {
            self.accum_cck += cck.saturating_sub(self.since_cck);
            self.since_cck = cck;
        }
        if self.used {
            self.last_frame = Some((self.accum_cck, cck.saturating_sub(self.frame_start_cck)));
        }
        self.accum_cck = 0;
        self.frame_start_cck = cck;
    }

    /// Whether the guest currently declares itself idle.
    pub fn is_idle(&self) -> bool {
        self.idle
    }

    /// Whether the guest has ever marked idle time.
    pub fn used(&self) -> bool {
        self.used
    }

    /// `(idle cck, frame cck)` of the last completed frame, once used.
    pub fn last_frame(&self) -> Option<(u64, u64)> {
        self.last_frame
    }

    pub fn last_frame_idle_cck(&self) -> Option<u64> {
        self.last_frame.map(|(idle, _)| idle)
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct UaeLib {
    /// Live copy of `IMAGE`; only the two latches ever change.
    image: [u8; 32],
    /// The latest guest warp request not yet taken by the frontend (the
    /// last one wins within a frame).
    pending_warp: Option<bool>,
    /// Function-86 lines and registry changes awaiting a control-protocol
    /// subscriber; bounded, oldest dropped.
    debug_events: VecDeque<DebugEvent>,
    debug_dropped: u64,
    /// Function-88 registry, one entry per address.
    resources: Vec<DebugResource>,
    idle: IdleAccounting,
    /// Function-88 overlay display list: what the guest asked to be drawn
    /// over the picture, until it sends `clear`. Guest state (serialized):
    /// run-ahead and rewind restore the picture's annotations with the
    /// picture.
    overlay: Vec<OverlayCmd>,
    overlay_dropped: u64,
    /// Host-only: while a run-ahead frame is speculative the console echo
    /// is withheld (Paula's `speculative_host_quiet`); the queued event is
    /// guest state and is rewound with the frame.
    #[serde(skip)]
    speculative_host_quiet: bool,
    #[serde(skip)]
    stdout_muted: bool,
    /// Held `--run` directory capability for the opt-in file commands. Host
    /// authority never travels in a save state; the running session reapplies
    /// this exact handle after timeline restores.
    #[serde(skip)]
    file_authority: Option<FileAuthority>,
    /// Host-only mirror of the echo for the debugger console pane:
    /// bounded, oldest dropped, drained once per committed frame by
    /// `App::service_uaelib`. Fed inside the run-ahead gate but NOT the
    /// stdout mute (a muted terminal must not blind the pane).
    #[serde(skip)]
    console_lines: VecDeque<String>,
    #[cfg(test)]
    #[serde(skip)]
    echoed: Vec<String>,
}

impl Default for UaeLib {
    fn default() -> Self {
        Self::new()
    }
}

impl UaeLib {
    pub fn new() -> Self {
        Self {
            image: IMAGE,
            pending_warp: None,
            debug_events: VecDeque::new(),
            debug_dropped: 0,
            resources: Vec::new(),
            idle: IdleAccounting::default(),
            overlay: Vec::new(),
            overlay_dropped: 0,
            speculative_host_quiet: false,
            stdout_muted: false,
            file_authority: None,
            console_lines: VecDeque::new(),
            #[cfg(test)]
            echoed: Vec::new(),
        }
    }

    /// Whether `addr` falls inside the decoded region.
    pub fn decodes(addr: u32) -> bool {
        addr.wrapping_sub(UAELIB_BASE) < UAELIB_SIZE
    }

    /// A machine reset: the latches return to the image and a pending warp
    /// request, the registry and the idle accounting go with the guest
    /// that made them. Queued debug events belong to the host observer and
    /// survive, and the registry teardown is queued as `Cleared` events so
    /// a subscriber that saw the registrations can reconcile.
    pub fn reset(&mut self) {
        self.image = IMAGE;
        self.pending_warp = None;
        self.clear_registry();
        self.idle = IdleAccounting::default();
        self.overlay.clear();
        self.overlay_dropped = 0;
    }

    /// Drop every registered resource, queueing a `Cleared` event per
    /// entry (the guest's `debug_unregister(0)`, and a machine reset).
    fn clear_registry(&mut self) {
        for resource in std::mem::take(&mut self.resources) {
            self.push_event(DebugEvent::Resource {
                action: ResourceAction::Cleared,
                resource,
            });
        }
    }

    /// Big-endian read of `size` bytes at `off`, any alignment; 0 when the
    /// access runs past the region.
    pub fn read(&self, off: u32, size: usize) -> u32 {
        let at = off as usize;
        if at + size > self.image.len() {
            return 0;
        }
        self.image[at..at + size]
            .iter()
            .fold(0, |acc, &b| (acc << 8) | u32::from(b))
    }

    pub fn peek_word(&self, off: u32) -> Option<u16> {
        let at = off as usize;
        (at + 2 <= self.image.len())
            .then(|| u16::from_be_bytes([self.image[at], self.image[at + 1]]))
    }

    pub fn peek_byte(&self, off: u32) -> Option<u8> {
        self.image.get(off as usize).copied()
    }

    /// A guest write into the region. Only the doorbell longword is
    /// writable; the call fires on the write that completes it (a 68000
    /// lands a `move.l` as two word writes, high word first). `address_mask`
    /// is the CPU's, so guest pointers are folded the way the CPU folds
    /// them (24 bits on a 68000, the full 32 on a 68020+). `cck` and
    /// `frame` stamp idle markers and registrations. Returns true when the
    /// call wrote guest memory (function 82 clears the caller's `out`
    /// buffer), so the CPU adapter can drop its data cache.
    pub fn write(
        &mut self,
        off: u32,
        size: usize,
        value: u32,
        mem: &mut Memory,
        address_mask: u32,
        cck: u64,
        frame: u64,
    ) -> bool {
        if !(OFF_DOORBELL..OFF_DOORBELL + 4).contains(&off) {
            return false;
        }
        let at = off as usize;
        for i in 0..size {
            if at + i < self.image.len() {
                self.image[at + i] = (value >> (8 * (size - 1 - i))) as u8;
            }
        }
        if Self::completes_long_reg(off, size, OFF_DOORBELL) {
            let sp = self.image_long(OFF_DOORBELL);
            return self.ring_doorbell(sp, mem, address_mask, cck, frame);
        }
        false
    }

    /// Whether this write is the one that completes the longword register
    /// at `reg_off`: the whole 32 bits in one access (a 32-bit data bus), or
    /// the low word of the split a 68000's 16-bit bus makes of a `move.l`
    /// (high word to `reg_off`, then low word to `reg_off + 2`). Same rule
    /// as the services board's doorbell (`FilesysBoard::completes_long_reg`).
    fn completes_long_reg(off: u32, size: usize, reg_off: u32) -> bool {
        (size == 4 && off == reg_off) || (size == 2 && off == reg_off + 2)
    }

    fn latch(&mut self, off: u32, value: u32) {
        let at = off as usize;
        self.image[at..at + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn image_long(&self, off: u32) -> u32 {
        let at = off as usize;
        u32::from_be_bytes(self.image[at..at + 4].try_into().unwrap())
    }

    /// The doorbell: `sp` is the caller's A7 as the inner routine saw it.
    fn ring_doorbell(
        &mut self,
        sp: u32,
        mem: &mut Memory,
        address_mask: u32,
        cck: u64,
        frame: u64,
    ) -> bool {
        let sp = sp & address_mask;
        // The function number must be readable; the arguments are read as
        // WinUAE reads them, straight off the stack, and a caller with a
        // short frame (or a stack at the top of RAM) sees 0 for whatever
        // lies beyond it rather than losing the call.
        let arg = |n: u32| guest_long(mem, sp.wrapping_add(ARG_BASE + 4 * n) & address_mask);
        let Some(function) = arg(0) else {
            log::debug!(
                "uaelib: doorbell rung with A7 {sp:#010X} outside RAM or ROM; call ignored"
            );
            self.latch(OFF_RESULT, 0);
            return false;
        };
        let args = [
            arg(1).unwrap_or(0),
            arg(2).unwrap_or(0),
            arg(3).unwrap_or(0),
            arg(4).unwrap_or(0),
            arg(5).unwrap_or(0),
        ];
        let [a1, a2, a3, a4, a5] = args;
        let (d0, touched) = self.call(
            function,
            [a1, a2, a3, a4, a5],
            mem,
            address_mask,
            cck,
            frame,
        );
        self.latch(OFF_RESULT, d0);
        touched
    }

    /// The trap body: `function` is `uaelib_demux`'s ARG0, `args` ARG1..5.
    /// Returns D0 and whether guest memory was written.
    pub fn call(
        &mut self,
        function: u32,
        args: [u32; 5],
        mem: &mut Memory,
        address_mask: u32,
        cck: u64,
        frame: u64,
    ) -> (u32, bool) {
        match function {
            FN_CFG_MODIFY => self.cfg_modify(args, mem, address_mask),
            FN_DEBUG_LOG => self.debug_log(args[0], mem, address_mask),
            FN_DEBUG_CMD => self.debug_cmd(args, mem, address_mask, cck, frame),
            other => {
                log::trace!("uaelib: function {other} is not provided; returning 0");
                (0, false)
            }
        }
    }

    /// Function 82, WinUAE `cfgfile_uaelib_modify`: `index` -1 applies the
    /// `"key value"` pairs in the line at `parms` (`size` 0 = NUL-terminated),
    /// clearing the caller's `out` string first; any other index reads the
    /// configuration enumeration WinUAE keeps, which we never start. WinUAE
    /// returns 0 from the apply path whether or not it understood the keys.
    fn cfg_modify(&mut self, args: [u32; 5], mem: &mut Memory, address_mask: u32) -> (u32, bool) {
        let [index, parms, size, out, outsize] = args;
        if outsize as usize >= CFG_STRING_MAX {
            return (0, false);
        }
        let touched = out != 0 && outsize > 0 && guest_put_byte(mem, out & address_mask, 0);
        let line = read_config_line(mem, parms & address_mask, size as usize, address_mask);
        if index != 0xFFFF_FFFF {
            return (0xFFFF_FFFF, touched);
        }
        let tokens: Vec<&str> = line.split_ascii_whitespace().collect();
        if tokens.len() < 2 {
            // WinUAE snapshots its configuration for enumeration here.
            return (0xFFFF_FFFF, touched);
        }
        for pair in tokens.chunks_exact(2) {
            self.apply_config_option(pair[0], pair[1]);
        }
        (0, touched)
    }

    fn apply_config_option(&mut self, key: &str, value: &str) {
        match key.to_ascii_lowercase().as_str() {
            // cfgfile_yesno accepts exactly these four, case-insensitively.
            "warp" => match value.to_ascii_lowercase().as_str() {
                "yes" | "true" => self.pending_warp = Some(true),
                "no" | "false" => self.pending_warp = Some(false),
                other => log::debug!("uaelib: warp {other:?} is not a yes/no value; ignored"),
            },
            "cpu_speed" | "cpu_cycle_exact" | "cpu_memory_cycle_exact" | "blitter_cycle_exact" => {
                log::debug!(
                    "uaelib: {key} = {value} accepted and ignored: the core is always cycle-exact"
                );
            }
            _ => log::debug!("uaelib: unsupported configuration key {key} = {value}"),
        }
    }

    /// Function 86, WinUAE `write_log("DBG: %s\n", string)`: returns 1 for a
    /// readable string, 0 otherwise.
    fn debug_log(&mut self, ptr: u32, mem: &Memory, address_mask: u32) -> (u32, bool) {
        let Some(bytes) = guest_cstring(mem, ptr & address_mask, DEBUG_TEXT_MAX, address_mask)
        else {
            return (0, false);
        };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        self.echo(&text);
        self.push_event(DebugEvent::Log(text));
        (1, false)
    }

    /// The host console echo, in step with serial output. WinUAE prints
    /// `"DBG: %s\n"` verbatim, which leaves a blank line after a `KPrintF`
    /// that already ends in a newline; the terminator is normalised here
    /// instead.
    fn echo(&mut self, text: &str) {
        if self.speculative_host_quiet {
            return;
        }
        // The console pane's mirror rides the same run-ahead gate as
        // stdout, but not the stdout mute: silencing the terminal (test
        // harnesses running many machines) must not blind the pane.
        for line in text.split('\n').filter(|line| !line.is_empty()) {
            if self.console_lines.len() >= CONSOLE_LINE_CAPACITY {
                self.console_lines.pop_front();
            }
            self.console_lines.push_back(line.to_string());
        }
        if self.stdout_muted {
            return;
        }
        #[cfg(test)]
        {
            self.echoed.push(text.to_string());
        }
        #[cfg(not(test))]
        {
            use std::io::Write as _;
            let mut out = std::io::stdout().lock();
            let _ = write!(out, "DBG: {text}");
            if !text.ends_with('\n') {
                let _ = out.write_all(b"\n");
            }
            let _ = out.flush();
        }
    }

    fn push_event(&mut self, event: DebugEvent) {
        if self.debug_events.len() >= DEBUG_EVENT_CAPACITY {
            self.debug_events.pop_front();
            self.debug_dropped = self.debug_dropped.saturating_add(1);
        }
        self.debug_events.push_back(event);
    }

    /// Function 88, the template's `debug_cmd(cmd, a2, a3, a4)`.
    fn debug_cmd(
        &mut self,
        args: [u32; 5],
        mem: &mut Memory,
        address_mask: u32,
        cck: u64,
        frame: u64,
    ) -> (u32, bool) {
        let [cmd, a2, a3, a4, _] = args;
        match cmd {
            CMD_REGISTER_RESOURCE => {
                match parse_resource(mem, a2 & address_mask, address_mask, frame) {
                    Some(resource) => self.register_resource(resource),
                    None => log::debug!(
                        "uaelib: debug_resource at {:#010X} is not readable; ignored",
                        a2 & address_mask
                    ),
                }
            }
            CMD_SET_IDLE => self.idle.set(a2 != 0, cck),
            CMD_UNREGISTER_RESOURCE => self.unregister_resource(a2 & address_mask),
            CMD_CLEAR => {
                self.overlay.clear();
                self.overlay_dropped = 0;
            }
            CMD_RECT | CMD_FILLED_RECT => {
                // a2 = (left << 16) | top, a3 = (right << 16) | bottom,
                // both halves signed shorts; a4 = 0x00RRGGBB.
                let (l, t) = unpack_overlay_point(a2);
                let (r, b) = unpack_overlay_point(a3);
                if r > l && b > t {
                    let colour = a4 & 0x00FF_FFFF;
                    let cmd = if cmd == CMD_RECT {
                        OverlayCmd::Rect { l, t, r, b, colour }
                    } else {
                        OverlayCmd::FilledRect { l, t, r, b, colour }
                    };
                    self.push_overlay(cmd);
                }
            }
            CMD_TEXT => {
                // The string is read at call time, like function 86: the
                // guest's buffer may be reused the moment the call returns.
                let (l, t) = unpack_overlay_point(a2);
                match guest_cstring(mem, a3 & address_mask, OVERLAY_TEXT_MAX, address_mask) {
                    Some(bytes) => self.push_overlay(OverlayCmd::Text {
                        l,
                        t,
                        text: String::from_utf8_lossy(&bytes).into_owned(),
                        colour: a4 & 0x00FF_FFFF,
                    }),
                    None => log::debug!(
                        "uaelib: overlay text at {:#010X} is not readable; dropped",
                        a3 & address_mask
                    ),
                }
            }
            CMD_LOAD => {
                return self.debug_load(a2 & address_mask, a3, mem, address_mask);
            }
            CMD_SAVE => {
                self.debug_save(a2 & address_mask, a3, a4, mem, address_mask);
            }
            other => log::trace!("uaelib: debug command {other} is not provided"),
        }
        (0, false)
    }

    /// Give the opt-in file commands their one host authority. `root` must
    /// already be canonical. It is opened once; guest operations never look
    /// it up again by ambient host path.
    pub fn set_file_root(&mut self, root: Option<PathBuf>) -> std::io::Result<()> {
        self.file_authority = match root {
            None => None,
            #[cfg(not(target_arch = "wasm32"))]
            Some(root) => Some(FileAuthority {
                dir: Arc::new(Dir::open_ambient_dir(&root, ambient_authority())?),
                root,
            }),
            #[cfg(target_arch = "wasm32")]
            Some(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "host file authority is unavailable in browser builds",
                ));
            }
        };
        Ok(())
    }

    /// Host-only file authority, used by the emulator to preserve it across
    /// save-state, reverse, and run-ahead restores.
    pub fn file_root(&self) -> Option<&Path> {
        self.file_authority
            .as_ref()
            .map(|authority| authority.root.as_path())
    }

    pub(crate) fn file_authority(&self) -> Option<FileAuthority> {
        self.file_authority.clone()
    }

    pub(crate) fn set_file_authority(&mut self, authority: Option<FileAuthority>) {
        self.file_authority = authority;
    }

    /// Parse a guest filename as a relative path. Absolute paths, prefixes and
    /// `..` never reach the capability-backed host filesystem.
    fn debug_file_name(&self, mem: &Memory, ptr: u32, address_mask: u32) -> Option<PathBuf> {
        self.file_authority.as_ref()?;
        let bytes = guest_cstring(mem, ptr & address_mask, DEBUG_FILE_NAME_MAX, address_mask)?;
        let name = std::str::from_utf8(&bytes).ok()?;
        let relative = Path::new(name);
        if name.is_empty()
            || relative.is_absolute()
            || relative.components().any(|part| {
                matches!(
                    part,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            log::debug!("uaelib: rejected file name {name:?}");
            return None;
        }
        Some(relative.to_path_buf())
    }

    fn debug_load(
        &self,
        address: u32,
        name_ptr: u32,
        mem: &mut Memory,
        address_mask: u32,
    ) -> (u32, bool) {
        use std::io::Read as _;

        let Some(relative) = self.debug_file_name(mem, name_ptr, address_mask) else {
            return (0, false);
        };
        let authority = self
            .file_authority
            .as_ref()
            .expect("checked by debug_file_name");
        let display_path = authority.root.join(&relative);
        let loaded = (|| -> std::io::Result<Vec<u8>> {
            let file = authority.open(&relative)?;
            if file.metadata()?.len() > DEBUG_FILE_MAX as u64 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "file exceeds the 16 MiB uaelib limit",
                ));
            }
            let mut bytes = Vec::new();
            file.take(DEBUG_FILE_MAX as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > DEBUG_FILE_MAX {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "file grew beyond the 16 MiB uaelib limit",
                ));
            }
            Ok(bytes)
        })();
        let bytes = match loaded {
            Ok(bytes) => bytes,
            Err(e) => {
                log::debug!("uaelib: debug_load {} failed: {e}", display_path.display());
                return (0, false);
            }
        };
        if !guest_span_fits(address, bytes.len(), address_mask)
            || (0..bytes.len()).any(|i| dma_read_byte(mem, address + i as u32).is_none())
        {
            log::debug!(
                "uaelib: debug_load {} destination {address:#010X}+{} is not writable RAM",
                display_path.display(),
                bytes.len()
            );
            return (0, false);
        }
        for (i, byte) in bytes.iter().copied().enumerate() {
            // The preflight above proved the whole range writable, so this
            // cannot leave a partial transfer behind.
            let written = dma_write_byte(mem, address + i as u32, byte);
            debug_assert!(written);
        }
        (bytes.len() as u32, !bytes.is_empty())
    }

    fn debug_save(&self, address: u32, size: u32, name_ptr: u32, mem: &Memory, address_mask: u32) {
        use std::io::Write as _;

        let Ok(size) = usize::try_from(size) else {
            return;
        };
        if size > DEBUG_FILE_MAX || !guest_span_fits(address, size, address_mask) {
            log::debug!("uaelib: rejected debug_save size {size}");
            return;
        }
        let Some(bytes) = guest_bytes(mem, address, size, address_mask) else {
            log::debug!("uaelib: debug_save source {address:#010X}+{size} is unreadable");
            return;
        };
        let Some(relative) = self.debug_file_name(mem, name_ptr, address_mask) else {
            return;
        };
        // A speculative run-ahead frame will be replayed on the committed
        // timeline. Delay its irreversible host write until that replay.
        if self.speculative_host_quiet {
            return;
        }
        let authority = self
            .file_authority
            .as_ref()
            .expect("checked by debug_file_name");
        let display_path = authority.root.join(&relative);
        let saved = authority
            .create(&relative)
            .and_then(|mut file| file.write_all(&bytes));
        if let Err(e) = saved {
            log::debug!("uaelib: debug_save {} failed: {e}", display_path.display());
        }
    }

    fn register_resource(&mut self, resource: DebugResource) {
        if let Some(slot) = self
            .resources
            .iter_mut()
            .find(|r| r.address == resource.address)
        {
            *slot = resource.clone();
            self.push_event(DebugEvent::Resource {
                action: ResourceAction::Replaced,
                resource,
            });
            return;
        }
        if self.resources.len() >= RESOURCE_MAX {
            log::warn!(
                "uaelib: resource registry full ({RESOURCE_MAX}); {} at {:#010X} not recorded",
                resource.name,
                resource.address
            );
            return;
        }
        self.resources.push(resource.clone());
        self.push_event(DebugEvent::Resource {
            action: ResourceAction::Registered,
            resource,
        });
    }

    fn unregister_resource(&mut self, address: u32) {
        if address == 0 {
            self.clear_registry();
            return;
        }
        if let Some(pos) = self.resources.iter().position(|r| r.address == address) {
            let resource = self.resources.remove(pos);
            self.push_event(DebugEvent::Resource {
                action: ResourceAction::Unregistered,
                resource,
            });
        }
    }

    /// Append to the overlay display list. At the cap the NEW command is
    /// dropped (the entries the guest asked for first stand; `clear`
    /// restarts the list), counted so diagnostics can say so.
    fn push_overlay(&mut self, cmd: OverlayCmd) {
        if self.overlay.len() >= OVERLAY_CAP {
            self.overlay_dropped = self.overlay_dropped.saturating_add(1);
            return;
        }
        self.overlay.push(cmd);
    }

    /// Frame boundary: closes the idle accounting of the frame just ended.
    pub fn note_frame_start(&mut self, cck: u64) {
        self.idle.note_frame_start(cck);
    }

    /// The guest's latest `warp` request, if any since the last take.
    pub fn take_warp_request(&mut self) -> Option<bool> {
        self.pending_warp.take()
    }

    /// Queued debug events and the number dropped since the last take.
    pub fn take_debug_events(&mut self) -> (Vec<DebugEvent>, u64) {
        let events = self.debug_events.drain(..).collect();
        let dropped = std::mem::take(&mut self.debug_dropped);
        (events, dropped)
    }

    /// Queued console-pane lines since the last take.
    pub fn take_console_lines(&mut self) -> Vec<String> {
        self.console_lines.drain(..).collect()
    }

    /// Discard queued events: a fresh subscription starts clean.
    pub fn clear_debug_events(&mut self) {
        self.debug_events.clear();
        self.debug_dropped = 0;
    }

    pub fn resources(&self) -> &[DebugResource] {
        &self.resources
    }

    /// The overlay display list, in the order the guest drew it.
    pub fn overlay(&self) -> &[OverlayCmd] {
        &self.overlay
    }

    pub fn overlay_dropped(&self) -> u64 {
        self.overlay_dropped
    }

    /// Test hook: append an overlay command as function 88 would.
    #[cfg(test)]
    pub fn queue_overlay(&mut self, cmd: OverlayCmd) {
        self.push_overlay(cmd);
    }

    pub fn idle(&self) -> &IdleAccounting {
        &self.idle
    }

    pub fn set_speculative_host_quiet(&mut self, on: bool) {
        self.speculative_host_quiet = on;
    }

    /// Silence the console echo (the CCP test harnesses run many machines).
    pub fn mute_stdout(&mut self) {
        self.stdout_muted = true;
    }

    /// Test hook: latch a warp request as function 82 would.
    pub fn request_warp(&mut self, on: bool) {
        self.pending_warp = Some(on);
    }

    /// Test hook: queue a debug line as function 86 would (no echo).
    pub fn queue_debug_line(&mut self, text: &str) {
        self.push_event(DebugEvent::Log(text.to_string()));
    }
}

/// Unpack a packed overlay point: each half a signed short, clamped into
/// the 768x576 space (negative coordinates saturate to the edge rather
/// than wrapping to huge offsets).
fn unpack_overlay_point(packed: u32) -> (u16, u16) {
    let x = ((packed >> 16) as i16).clamp(0, OVERLAY_WIDTH as i16) as u16;
    let y = (packed as i16).clamp(0, OVERLAY_HEIGHT as i16) as u16;
    (x, y)
}

/// A guest byte the way a bus master sees it: RAM through the DMA view,
/// plus the Kickstart and extended ROMs (a 256K ROM mirrors across its
/// 512K window), `None` for anything unmapped.
fn guest_byte(mem: &Memory, addr: u32) -> Option<u8> {
    if let Some(b) = dma_read_byte(mem, addr) {
        return Some(b);
    }
    let a = u64::from(addr);
    if (crate::memory::ROM_BASE..0x0100_0000).contains(&a) && !mem.rom.is_empty() {
        let off = (a - crate::memory::ROM_BASE) as usize;
        return Some(mem.rom[off % mem.rom.len()]);
    }
    let ext = a.wrapping_sub(mem.extended_rom_base) as usize;
    if a >= mem.extended_rom_base && ext < mem.extended_rom.len() {
        return Some(mem.extended_rom[ext]);
    }
    None
}

fn guest_long(mem: &Memory, addr: u32) -> Option<u32> {
    let mut v = 0u32;
    for i in 0..4 {
        v = (v << 8) | u32::from(guest_byte(mem, addr.wrapping_add(i))?);
    }
    Some(v)
}

/// `n` guest bytes from `addr`, `None` if any is unreadable.
fn guest_bytes(mem: &Memory, addr: u32, n: usize, address_mask: u32) -> Option<Vec<u8>> {
    (0..n)
        .map(|i| guest_byte(mem, addr.wrapping_add(i as u32) & address_mask))
        .collect()
}

/// Whether a transfer stays within both `u32` and the CPU's address bus.
/// File commands reject wrapping spans instead of silently folding their tail
/// to address zero as ordinary individual CPU accesses would.
fn guest_span_fits(addr: u32, len: usize, address_mask: u32) -> bool {
    let Some(last) = len.checked_sub(1) else {
        return addr <= address_mask;
    };
    u32::try_from(last)
        .ok()
        .and_then(|last| addr.checked_add(last))
        .is_some_and(|end| end <= address_mask)
}

/// A NUL-terminated guest string of at most `max` bytes; `None` when its
/// first byte is unreadable. Stops early at an unreadable byte.
fn guest_cstring(mem: &Memory, addr: u32, max: usize, address_mask: u32) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    for i in 0..max {
        match guest_byte(mem, addr.wrapping_add(i as u32) & address_mask) {
            Some(0) => break,
            Some(b) => bytes.push(b),
            None if i == 0 => return None,
            None => break,
        }
    }
    Some(bytes)
}

fn guest_put_byte(mem: &mut Memory, addr: u32, value: u8) -> bool {
    dma_write_byte(mem, addr, value)
}

/// The parameter line of function 82: `size` 0 means NUL-terminated; the
/// copy stops at a line break, a NUL, an unreadable byte, or the cap.
fn read_config_line(mem: &Memory, parms: u32, size: usize, address_mask: u32) -> String {
    let limit = if size == 0 {
        CFG_STRING_MAX
    } else {
        size.min(CFG_STRING_MAX)
    };
    let mut bytes = Vec::new();
    for i in 0..limit {
        match guest_byte(mem, parms.wrapping_add(i as u32) & address_mask) {
            Some(0) | Some(b'\n') | Some(b'\r') | None => break,
            Some(b) => bytes.push(b),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Decode the template's `struct debug_resource` at `ptr`.
fn parse_resource(mem: &Memory, ptr: u32, address_mask: u32, frame: u64) -> Option<DebugResource> {
    let bytes = guest_bytes(mem, ptr, RESOURCE_STRUCT_SIZE, address_mask)?;
    let be32 = |o: usize| u32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let be16 = |o: usize| u16::from_be_bytes([bytes[o], bytes[o + 1]]);
    let name_bytes = &bytes[8..40];
    let name_len = name_bytes
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(name_bytes.len());
    let name = String::from_utf8_lossy(&name_bytes[..name_len]).into_owned();
    let kind = match be16(40) {
        0 => ResourceKind::Bitmap {
            width: be16(44),
            height: be16(46),
            planes: be16(48),
        },
        1 => ResourceKind::Palette { entries: be16(44) },
        2 => ResourceKind::Copperlist,
        other => ResourceKind::Unknown(other),
    };
    Some(DebugResource {
        address: be32(0) & address_mask,
        size: be32(4),
        name,
        kind,
        flags: be16(42),
        registered_frame: frame,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zorro::ZorroChain;

    const MASK24: u32 = 0x00FF_FFFF;
    const MASK32: u32 = 0xFFFF_FFFF;

    fn memory() -> Memory {
        Memory::placeholder(512 * 1024, 0, ZorroChain::default())
    }

    fn put(mem: &mut Memory, addr: u32, bytes: &[u8]) {
        for (i, b) in bytes.iter().enumerate() {
            assert!(
                dma_write_byte(mem, addr + i as u32, *b),
                "{addr:#010X} not RAM"
            );
        }
    }

    fn put_long(mem: &mut Memory, addr: u32, value: u32) {
        put(mem, addr, &value.to_be_bytes());
    }

    fn put_str(mem: &mut Memory, addr: u32, s: &str) {
        put(mem, addr, s.as_bytes());
        put(mem, addr + s.len() as u32, &[0]);
    }

    fn byte(mem: &Memory, addr: u32) -> u8 {
        dma_read_byte(mem, addr).expect("RAM")
    }

    /// Lay out a C call frame as the stub sees it at the doorbell: the
    /// inner routine's return address, the caller's, then the function
    /// number and its arguments.
    fn frame(mem: &mut Memory, sp: u32, function: u32, args: &[u32]) {
        put_long(mem, sp, UAELIB_BASE + 6);
        put_long(mem, sp + 4, 0x0000_1234);
        put_long(mem, sp + 8, function);
        for (i, a) in args.iter().enumerate() {
            put_long(mem, sp + 12 + 4 * i as u32, *a);
        }
    }

    /// Ring the doorbell the way a 68000 does: high word, then low word.
    fn ring_split(lib: &mut UaeLib, mem: &mut Memory, sp: u32) -> bool {
        assert!(!lib.write(OFF_DOORBELL, 2, sp >> 16, mem, MASK24, 0, 0));
        lib.write(OFF_DOORBELL + 2, 2, sp & 0xFFFF, mem, MASK24, 0, 0)
    }

    fn result(lib: &UaeLib) -> u32 {
        lib.read(OFF_RESULT, 4)
    }

    fn cfg_modify(lib: &mut UaeLib, mem: &mut Memory, line: &str) -> (u32, bool) {
        put_str(mem, 0x2000, line);
        lib.call(
            FN_CFG_MODIFY,
            [0xFFFF_FFFF, 0x2000, 0, 0x3000, 1],
            mem,
            MASK24,
            0,
            0,
        )
    }

    fn resource_bytes(
        address: u32,
        size: u32,
        name: &str,
        kind: u16,
        flags: u16,
        union: [u16; 3],
    ) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&address.to_be_bytes());
        b.extend_from_slice(&size.to_be_bytes());
        let mut n = [0u8; 32];
        n[..name.len()].copy_from_slice(name.as_bytes());
        b.extend_from_slice(&n);
        b.extend_from_slice(&kind.to_be_bytes());
        b.extend_from_slice(&flags.to_be_bytes());
        for u in union {
            b.extend_from_slice(&u.to_be_bytes());
        }
        assert_eq!(b.len(), RESOURCE_STRUCT_SIZE);
        b
    }

    fn register(lib: &mut UaeLib, mem: &mut Memory, at: u32, bytes: &[u8], frame: u64) {
        put(mem, at, bytes);
        lib.call(
            FN_DEBUG_CMD,
            [CMD_REGISTER_RESOURCE, at, 0, 0, 0],
            mem,
            MASK24,
            0,
            frame,
        );
    }

    #[test]
    fn image_matches_the_documented_layout_and_starts_with_the_detection_word() {
        let lib = UaeLib::new();
        let expected: [u8; 32] = [
            0x4E, 0xB9, 0x00, 0xF0, 0xFF, 0x68, // JSR ($F0FF68).L
            0x4E, 0x75, // RTS
            0x23, 0xCF, 0x00, 0xF0, 0xFF, 0x7C, // MOVE.L A7,($F0FF7C).L
            0x20, 0x39, 0x00, 0xF0, 0xFF, 0x78, // MOVE.L ($F0FF78).L,D0
            0x4E, 0x75, // RTS
            0, 0, // pad
            0, 0, 0, 0, // result
            0, 0, 0, 0, // doorbell
        ];
        assert_eq!(lib.image, expected);
        assert_eq!(lib.read(0, 2), u32::from(DETECTION_WORD));
        assert!(UaeLib::decodes(UAELIB_BASE));
        assert!(UaeLib::decodes(UAELIB_BASE + 0x1F));
        assert!(!UaeLib::decodes(UAELIB_BASE + 0x20));
        assert!(!UaeLib::decodes(UAELIB_BASE - 2));
        assert!(!UaeLib::decodes(0x00F0_0000));
    }

    #[test]
    fn reads_serve_the_image_at_any_size_and_alignment() {
        let lib = UaeLib::new();
        assert_eq!(lib.read(0, 4), 0x4EB9_00F0);
        assert_eq!(lib.read(1, 1), 0xB9);
        assert_eq!(lib.read(3, 3), 0x00F0_FF68);
        assert_eq!(lib.read(0x16, 2), 0);
        assert_eq!(lib.read(0x1E, 4), 0, "a read past the region floats");
        assert_eq!(lib.peek_word(0), Some(0x4EB9));
        assert_eq!(lib.peek_word(0x1E), Some(0));
        assert_eq!(lib.peek_word(0x1F), None);
        assert_eq!(lib.peek_byte(0x1F), Some(0));
        assert_eq!(lib.peek_byte(0x20), None);
    }

    #[test]
    fn doorbell_fires_only_on_the_write_that_completes_the_longword() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        put_str(&mut mem, 0x4000, "hi");
        frame(&mut mem, 0x1000, FN_DEBUG_LOG, &[0x4000]);

        // A 68000 lands the high word first: nothing fires until the low word.
        assert!(!ring_split(&mut lib, &mut mem, 0x1000));
        assert_eq!(result(&lib), 1);
        assert_eq!(
            lib.take_debug_events().0,
            vec![DebugEvent::Log("hi".into())]
        );

        // A 32-bit bus writes the longword in one access.
        assert!(!lib.write(OFF_DOORBELL, 4, 0x1000, &mut mem, MASK32, 0, 0));
        assert_eq!(lib.take_debug_events().0.len(), 1);

        // A lone high word or a byte never rings.
        lib.write(OFF_DOORBELL, 2, 0, &mut mem, MASK24, 0, 0);
        lib.write(OFF_DOORBELL + 3, 1, 0, &mut mem, MASK24, 0, 0);
        assert!(lib.take_debug_events().0.is_empty());
        assert_eq!(
            lib.peek_word(OFF_DOORBELL),
            Some(0),
            "the high word latched"
        );

        // The code and the result latch are read-only.
        assert!(!lib.write(0, 2, 0xFFFF, &mut mem, MASK24, 0, 0));
        assert!(!lib.write(OFF_RESULT, 4, 0xFFFF_FFFF, &mut mem, MASK24, 0, 0));
        assert_eq!(lib.read(0, 2), 0x4EB9);
        assert_eq!(result(&lib), 1);
    }

    #[test]
    fn function_82_walks_key_value_pairs_and_latches_only_the_warp_key() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        put(&mut mem, 0x3000, &[0xAA]);

        assert_eq!(cfg_modify(&mut lib, &mut mem, "warp true"), (0, true));
        assert_eq!(lib.take_warp_request(), Some(true));
        assert_eq!(byte(&mem, 0x3000), 0, "the out string is cleared");
        assert_eq!(lib.take_warp_request(), None);

        for (line, want) in [
            ("warp yes", Some(true)),
            ("WARP True", Some(true)),
            ("warp no", Some(false)),
            ("warp false", Some(false)),
            ("warp maybe", None),
            ("cpu_speed max", None),
            ("cpu_cycle_exact false", None),
            ("cpu_cycle_exact false warp true", Some(true)),
            ("warp true extra", Some(true)),
        ] {
            assert_eq!(cfg_modify(&mut lib, &mut mem, line).0, 0, "{line}");
            assert_eq!(lib.take_warp_request(), want, "{line}");
        }

        // WinUAE refuses an oversized out buffer before touching anything.
        put(&mut mem, 0x3000, &[0xAA]);
        put_str(&mut mem, 0x2000, "warp true");
        let r = lib.call(
            FN_CFG_MODIFY,
            [0xFFFF_FFFF, 0x2000, 0, 0x3000, CFG_STRING_MAX as u32],
            &mut mem,
            MASK24,
            0,
            0,
        );
        assert_eq!(r, (0, false));
        assert_eq!(byte(&mem, 0x3000), 0xAA);
        assert_eq!(lib.take_warp_request(), None);

        // No out buffer: nothing written, still applied.
        put_str(&mut mem, 0x2000, "warp true");
        let r = lib.call(
            FN_CFG_MODIFY,
            [0xFFFF_FFFF, 0x2000, 0, 0, 0],
            &mut mem,
            MASK24,
            0,
            0,
        );
        assert_eq!(r, (0, false));
        assert_eq!(lib.take_warp_request(), Some(true));
    }

    #[test]
    fn function_82_reports_no_enumeration_for_an_index_or_a_single_token() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        put_str(&mut mem, 0x2000, "warp true");
        let r = lib.call(
            FN_CFG_MODIFY,
            [5, 0x2000, 0, 0x3000, 1],
            &mut mem,
            MASK24,
            0,
            0,
        );
        assert_eq!(r, (0xFFFF_FFFF, true));
        assert_eq!(lib.take_warp_request(), None);
        assert_eq!(cfg_modify(&mut lib, &mut mem, "warp"), (0xFFFF_FFFF, true));
        assert_eq!(cfg_modify(&mut lib, &mut mem, ""), (0xFFFF_FFFF, true));
        assert_eq!(lib.take_warp_request(), None);
    }

    #[test]
    fn function_82_measures_an_unsized_string_and_stops_at_a_line_break() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        assert_eq!(cfg_modify(&mut lib, &mut mem, "warp true\nwarp false").0, 0);
        assert_eq!(lib.take_warp_request(), Some(true));
        // An explicit size cuts the line short: one token, no apply.
        put_str(&mut mem, 0x2000, "warp true");
        let r = lib.call(
            FN_CFG_MODIFY,
            [0xFFFF_FFFF, 0x2000, 4, 0, 0],
            &mut mem,
            MASK24,
            0,
            0,
        );
        assert_eq!(r, (0xFFFF_FFFF, false));
        assert_eq!(lib.take_warp_request(), None);
        // An unreadable parameter pointer reads as an empty line.
        let r = lib.call(
            FN_CFG_MODIFY,
            [0xFFFF_FFFF, 0x00F0_0000, 0, 0, 0],
            &mut mem,
            MASK24,
            0,
            0,
        );
        assert_eq!(r, (0xFFFF_FFFF, false));
    }

    #[test]
    fn function_86_queues_a_valid_string_and_returns_zero_for_an_unreadable_pointer() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        put_str(&mut mem, 0x2000, "hello\n");
        let r = lib.call(FN_DEBUG_LOG, [0x2000, 0, 0, 0, 0], &mut mem, MASK24, 0, 0);
        assert_eq!(r, (1, false));
        assert_eq!(lib.echoed, vec!["hello\n".to_string()]);
        assert_eq!(
            lib.take_debug_events(),
            (vec![DebugEvent::Log("hello\n".into())], 0)
        );
        let r = lib.call(
            FN_DEBUG_LOG,
            [0x00F0_0000, 0, 0, 0, 0],
            &mut mem,
            MASK24,
            0,
            0,
        );
        assert_eq!(r, (0, false));
        assert!(lib.take_debug_events().0.is_empty());
        assert_eq!(lib.echoed.len(), 1);
    }

    #[test]
    fn console_ring_mirrors_the_echo_and_drains_once() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        put_str(&mut mem, 0x2000, "one\ntwo\n");
        lib.call(FN_DEBUG_LOG, [0x2000, 0, 0, 0, 0], &mut mem, MASK24, 0, 0);
        assert_eq!(lib.take_console_lines(), vec!["one", "two"]);
        assert_eq!(lib.take_console_lines(), Vec::<String>::new());
        // The CCP-facing event queue is untouched by the console mirror.
        assert_eq!(lib.take_debug_events().0.len(), 1);
    }

    #[test]
    fn console_ring_is_gated_by_speculative_quiet_but_not_stdout_mute() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        put_str(&mut mem, 0x2000, "spec");
        lib.set_speculative_host_quiet(true);
        lib.call(FN_DEBUG_LOG, [0x2000, 0, 0, 0, 0], &mut mem, MASK24, 0, 0);
        assert!(lib.take_console_lines().is_empty());
        lib.set_speculative_host_quiet(false);
        lib.mute_stdout();
        lib.call(FN_DEBUG_LOG, [0x2000, 0, 0, 0, 0], &mut mem, MASK24, 0, 0);
        assert_eq!(lib.take_console_lines(), vec!["spec"]);
        assert!(lib.echoed.is_empty(), "stdout stayed muted");
    }

    #[test]
    fn console_ring_caps_at_capacity() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        put_str(&mut mem, 0x2000, "x");
        lib.mute_stdout();
        for _ in 0..CONSOLE_LINE_CAPACITY + 3 {
            lib.call(FN_DEBUG_LOG, [0x2000, 0, 0, 0, 0], &mut mem, MASK24, 0, 0);
        }
        assert_eq!(lib.take_console_lines().len(), CONSOLE_LINE_CAPACITY);
    }

    #[test]
    fn debug_event_queue_is_bounded_and_counts_drops() {
        let mut lib = UaeLib::new();
        for i in 0..DEBUG_EVENT_CAPACITY + 3 {
            lib.queue_debug_line(&format!("line {i}"));
        }
        let (events, dropped) = lib.take_debug_events();
        assert_eq!(events.len(), DEBUG_EVENT_CAPACITY);
        assert_eq!(dropped, 3);
        assert_eq!(events[0], DebugEvent::Log("line 3".into()));
        assert_eq!(lib.take_debug_events(), (Vec::new(), 0));
        lib.queue_debug_line("x");
        lib.clear_debug_events();
        assert_eq!(lib.take_debug_events(), (Vec::new(), 0));
    }

    #[test]
    fn guest_pointers_are_masked_to_the_cpu_address_bus() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        put_str(&mut mem, 0x1000, "hi");
        // A 68000 drives 24 address lines: $FF001000 lands on chip RAM.
        let r = lib.call(
            FN_DEBUG_LOG,
            [0xFF00_1000, 0, 0, 0, 0],
            &mut mem,
            MASK24,
            0,
            0,
        );
        assert_eq!(r, (1, false));
        // A 32-bit CPU reaches nothing there...
        let r = lib.call(
            FN_DEBUG_LOG,
            [0xFF00_1000, 0, 0, 0, 0],
            &mut mem,
            MASK32,
            0,
            0,
        );
        assert_eq!(r, (0, false));
        // ...but does reach motherboard RAM above the 24-bit space, which a
        // 68000's fold would miss.
        mem.fit_mb_ram(1024 * 1024);
        let base = mem.mb_ram_base() as u32;
        put_str(&mut mem, base + 0x10, "far");
        let r = lib.call(
            FN_DEBUG_LOG,
            [base + 0x10, 0, 0, 0, 0],
            &mut mem,
            MASK32,
            0,
            0,
        );
        assert_eq!(r, (1, false));
        let r = lib.call(
            FN_DEBUG_LOG,
            [base + 0x10, 0, 0, 0, 0],
            &mut mem,
            MASK24,
            0,
            0,
        );
        assert_eq!(r, (0, false));
        assert_eq!(lib.take_debug_events().0.len(), 2);
    }

    #[test]
    fn other_function_numbers_return_zero_without_side_effects() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        for function in [FN_GET_VERSION, FN_CFG_READ, 1, 3, 1234] {
            frame(&mut mem, 0x1000, function, &[0x2000, 1, 2, 3, 4]);
            assert!(!ring_split(&mut lib, &mut mem, 0x1000), "{function}");
            assert_eq!(result(&lib), 0, "{function}");
        }
        assert!(lib.take_debug_events().0.is_empty());
        assert_eq!(lib.take_warp_request(), None);
        assert!(lib.resources().is_empty());
    }

    #[test]
    fn reset_restores_the_latches_and_drops_guest_state_but_keeps_the_queue() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        put_str(&mut mem, 0x4000, "x");
        frame(&mut mem, 0x1000, FN_DEBUG_LOG, &[0x4000]);
        ring_split(&mut lib, &mut mem, 0x1000);
        assert_eq!(result(&lib), 1);
        lib.request_warp(true);
        let bytes = resource_bytes(0x2_0000, 100, "r", 2, 0, [0; 3]);
        register(&mut lib, &mut mem, 0x5000, &bytes, 1);
        lib.call(
            FN_DEBUG_CMD,
            [CMD_SET_IDLE, 1, 0, 0, 0],
            &mut mem,
            MASK24,
            10,
            1,
        );
        assert!(lib.idle().used());

        lib.reset();
        assert_eq!(lib.image, IMAGE);
        assert_eq!(lib.take_warp_request(), None);
        assert!(lib.resources().is_empty());
        assert!(!lib.idle().used());
        let (events, _) = lib.take_debug_events();
        assert_eq!(
            events.len(),
            3,
            "log + registration survive, teardown queued"
        );
        assert!(
            matches!(
                &events[2],
                DebugEvent::Resource {
                    action: ResourceAction::Cleared,
                    ..
                }
            ),
            "a subscriber that saw the registration hears the reset clear it"
        );
    }

    #[test]
    fn stdout_echo_is_withheld_while_a_frame_is_speculative() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        put_str(&mut mem, 0x2000, "spec");
        lib.set_speculative_host_quiet(true);
        lib.call(FN_DEBUG_LOG, [0x2000, 0, 0, 0, 0], &mut mem, MASK24, 0, 0);
        assert!(lib.echoed.is_empty());
        assert_eq!(
            lib.take_debug_events().0.len(),
            1,
            "the event is guest state"
        );
        lib.set_speculative_host_quiet(false);
        lib.call(FN_DEBUG_LOG, [0x2000, 0, 0, 0, 0], &mut mem, MASK24, 0, 0);
        assert_eq!(lib.echoed, vec!["spec".to_string()]);
        lib.mute_stdout();
        lib.call(FN_DEBUG_LOG, [0x2000, 0, 0, 0, 0], &mut mem, MASK24, 0, 0);
        assert_eq!(lib.echoed.len(), 1);
    }

    #[test]
    fn function_88_registers_bitmap_palette_and_copperlist_from_the_guest_struct() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        let bitmap = resource_bytes(
            0x2_0000,
            51200,
            "screen",
            0,
            RESOURCE_FLAG_INTERLEAVED | RESOURCE_FLAG_MASKED,
            [320, 256, 5],
        );
        let palette = resource_bytes(0x3_0000, 64, "pal", 1, 0, [32, 0, 0]);
        let mut long_name = resource_bytes(0x4_0000, 1000, "", 2, RESOURCE_FLAG_HAM, [0; 3]);
        long_name[8..40].copy_from_slice(&[b'c'; 32]);
        let odd = resource_bytes(0x5_0000, 8, "odd", 7, 0, [1, 2, 3]);
        register(&mut lib, &mut mem, 0x5000, &bitmap, 61);
        register(&mut lib, &mut mem, 0x5100, &palette, 62);
        register(&mut lib, &mut mem, 0x5200, &long_name, 63);
        register(&mut lib, &mut mem, 0x5300, &odd, 64);

        let got = lib.resources();
        assert_eq!(got.len(), 4);
        assert_eq!(
            got[0],
            DebugResource {
                address: 0x2_0000,
                size: 51200,
                name: "screen".into(),
                kind: ResourceKind::Bitmap {
                    width: 320,
                    height: 256,
                    planes: 5,
                },
                flags: 3,
                registered_frame: 61,
            }
        );
        assert_eq!(got[0].kind_name(), "bitmap");
        assert_eq!(got[1].kind, ResourceKind::Palette { entries: 32 });
        assert_eq!(got[1].name, "pal");
        assert_eq!(got[2].kind, ResourceKind::Copperlist);
        assert_eq!(
            got[2].name,
            "c".repeat(32),
            "a full 32-byte name has no NUL"
        );
        assert_eq!(got[2].flags, RESOURCE_FLAG_HAM);
        assert_eq!(got[3].kind, ResourceKind::Unknown(7));
        assert_eq!(got[3].kind_name(), "unknown");

        let (events, dropped) = lib.take_debug_events();
        assert_eq!(dropped, 0);
        assert_eq!(events.len(), 4);
        assert!(events.iter().all(|e| matches!(
            e,
            DebugEvent::Resource {
                action: ResourceAction::Registered,
                ..
            }
        )));
    }

    #[test]
    fn reregistering_an_address_replaces_and_queues_replaced() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        let a = resource_bytes(0x2_0000, 100, "first", 2, 0, [0; 3]);
        let b = resource_bytes(0x2_0000, 200, "second", 1, 0, [8, 0, 0]);
        register(&mut lib, &mut mem, 0x5000, &a, 1);
        register(&mut lib, &mut mem, 0x5000, &b, 2);
        assert_eq!(lib.resources().len(), 1);
        assert_eq!(lib.resources()[0].name, "second");
        assert_eq!(lib.resources()[0].registered_frame, 2);
        let (events, _) = lib.take_debug_events();
        assert!(matches!(
            events[1],
            DebugEvent::Resource {
                action: ResourceAction::Replaced,
                ..
            }
        ));
    }

    #[test]
    fn unregister_removes_one_address_and_zero_clears_all() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        let a = resource_bytes(0x2_0000, 100, "a", 2, 0, [0; 3]);
        let b = resource_bytes(0x3_0000, 100, "b", 2, 0, [0; 3]);
        register(&mut lib, &mut mem, 0x5000, &a, 1);
        register(&mut lib, &mut mem, 0x5100, &b, 1);
        lib.take_debug_events();

        lib.call(
            FN_DEBUG_CMD,
            [CMD_UNREGISTER_RESOURCE, 0x2_0000, 0, 0, 0],
            &mut mem,
            MASK24,
            0,
            0,
        );
        assert_eq!(lib.resources().len(), 1);
        assert_eq!(lib.resources()[0].name, "b");
        lib.call(
            FN_DEBUG_CMD,
            [CMD_UNREGISTER_RESOURCE, 0x7_0000, 0, 0, 0],
            &mut mem,
            MASK24,
            0,
            0,
        );
        assert_eq!(
            lib.resources().len(),
            1,
            "an unknown address changes nothing"
        );
        let (events, _) = lib.take_debug_events();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            DebugEvent::Resource {
                action: ResourceAction::Unregistered,
                resource,
            } if resource.name == "a"
        ));

        register(&mut lib, &mut mem, 0x5000, &a, 1);
        lib.take_debug_events();
        lib.call(
            FN_DEBUG_CMD,
            [CMD_UNREGISTER_RESOURCE, 0, 0, 0, 0],
            &mut mem,
            MASK24,
            0,
            0,
        );
        assert!(lib.resources().is_empty());
        let (events, _) = lib.take_debug_events();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| matches!(
            e,
            DebugEvent::Resource {
                action: ResourceAction::Cleared,
                ..
            }
        )));
    }

    #[test]
    fn registry_is_bounded_and_an_unreadable_struct_is_ignored() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        for i in 0..RESOURCE_MAX as u32 + 1 {
            let bytes = resource_bytes(0x1_0000 + i * 0x100, 16, "r", 2, 0, [0; 3]);
            register(&mut lib, &mut mem, 0x5000, &bytes, 1);
        }
        assert_eq!(lib.resources().len(), RESOURCE_MAX);
        lib.call(
            FN_DEBUG_CMD,
            [CMD_REGISTER_RESOURCE, 0x00F0_0000, 0, 0, 0],
            &mut mem,
            MASK24,
            0,
            0,
        );
        assert_eq!(lib.resources().len(), RESOURCE_MAX);
    }

    #[test]
    fn idle_markers_accumulate_per_frame_and_reset_on_frame_start() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        lib.note_frame_start(0);
        assert!(!lib.idle().used());
        assert_eq!(lib.idle().last_frame(), None);

        lib.call(
            FN_DEBUG_CMD,
            [CMD_SET_IDLE, 1, 0, 0, 0],
            &mut mem,
            MASK24,
            100,
            0,
        );
        assert!(lib.idle().is_idle());
        lib.call(
            FN_DEBUG_CMD,
            [CMD_SET_IDLE, 1, 0, 0, 0],
            &mut mem,
            MASK24,
            200,
            0,
        );
        lib.call(
            FN_DEBUG_CMD,
            [CMD_SET_IDLE, 0, 0, 0, 0],
            &mut mem,
            MASK24,
            400,
            0,
        );
        assert!(!lib.idle().is_idle());
        lib.note_frame_start(1000);
        assert_eq!(lib.idle().last_frame(), Some((300, 1000)));
        assert_eq!(lib.idle().last_frame_idle_cck(), Some(300));

        // Idle across a frame boundary is split between the two frames.
        lib.call(
            FN_DEBUG_CMD,
            [CMD_SET_IDLE, 1, 0, 0, 0],
            &mut mem,
            MASK24,
            1500,
            0,
        );
        lib.note_frame_start(2000);
        assert_eq!(lib.idle().last_frame(), Some((500, 1000)));
        lib.note_frame_start(3000);
        assert_eq!(lib.idle().last_frame(), Some((1000, 1000)));
        assert!(
            lib.take_debug_events().0.is_empty(),
            "idle markers are not events"
        );
    }

    #[test]
    fn file_commands_are_disabled_without_an_explicit_root() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        put_str(&mut mem, 0x2000, "asset.bin");
        assert_eq!(
            lib.call(
                FN_DEBUG_CMD,
                [CMD_LOAD, 0x3000, 0x2000, 0, 0],
                &mut mem,
                MASK24,
                0,
                0,
            ),
            (0, false)
        );
        assert_eq!(
            lib.call(
                FN_DEBUG_CMD,
                [CMD_SAVE, 0x3000, 4, 0x2000, 0],
                &mut mem,
                MASK24,
                0,
                0,
            ),
            (0, false)
        );
        let r = lib.call(FN_DEBUG_CMD, [99, 0x2000, 3, 4, 0], &mut mem, MASK24, 0, 0);
        assert_eq!(r, (0, false));
        assert!(lib.take_debug_events().0.is_empty());
        assert!(lib.resources().is_empty());
        assert!(!lib.idle().used());
        assert!(lib.overlay().is_empty());
    }

    #[test]
    fn file_commands_round_trip_below_the_configured_root() {
        let dir = std::env::temp_dir().join(format!(
            "copperline-uaelib-files-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let root = std::fs::canonicalize(&dir).unwrap();
        std::fs::write(root.join("data/input.bin"), b"from host").unwrap();

        let mut lib = UaeLib::new();
        lib.set_file_root(Some(root.clone())).unwrap();
        let mut mem = memory();
        put_str(&mut mem, 0x2000, "data/input.bin");
        put_str(&mut mem, 0x2100, "data/output.bin");
        let loaded = lib.call(
            FN_DEBUG_CMD,
            [CMD_LOAD, 0x1_0000, 0x2000, 0, 0],
            &mut mem,
            MASK24,
            0,
            0,
        );
        assert_eq!(loaded, (9, true));
        assert_eq!(
            guest_bytes(&mem, 0x1_0000, 9, MASK24).unwrap(),
            b"from host"
        );

        put(&mut mem, 0x1_1000, b"from guest");
        let saved = lib.call(
            FN_DEBUG_CMD,
            [CMD_SAVE, 0x1_1000, 10, 0x2100, 0],
            &mut mem,
            MASK24,
            0,
            0,
        );
        assert_eq!(saved, (0, false));
        assert_eq!(
            std::fs::read(root.join("data/output.bin")).unwrap(),
            b"from guest"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn file_commands_reject_escapes_oversize_and_partly_unmapped_loads() {
        let dir = std::env::temp_dir().join(format!(
            "copperline-uaelib-files-reject-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let root = std::fs::canonicalize(&dir).unwrap();
        std::fs::write(root.join("small.bin"), b"four").unwrap();
        let oversized = root.join("oversized.bin");
        std::fs::File::create(&oversized)
            .unwrap()
            .set_len(DEBUG_FILE_MAX as u64 + 1)
            .unwrap();

        let mut lib = UaeLib::new();
        lib.set_file_root(Some(root.clone())).unwrap();
        let mut mem = memory();
        for (at, name) in [
            (0x2000, "../escape.bin"),
            (0x2100, "/absolute.bin"),
            (0x2200, "oversized.bin"),
            (0x2300, "small.bin"),
        ] {
            put_str(&mut mem, at, name);
        }
        for name in [0x2000, 0x2100, 0x2200] {
            assert_eq!(
                lib.call(
                    FN_DEBUG_CMD,
                    [CMD_LOAD, 0x1_0000, name, 0, 0],
                    &mut mem,
                    MASK24,
                    0,
                    0,
                ),
                (0, false)
            );
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let outside = dir.with_extension("outside");
            let _ = std::fs::remove_dir_all(&outside);
            std::fs::create_dir_all(&outside).unwrap();
            std::fs::write(outside.join("secret.bin"), b"secret").unwrap();
            symlink(&outside, root.join("escape")).unwrap();
            put_str(&mut mem, 0x2400, "escape/secret.bin");
            assert_eq!(
                lib.call(
                    FN_DEBUG_CMD,
                    [CMD_LOAD, 0x1_0000, 0x2400, 0, 0],
                    &mut mem,
                    MASK24,
                    0,
                    0,
                ),
                (0, false)
            );
            put(&mut mem, 0x1_1000, b"guest!");
            assert_eq!(
                lib.call(
                    FN_DEBUG_CMD,
                    [CMD_SAVE, 0x1_1000, 6, 0x2400, 0],
                    &mut mem,
                    MASK24,
                    0,
                    0,
                ),
                (0, false)
            );
            assert_eq!(
                std::fs::read(outside.join("secret.bin")).unwrap(),
                b"secret"
            );

            // A dangling leaf is still an existing symlink entry. Creating
            // through it must not materialize its out-of-root target.
            symlink(outside.join("created.bin"), root.join("dangling.bin")).unwrap();
            put_str(&mut mem, 0x2500, "dangling.bin");
            assert_eq!(
                lib.call(
                    FN_DEBUG_CMD,
                    [CMD_SAVE, 0x1_1000, 6, 0x2500, 0],
                    &mut mem,
                    MASK24,
                    0,
                    0,
                ),
                (0, false)
            );
            assert!(!outside.join("created.bin").exists());
            std::fs::remove_dir_all(outside).unwrap();
        }

        let end = mem.chip_ram.len() as u32 - 2;
        put(&mut mem, end, b"xx");
        assert_eq!(
            lib.call(
                FN_DEBUG_CMD,
                [CMD_LOAD, end, 0x2300, 0, 0],
                &mut mem,
                MASK24,
                0,
                0,
            ),
            (0, false)
        );
        assert_eq!(guest_bytes(&mem, end, 2, MASK24).unwrap(), b"xx");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn speculative_file_save_waits_for_the_committed_replay() {
        let dir = std::env::temp_dir().join(format!(
            "copperline-uaelib-files-spec-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let root = std::fs::canonicalize(&dir).unwrap();
        let target = root.join("saved.bin");

        let mut lib = UaeLib::new();
        lib.set_file_root(Some(root)).unwrap();
        let mut mem = memory();
        put_str(&mut mem, 0x2000, "saved.bin");
        put(&mut mem, 0x3000, b"data");
        lib.set_speculative_host_quiet(true);
        lib.call(
            FN_DEBUG_CMD,
            [CMD_SAVE, 0x3000, 4, 0x2000, 0],
            &mut mem,
            MASK24,
            0,
            0,
        );
        assert!(!target.exists());
        lib.set_speculative_host_quiet(false);
        lib.call(
            FN_DEBUG_CMD,
            [CMD_SAVE, 0x3000, 4, 0x2000, 0],
            &mut mem,
            MASK24,
            0,
            0,
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"data");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn file_authority_does_not_travel_in_serialized_guest_state() {
        let dir = std::env::temp_dir().join(format!(
            "copperline-uaelib-authority-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut lib = UaeLib::new();
        lib.set_file_root(Some(std::fs::canonicalize(&dir).unwrap()))
            .unwrap();
        let encoded = bincode::serialize(&lib).unwrap();
        let restored: UaeLib = bincode::deserialize(&encoded).unwrap();
        assert!(restored.file_root().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn overlay_cmd(lib: &mut UaeLib, mem: &mut Memory, cmd: u32, a2: u32, a3: u32, a4: u32) {
        let r = lib.call(FN_DEBUG_CMD, [cmd, a2, a3, a4, 0], mem, MASK24, 0, 0);
        assert_eq!(r, (0, false));
    }

    #[test]
    fn overlay_rect_commands_unpack_packed_corners_and_colour() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        overlay_cmd(
            &mut lib,
            &mut mem,
            CMD_RECT,
            (10 << 16) | 20,
            (110 << 16) | 220,
            0xFFAA_BB01,
        );
        overlay_cmd(
            &mut lib,
            &mut mem,
            CMD_FILLED_RECT,
            0,
            (768 << 16) | 576,
            0x0000_FF00,
        );
        assert_eq!(
            lib.overlay(),
            &[
                OverlayCmd::Rect {
                    l: 10,
                    t: 20,
                    r: 110,
                    b: 220,
                    // The guest's stray alpha byte is dropped.
                    colour: 0x00AA_BB01,
                },
                OverlayCmd::FilledRect {
                    l: 0,
                    t: 0,
                    r: 768,
                    b: 576,
                    colour: 0x0000_FF00,
                },
            ]
        );
    }

    #[test]
    fn overlay_coordinates_clamp_to_the_pal_hires_space() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        // Negative left/top saturate to 0; oversized right/bottom to the
        // space's edge.
        let neg = ((-20i16 as u16 as u32) << 16) | (-4i16 as u16 as u32);
        let big = (4000u32 << 16) | 3000;
        overlay_cmd(&mut lib, &mut mem, CMD_FILLED_RECT, neg, big, 1);
        assert_eq!(
            lib.overlay(),
            &[OverlayCmd::FilledRect {
                l: 0,
                t: 0,
                r: 768,
                b: 576,
                colour: 1,
            }]
        );
        // A rect empty after clamping is skipped.
        overlay_cmd(
            &mut lib,
            &mut mem,
            CMD_RECT,
            (50 << 16) | 50,
            (50 << 16) | 50,
            1,
        );
        overlay_cmd(
            &mut lib,
            &mut mem,
            CMD_RECT,
            (90 << 16) | 50,
            (50 << 16) | 90,
            1,
        );
        assert_eq!(lib.overlay().len(), 1);
    }

    #[test]
    fn overlay_text_reads_the_guest_string_at_call_time() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        put_str(&mut mem, 0x2000, "score 42");
        overlay_cmd(
            &mut lib,
            &mut mem,
            CMD_TEXT,
            (8 << 16) | 16,
            0x2000,
            0x00FF_FFFF,
        );
        // The guest may reuse its buffer the moment the call returns; the
        // stored text stands.
        put_str(&mut mem, 0x2000, "clobbered");
        assert_eq!(
            lib.overlay(),
            &[OverlayCmd::Text {
                l: 8,
                t: 16,
                text: "score 42".to_string(),
                colour: 0x00FF_FFFF,
            }]
        );
        // An unreadable pointer drops the command.
        overlay_cmd(&mut lib, &mut mem, CMD_TEXT, 0, 0x00F0_0000, 0);
        assert_eq!(lib.overlay().len(), 1);
    }

    #[test]
    fn overlay_clear_empties_the_list_and_the_cap_counts_drops() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        for _ in 0..OVERLAY_CAP + 3 {
            overlay_cmd(&mut lib, &mut mem, CMD_FILLED_RECT, 0, (8 << 16) | 8, 2);
        }
        assert_eq!(lib.overlay().len(), OVERLAY_CAP);
        assert_eq!(
            lib.overlay_dropped(),
            3,
            "the new command is the one dropped"
        );
        overlay_cmd(&mut lib, &mut mem, CMD_CLEAR, 0, 0, 0);
        assert!(lib.overlay().is_empty());
        assert_eq!(lib.overlay_dropped(), 0);
    }

    #[test]
    fn overlay_list_goes_with_a_machine_reset() {
        let mut lib = UaeLib::new();
        let mut mem = memory();
        overlay_cmd(&mut lib, &mut mem, CMD_FILLED_RECT, 0, (8 << 16) | 8, 2);
        lib.reset();
        assert!(lib.overlay().is_empty());
    }
}
