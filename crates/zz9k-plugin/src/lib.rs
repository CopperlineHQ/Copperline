// SPDX-License-Identifier: GPL-3.0-or-later

//! Copperline WASM board: a ZZ9000 SDK v2 service platform subset (CORE +
//! MEMORY + CRYPTO services plus DIAG_READ) with the crypto computed by
//! RustCrypto instead of the real card's ARM core. Register-compatible with
//! the SDK's Amiga-side transport (`zz9000-sdk/host/src/zz9k_host.c`), so
//! the unmodified SDK tools and the accelerated AmiSSL build drive this
//! board exactly as they drive real hardware. The protocol contract lives
//! in Copperline's `docs/internals/zz9k.md` -- keep this file in sync with
//! that page, not the other way around.
//!
//! The whole board window (registers, mailbox descriptor + rings, and the
//! shared-buffer heap) is one byte array in linear memory, and every other
//! piece of board state lives in ordinary Rust data (also linear memory),
//! so Copperline's linear-memory save-state snapshots capture the board
//! exactly; the module keeps no state in WebAssembly globals. The board is
//! pure compute -- its only imports are `log`/`config_get`/`resource_*` --
//! which is what keeps it deterministic and replay-safe.
//!
//! Requests are picked up by `tick()` scanning the request ring (the Zorro
//! II transport never rings the doorbell -- the Z3 aperture doorbell write
//! is accepted but the scan does the work), computed immediately, and their
//! completions published after a deterministic latency counted in colour
//! clocks, one request per tick so a single call can never approach the
//! host's per-call fuel budget.
//!
//! `memory` is exported automatically: rustc gives a wasm32-unknown-unknown
//! `cdylib` a `memory` export with no extra code required.

use std::collections::VecDeque;

mod alloc;
mod cryptoimpl;
mod drbg;
mod ops;
#[cfg(test)]
mod rsa_kat;
mod wire;

use crate::alloc::Heap;
use crate::drbg::Drbg;
use crate::wire::*;

// -- Host imports (module "env"), per Copperline's WASM plugin ABI ---------
//
// Only the always-available imports: this board needs no DMA (the guest
// copies payloads through the board window itself, exactly as it does on
// the real card), no network, and no host sockets.

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
extern "C" {
    fn log(ptr: i32, len: i32);
    fn config_get(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32) -> i32;
}

#[cfg(not(target_arch = "wasm32"))]
mod native_host_stubs {
    #![allow(unused_variables, clippy::missing_safety_doc)]
    pub unsafe fn log(ptr: i32, len: i32) {}
    pub unsafe fn config_get(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32) -> i32 {
        -1
    }
}
#[cfg(not(target_arch = "wasm32"))]
use native_host_stubs::*;

fn host_log(msg: &str) {
    // Safety: `msg` is backed by this module's own linear memory, which is
    // exactly what the `log` import expects.
    unsafe { log(msg.as_ptr() as i32, msg.len() as i32) };
}

fn config_get_string(key: &str) -> Option<String> {
    let mut buf = [0u8; 80];
    let n = unsafe {
        config_get(
            key.as_ptr() as i32,
            key.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if n < 0 {
        return None;
    }
    let n = (n as usize).min(buf.len());
    Some(String::from_utf8_lossy(&buf[..n]).into_owned())
}

/// Default board window: 4 MB. On Zorro II this is also the only size the
/// SDK transport accepts HOST_WINDOW allocations for (its "historical fixed
/// 4 MB" profile); on Zorro III it is simply the default.
const DEFAULT_BOARD_SIZE: u32 = 0x0040_0000;

pub struct Board {
    /// The board window: registers, mailbox, shared-buffer heap.
    pub(crate) win: Vec<u8>,
    pub(crate) heap: Heap,
    /// Completions waiting out their deterministic latency, in submission
    /// order (the model is a serial coprocessor: the front entry's
    /// remaining latency counts down first).
    pub(crate) pending: VecDeque<ops::Reply>,
    irq_enabled: bool,
    irq_pending: bool,
    /// Interrupt line selection (the ZZ9000.CFG `int2` key): true = INT2
    /// (PORTS), false = INT6 (EXTER, the hardware default).
    int2_selected: bool,
    pub(crate) requests_completed: u32,
    pub(crate) requests_failed: u32,
    pub(crate) last_status: u32,
    /// Reserved deterministic entropy source; nothing draws from it today
    /// (see drbg.rs).
    #[allow(dead_code)]
    drbg: Drbg,
}

impl Board {
    fn new() -> Self {
        Board {
            win: Vec::new(),
            heap: Heap::new(AMIGA_MEMORY_OFFSET),
            pending: VecDeque::new(),
            irq_enabled: false,
            irq_pending: false,
            int2_selected: false,
            requests_completed: 0,
            requests_failed: 0,
            last_status: 0,
            drbg: Drbg::from_seed_hex(None),
        }
    }

    pub fn init(&mut self) {
        let size = config_get_string("size")
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(DEFAULT_BOARD_SIZE);
        let int2 = config_get_string("int2")
            .map(|s| matches!(s.trim(), "1" | "true" | "yes"))
            .unwrap_or(false);
        let seed = config_get_string("seed");
        self.setup(size, int2, seed.as_deref());
        host_log(&format!(
            "zz9k: SDK v2 crypto board, {} KB window, completion IRQ on INT{}",
            self.win.len() / 1024,
            if self.int2_selected { 2 } else { 6 }
        ));
    }

    /// Build the window image: bootstrap registers and the mailbox
    /// descriptor. Everything else in the window starts as zeroes.
    fn setup(&mut self, size: u32, int2: bool, seed: Option<&str>) {
        // The window must at least reach past the mailbox rings; anything
        // smaller than 64 KB has no Amiga-visible heap at all.
        let size = size.clamp(AMIGA_MEMORY_OFFSET, 0x1000_0000);
        self.win = vec![0u8; size as usize];
        self.heap = Heap::new(size);
        self.pending.clear();
        self.irq_enabled = false;
        self.irq_pending = false;
        self.int2_selected = int2;
        self.requests_completed = 0;
        self.requests_failed = 0;
        self.last_status = 0;
        self.drbg = Drbg::from_seed_hex(seed);

        put_be16(&mut self.win, REG_SDK_MAGIC, SDK_MAGIC_VALUE);
        put_be16(&mut self.win, REG_SDK_VERSION, SDK_VERSION_VALUE);
        put_be16(
            &mut self.win,
            REG_SDK_MAILBOX_HI,
            (MAILBOX_ARM_ADDRESS >> 16) as u16,
        );
        put_be16(
            &mut self.win,
            REG_SDK_MAILBOX_LO,
            (MAILBOX_ARM_ADDRESS & 0xFFFF) as u16,
        );

        let d = MAILBOX_BOARD_OFFSET;
        put_be32(&mut self.win, d + DESC_MAGIC, ABI_MAGIC);
        put_be16(&mut self.win, d + DESC_ABI_MAJOR, ABI_MAJOR);
        put_be16(&mut self.win, d + DESC_ABI_MINOR, ABI_MINOR);
        put_be32(&mut self.win, d + DESC_SIZE, MAILBOX_DESCRIPTOR_SIZE);
        put_be32(&mut self.win, d + DESC_REQ_RING_OFFSET, REQUEST_RING_OFFSET);
        put_be32(&mut self.win, d + DESC_REQ_RING_ENTRIES, RING_ENTRIES);
        put_be32(
            &mut self.win,
            d + DESC_COMPL_RING_OFFSET,
            COMPLETION_RING_OFFSET,
        );
        put_be32(&mut self.win, d + DESC_COMPL_RING_ENTRIES, RING_ENTRIES);
        put_be32(&mut self.win, d + DESC_CAPABILITY_BITS, CAPABILITY_BITS);
    }

    /// Construct a board with explicit parameters, bypassing the host
    /// config -- the constructor native test harnesses use.
    pub fn new_with(size: u32, int2: bool, seed: Option<&str>) -> Self {
        let mut board = Board::new();
        board.setup(size, int2, seed);
        board
    }

    /// Fold the Zorro III register aperture (0x1000..0x1FFF) onto the low
    /// register block it aliases.
    fn fold(off: u32) -> u32 {
        if (Z3_REGISTER_WINDOW_OFFSET..Z3_REGISTER_WINDOW_OFFSET + 0x1000).contains(&off) {
            off - Z3_REGISTER_WINDOW_OFFSET
        } else {
            off
        }
    }

    pub fn read(&self, off: i32, size: i32) -> i32 {
        if self.win.is_empty() {
            return 0;
        }
        let off = Self::fold(off as u32);
        let size = size as u32;
        if !(1..=4).contains(&size) || off.checked_add(size).is_none() {
            return 0;
        }
        if (off + size) as usize > self.win.len() {
            return 0;
        }
        // Big-endian byte composition: read(off, 2) is the 68k word at that
        // address, exactly as the bus delivers it.
        let mut value: u32 = 0;
        for i in 0..size {
            value = (value << 8) | u32::from(self.win[(off + i) as usize]);
        }
        value as i32
    }

    pub fn write(&mut self, off: i32, size: i32, value: i32) {
        if self.win.is_empty() {
            return;
        }
        let off = Self::fold(off as u32);
        match size {
            4 => {
                // A 32-bit store is two 16-bit bus cycles (and a 68000
                // delivers it as two size-2 writes anyway).
                self.write_folded(off, 2, ((value >> 16) & 0xFFFF) as u32);
                self.write_folded(off.wrapping_add(2), 2, (value & 0xFFFF) as u32);
            }
            1 | 2 => self.write_folded(
                off,
                size as u32,
                (value as u32) & if size == 1 { 0xFF } else { 0xFFFF },
            ),
            _ => {}
        }
    }

    fn write_folded(&mut self, off: u32, size: u32, value: u32) {
        if size == 2 {
            match off {
                REG_CONFIG => {
                    if value as u16 & CONFIG_ACK_SDK != 0 {
                        self.set_irq_pending(false);
                    }
                    return;
                }
                REG_CONFIG_KEY => {
                    // Latched key query: store the key's value at CONFIG_KEY
                    // and its presence at CONFIG_PRESENT for the readback.
                    let (present, key_value) = match value as u16 {
                        CFG_KEY_INT2 => (1, u16::from(self.int2_selected)),
                        _ => (0, 0),
                    };
                    put_be16(&mut self.win, REG_CONFIG_KEY, key_value);
                    put_be16(&mut self.win, REG_CONFIG_PRESENT, present);
                    return;
                }
                REG_SDK_DOORBELL => {
                    // Accepted but redundant: tick()'s ring scan is what
                    // picks requests up (the Zorro II transport never rings
                    // the doorbell at all).
                    return;
                }
                REG_SDK_IRQ_CTRL => {
                    match value as u16 {
                        SDK_IRQ_ACK => self.set_irq_pending(false),
                        SDK_IRQ_ENABLE => self.irq_enabled = true,
                        SDK_IRQ_DISABLE => self.irq_enabled = false,
                        _ => {}
                    }
                    return;
                }
                _ => {}
            }
        }
        // Reserved registers and the rest of the low block ignore writes;
        // the mapped-IO window (mailbox descriptor + rings) and the shared
        // heap are plain memory.
        if off < MAPPED_IO_BOARD_OFFSET {
            return;
        }
        if (off + size) as usize > self.win.len() {
            return;
        }
        for i in 0..size {
            self.win[(off + i) as usize] = (value >> (8 * (size - 1 - i))) as u8;
        }
    }

    fn set_irq_pending(&mut self, pending: bool) {
        self.irq_pending = pending;
        let status = if pending { INTERRUPT_SDK } else { 0 };
        put_be16(&mut self.win, REG_CONFIG, status);
    }

    pub fn tick(&mut self, cck: i32) {
        if self.win.is_empty() {
            return;
        }
        // Age the front pending completion (serial-coprocessor model) and
        // publish everything that comes due, letting leftover budget flow
        // to the next entry.
        let mut budget = i64::from(cck.max(0));
        while budget > 0 {
            let Some(front) = self.pending.front_mut() else {
                break;
            };
            if front.latency_cck > budget {
                front.latency_cck -= budget;
                break;
            }
            budget -= front.latency_cck;
            front.latency_cck = 0;
            if !self.publish_front() {
                // Completion ring full: hold until the guest consumes.
                break;
            }
        }
        // Consume at most one new request per tick: each request is
        // computed inside this call, and one op per call stays far inside
        // the host's per-call fuel budget.
        self.consume_one_request();
    }

    fn publish_front(&mut self) -> bool {
        let d = MAILBOX_BOARD_OFFSET;
        let head = get_be32(&self.win, d + DESC_COMPL_HEAD);
        let tail = get_be32(&self.win, d + DESC_COMPL_TAIL);
        if head >= RING_ENTRIES || tail >= RING_ENTRIES {
            // The guest scribbled over the ring indices; drop the
            // completion rather than write through a bogus index.
            self.pending.pop_front();
            return true;
        }
        let next = (tail + 1) % RING_ENTRIES;
        if next == head {
            return false;
        }
        let reply = self
            .pending
            .pop_front()
            .expect("publish_front with empty queue");
        let entry_off = (d + COMPLETION_RING_OFFSET + tail * MAILBOX_ENTRY_SIZE) as usize;
        self.win[entry_off..entry_off + MAILBOX_ENTRY_SIZE as usize].copy_from_slice(&reply.entry);
        put_be32(&mut self.win, d + DESC_COMPL_TAIL, next);
        if self.irq_enabled {
            self.set_irq_pending(true);
        }
        true
    }

    fn consume_one_request(&mut self) {
        // Backpressure: while the guest leaves the completion ring full,
        // stop consuming requests once a ring's worth of completions is
        // already waiting -- the request ring then fills and the guest
        // sees BUSY at submit, instead of `pending` (and linear memory)
        // growing without bound.
        if self.pending.len() >= RING_ENTRIES as usize {
            return;
        }
        let d = MAILBOX_BOARD_OFFSET;
        let head = get_be32(&self.win, d + DESC_REQ_HEAD);
        let tail = get_be32(&self.win, d + DESC_REQ_TAIL);
        if head >= RING_ENTRIES || tail >= RING_ENTRIES || head == tail {
            return;
        }
        let entry_off = (d + REQUEST_RING_OFFSET + head * MAILBOX_ENTRY_SIZE) as usize;
        let mut entry = [0u8; MAILBOX_ENTRY_SIZE as usize];
        entry.copy_from_slice(&self.win[entry_off..entry_off + MAILBOX_ENTRY_SIZE as usize]);
        put_be32(&mut self.win, d + DESC_REQ_HEAD, (head + 1) % RING_ENTRIES);
        let reply = ops::process(self, &entry);
        self.pending.push_back(reply);
    }

    pub fn int2_line(&self) -> i32 {
        i32::from(self.irq_pending && self.irq_enabled && self.int2_selected)
    }

    pub fn int6_line(&self) -> i32 {
        i32::from(self.irq_pending && self.irq_enabled && !self.int2_selected)
    }
}

thread_local! {
    static BOARD: std::cell::RefCell<Board> = std::cell::RefCell::new(Board::new());
}

// -- Copperline WASM board ABI ---------------------------------------------
//
// #[cfg(target_arch = "wasm32")] on every export: `read`/`write` are also
// libc symbol names, and exporting them unconditionally would shadow libc
// in a native test binary (see crates/hostsocket-plugin for the war
// story).

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn init() {
    BOARD.with(|b| b.borrow_mut().init());
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn read(off: i32, size: i32) -> i32 {
    BOARD.with(|b| b.borrow().read(off, size))
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn write(off: i32, size: i32, value: i32) {
    BOARD.with(|b| b.borrow_mut().write(off, size, value));
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn tick(cck: i32) {
    BOARD.with(|b| b.borrow_mut().tick(cck));
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn int2() -> i32 {
    BOARD.with(|b| b.borrow().int2_line())
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn int6() -> i32 {
    BOARD.with(|b| b.borrow().int6_line())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    // -- A minimal native reimplementation of the SDK transport, driving
    // -- the board purely through its public read/write/tick surface with
    // -- 16-bit accesses, the way the 68k does.

    fn mk() -> Board {
        Board::new_with(DEFAULT_BOARD_SIZE, false, None)
    }

    fn r16(b: &Board, off: u32) -> u16 {
        b.read(off as i32, 2) as u16
    }

    fn r32(b: &Board, off: u32) -> u32 {
        ((b.read(off as i32, 2) as u32) << 16) | (b.read(off as i32 + 2, 2) as u32 & 0xFFFF)
    }

    fn w16(b: &mut Board, off: u32, value: u16) {
        b.write(off as i32, 2, i32::from(value));
    }

    fn w32(b: &mut Board, off: u32, value: u32) {
        w16(b, off, (value >> 16) as u16);
        w16(b, off + 2, (value & 0xFFFF) as u16);
    }

    fn wbytes(b: &mut Board, off: u32, data: &[u8]) {
        // 16-bit stores with a trailing byte, like zz9k_copy_payload_to_wire.
        let mut i = 0;
        while i + 1 < data.len() {
            w16(
                b,
                off + i as u32,
                u16::from_be_bytes([data[i], data[i + 1]]),
            );
            i += 2;
        }
        if i < data.len() {
            b.write((off + i as u32) as i32, 1, i32::from(data[i]));
        }
    }

    fn rbytes(b: &Board, off: u32, len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| b.read((off + i as u32) as i32, 1) as u8)
            .collect()
    }

    const DESC: u32 = MAILBOX_BOARD_OFFSET;

    fn submit(b: &mut Board, request_id: u32, opcode: u16, payload: &[u8]) {
        assert!(payload.len() <= 48);
        let head = r32(b, DESC + DESC_REQ_HEAD);
        let tail = r32(b, DESC + DESC_REQ_TAIL);
        assert_ne!((tail + 1) % RING_ENTRIES, head, "request ring full");
        let entry = DESC + REQUEST_RING_OFFSET + tail * MAILBOX_ENTRY_SIZE;
        w32(b, entry + ENTRY_REQUEST_ID, request_id);
        w16(b, entry + ENTRY_OPCODE, opcode);
        w16(b, entry + ENTRY_STATUS, STATUS_QUEUED);
        w16(b, entry + ENTRY_FLAGS, ENTRY_FLAG_INLINE_PAYLOAD);
        w16(b, entry + ENTRY_PAYLOAD_LEN, payload.len() as u16);
        w32(b, entry + ENTRY_USER_COOKIE, request_id ^ 0x5A5A_5A5A);
        wbytes(b, entry + ENTRY_PAYLOAD, payload);
        w32(b, DESC + DESC_REQ_TAIL, (tail + 1) % RING_ENTRIES);
        // The Z3 doorbell; harmless, and Zorro II never writes it.
        w16(b, Z3_REGISTER_WINDOW_OFFSET + REG_SDK_DOORBELL, 1);
    }

    struct Completion {
        request_id: u32,
        opcode: u16,
        status: u16,
        payload_len: u16,
        user_cookie: u32,
        payload: Vec<u8>,
    }

    fn poll(b: &mut Board) -> Option<Completion> {
        let head = r32(b, DESC + DESC_COMPL_HEAD);
        let tail = r32(b, DESC + DESC_COMPL_TAIL);
        if head == tail {
            return None;
        }
        let entry = DESC + COMPLETION_RING_OFFSET + head * MAILBOX_ENTRY_SIZE;
        let raw = rbytes(b, entry, 64);
        w32(b, DESC + DESC_COMPL_HEAD, (head + 1) % RING_ENTRIES);
        Some(Completion {
            request_id: get_be32(&raw, ENTRY_REQUEST_ID),
            opcode: get_be16(&raw, ENTRY_OPCODE),
            status: get_be16(&raw, ENTRY_STATUS),
            payload_len: get_be16(&raw, ENTRY_PAYLOAD_LEN),
            user_cookie: get_be32(&raw, ENTRY_USER_COOKIE),
            payload: raw[ENTRY_PAYLOAD as usize..].to_vec(),
        })
    }

    fn call(b: &mut Board, request_id: u32, opcode: u16, payload: &[u8]) -> Completion {
        submit(b, request_id, opcode, payload);
        for _ in 0..100_000 {
            b.tick(20);
            if let Some(c) = poll(b) {
                assert_eq!(c.request_id, request_id);
                assert_eq!(c.opcode, opcode);
                assert_eq!(c.user_cookie, request_id ^ 0x5A5A_5A5A);
                return c;
            }
        }
        panic!("no completion for opcode {opcode:#x}");
    }

    fn alloc(b: &mut Board, id: u32, len: u32) -> (u32, u32) {
        let mut payload = [0u8; 12];
        put_be32(&mut payload, 0, len);
        put_be32(&mut payload, 4, 16);
        put_be32(&mut payload, 8, ALLOC_HOST_WINDOW);
        let c = call(b, id, OP_ALLOC_SHARED, &payload);
        assert_eq!(c.status, STATUS_OK);
        assert!(c.payload_len >= 16);
        let handle = get_be32(&c.payload, 0);
        let arm_addr = get_be32(&c.payload, 4);
        assert_ne!(handle, 0);
        assert_ne!(handle, INVALID_HANDLE);
        assert!(get_be32(&c.payload, 8) >= len);
        // The window offset the 68k would poke data at.
        let win_off = AMIGA_MEMORY_OFFSET + (arm_addr - ARM_MEMORY_START);
        (handle, win_off)
    }

    #[test]
    fn bootstrap_registers_and_descriptor() {
        let b = mk();
        assert_eq!(r16(&b, REG_SDK_MAGIC), SDK_MAGIC_VALUE);
        assert_eq!(r16(&b, REG_SDK_VERSION), 0x0203);
        let mailbox =
            (u32::from(r16(&b, REG_SDK_MAILBOX_HI)) << 16) | u32::from(r16(&b, REG_SDK_MAILBOX_LO));
        assert_eq!(mailbox, 0x3FE4_3000);
        // The Z3 register aperture aliases the low block for reads too.
        assert_eq!(
            r16(&b, Z3_REGISTER_WINDOW_OFFSET + REG_SDK_MAGIC),
            SDK_MAGIC_VALUE
        );
        // Descriptor, as the transport's attach reads it.
        assert_eq!(r32(&b, DESC + DESC_MAGIC), ABI_MAGIC);
        assert_eq!(r16(&b, DESC + DESC_ABI_MAJOR), 2);
        assert_eq!(r32(&b, DESC + DESC_REQ_RING_ENTRIES), RING_ENTRIES);
        assert_eq!(r32(&b, DESC + DESC_COMPL_RING_ENTRIES), RING_ENTRIES);
        let caps = r32(&b, DESC + DESC_CAPABILITY_BITS);
        assert_eq!(caps, CAPABILITY_BITS);
        assert_eq!(caps & (1 << 24), 0, "APERTURE_LAYOUT must stay clear");
        // Unmapped low registers read zero (P96 legacy probes must not see
        // garbage), and a 32-bit read composes big-endian.
        assert_eq!(r16(&b, 0x0010), 0);
        assert_eq!(b.read(REG_SDK_MAGIC as i32, 4), 0x5A39_0203_u32 as i32);
    }

    #[test]
    fn ping_echoes_and_matches_cookie() {
        let mut b = mk();
        let c = call(&mut b, 7, OP_PING, b"zz9k");
        assert_eq!(c.status, STATUS_OK);
        assert_eq!(c.payload_len, 4);
        assert_eq!(&c.payload[..4], b"zz9k");
    }

    #[test]
    fn query_caps_and_services() {
        let mut b = mk();
        let c = call(&mut b, 1, OP_QUERY_CAPS, &[]);
        assert_eq!(c.status, STATUS_OK);
        assert!(c.payload_len >= 40);
        assert_eq!(get_be32(&c.payload, 0), ABI_MAGIC);
        assert_eq!(get_be16(&c.payload, 4), 2);
        assert_eq!(get_be32(&c.payload, 8), CAPABILITY_BITS);
        assert_eq!(get_be32(&c.payload, 12), 48);
        assert_eq!(get_be32(&c.payload, 16), MAX_SHARED_BUFFERS);
        assert_eq!(get_be32(&c.payload, 28), RING_ENTRIES);

        let mut q = [0u8; 4];
        put_be32(&mut q, 0, u32::from(SERVICE_CRYPTO));
        let c = call(&mut b, 2, OP_QUERY_SERVICE, &q);
        assert_eq!(c.status, STATUS_OK);
        assert_eq!(get_be32(&c.payload, 0), u32::from(SERVICE_CRYPTO));
        assert_eq!(get_be32(&c.payload, 12), CRYPTO_SERVICE_FLAGS);
        assert_eq!(&c.payload[28..34], b"crypto");

        put_be32(&mut q, 0, 0x0200); // SURFACE: not offered
        let c = call(&mut b, 3, OP_QUERY_SERVICE, &q);
        assert_eq!(c.status, STATUS_NOT_FOUND);
        assert_eq!(c.payload_len, 0);

        let c = call(&mut b, 4, OP_CANCEL, &[]);
        assert_eq!(c.status, STATUS_NOT_FOUND);
        let c = call(&mut b, 5, OP_QUERY_APERTURE_LAYOUT, &[]);
        assert_eq!(c.status, STATUS_UNSUPPORTED);
        // An opcode from a service this board does not implement.
        let c = call(&mut b, 6, 0x0200, &[]);
        assert_eq!(c.status, STATUS_UNSUPPORTED);
    }

    #[test]
    fn alloc_free_and_memory_ops() {
        let mut b = mk();
        let (h1, off1) = alloc(&mut b, 10, 64);
        let (h2, _off2) = alloc(&mut b, 11, 64);

        // MEM_FILL h1 with 0xAB, then MEM_COPY h1 -> h2 and read back
        // through the window.
        let mut fill = [0u8; 13];
        put_be32(&mut fill, 0, h1);
        put_be32(&mut fill, 4, 0);
        put_be32(&mut fill, 8, 64);
        fill[12] = 0xAB;
        assert_eq!(call(&mut b, 12, OP_MEM_FILL, &fill).status, STATUS_OK);

        let mut copy = [0u8; 24];
        put_be32(&mut copy, 0, h2);
        put_be32(&mut copy, 4, 0);
        put_be32(&mut copy, 8, h1);
        put_be32(&mut copy, 12, 0);
        put_be32(&mut copy, 16, 64);
        assert_eq!(call(&mut b, 13, OP_MEM_COPY, &copy).status, STATUS_OK);
        assert_eq!(rbytes(&b, off1, 4), vec![0xAB; 4]);

        // Free h1 twice: second time is a stale handle.
        let mut free = [0u8; 4];
        put_be32(&mut free, 0, h1);
        assert_eq!(call(&mut b, 14, OP_FREE_SHARED, &free).status, STATUS_OK);
        assert_eq!(
            call(&mut b, 15, OP_FREE_SHARED, &free).status,
            STATUS_BAD_HANDLE
        );
        // And an op against the stale handle fails without touching h2.
        put_be32(&mut fill, 0, h1);
        assert_eq!(
            call(&mut b, 16, OP_MEM_FILL, &fill).status,
            STATUS_BAD_HANDLE
        );
        put_be32(&mut free, 0, h2);
        assert_eq!(call(&mut b, 17, OP_FREE_SHARED, &free).status, STATUS_OK);
    }

    fn hash_payload(
        alg: u32,
        flags: u32,
        src: (u32, u32, u32),
        dst: (u32, u32),
        key: (u32, u32, u32),
    ) -> [u8; 40] {
        let mut p = [0u8; 40];
        put_be32(&mut p, 0, src.0);
        put_be32(&mut p, 4, src.1);
        put_be32(&mut p, 8, src.2);
        put_be32(&mut p, 12, dst.0);
        put_be32(&mut p, 16, dst.1);
        put_be32(&mut p, 20, key.0);
        put_be32(&mut p, 24, key.1);
        put_be32(&mut p, 28, key.2);
        put_be32(&mut p, 32, alg);
        put_be32(&mut p, 36, flags);
        p
    }

    #[test]
    fn sha256_abc_through_the_full_mailbox() {
        let mut b = mk();
        let (src, src_off) = alloc(&mut b, 20, 64);
        let (dst, dst_off) = alloc(&mut b, 21, 64);
        wbytes(&mut b, src_off, b"abc");
        let p = hash_payload(
            HASH_SHA256,
            0,
            (src, 0, 3),
            (dst, 0),
            (INVALID_HANDLE, 0, 0),
        );
        let c = call(&mut b, 22, OP_CRYPTO_HASH, &p);
        assert_eq!(c.status, STATUS_OK);
        assert!(c.payload_len >= 48);
        assert_eq!(get_be32(&c.payload, 0), 32); // bytes_written
        assert_eq!(get_be32(&c.payload, 4), HASH_SHA256);
        assert_eq!(get_be32(&c.payload, 8), 0);
        let digest = rbytes(&b, dst_off, 32);
        assert_eq!(digest, sha2::Sha256::digest(b"abc").to_vec());
        // Out-of-bounds source range: BAD_HANDLE, not a crash.
        let p = hash_payload(
            HASH_SHA256,
            0,
            (src, 60, 10),
            (dst, 0),
            (INVALID_HANDLE, 0, 0),
        );
        assert_eq!(
            call(&mut b, 23, OP_CRYPTO_HASH, &p).status,
            STATUS_BAD_HANDLE
        );
        // Unknown algorithm: UNSUPPORTED.
        let p = hash_payload(99, 0, (src, 0, 3), (dst, 0), (INVALID_HANDLE, 0, 0));
        assert_eq!(
            call(&mut b, 24, OP_CRYPTO_HASH, &p).status,
            STATUS_UNSUPPORTED
        );
    }

    #[test]
    fn aead_round_trip_and_tag_failure_statuses() {
        let mut b = mk();
        let (key, key_off) = alloc(&mut b, 30, 32);
        let (nonce, nonce_off) = alloc(&mut b, 31, 16);
        let (src, src_off) = alloc(&mut b, 32, 64);
        let (dst, dst_off) = alloc(&mut b, 33, 64);
        wbytes(&mut b, key_off, &[7u8; 32]);
        wbytes(&mut b, nonce_off, &[9u8; 12]);
        wbytes(&mut b, src_off, b"secret message");

        let mut p = [0u8; 48];
        put_be32(&mut p, 0, src);
        put_be32(&mut p, 4, 0);
        put_be32(&mut p, 8, 14);
        put_be32(&mut p, 12, dst);
        put_be32(&mut p, 16, 0);
        put_be32(&mut p, 20, INVALID_HANDLE); // no AAD
        put_be32(&mut p, 28, 0);
        put_be32(&mut p, 32, key);
        put_be32(&mut p, 36, 0);
        put_be32(&mut p, 40, nonce);
        put_be32(&mut p, 44, 0); // flags: encrypt, legacy algorithm 0
        let c = call(&mut b, 34, OP_CRYPTO_AEAD, &p);
        assert_eq!(c.status, STATUS_OK);
        assert_eq!(get_be32(&c.payload, 0), 14 + 16);
        // Legacy algorithm 0 resolves to ChaCha20-Poly1305 in the result...
        assert_eq!(get_be32(&c.payload, 4), AEAD_CHACHA20_POLY1305);
        // ...and the result flags echo only the DECRYPT bit.
        assert_eq!(get_be32(&c.payload, 8), 0);

        // Decrypt it back (ciphertext||tag is at dst); write plaintext over src.
        let mut d = p;
        put_be32(&mut d, 0, dst);
        put_be32(&mut d, 8, 14);
        put_be32(&mut d, 12, src);
        put_be32(&mut d, 44, AEAD_FLAG_DECRYPT);
        let c = call(&mut b, 35, OP_CRYPTO_AEAD, &d);
        assert_eq!(c.status, STATUS_OK);
        assert_eq!(get_be32(&c.payload, 0), 14);
        assert_eq!(get_be32(&c.payload, 8), AEAD_FLAG_DECRYPT);
        assert_eq!(rbytes(&b, src_off, 14), b"secret message");

        // Corrupt the tag: decrypt reports IO_ERROR.
        let tag_byte = rbytes(&b, dst_off + 14, 1)[0];
        b.write((dst_off + 14) as i32, 1, i32::from(tag_byte ^ 1));
        let c = call(&mut b, 36, OP_CRYPTO_AEAD, &d);
        assert_eq!(c.status, STATUS_IO_ERROR);
        assert_eq!(c.payload_len, 0);
    }

    #[test]
    fn x25519_and_p256_keygen_through_the_mailbox() {
        let mut b = mk();
        let (scalar, scalar_off) = alloc(&mut b, 40, 32);
        let (point, point_off) = alloc(&mut b, 41, 80);
        let (dst, dst_off) = alloc(&mut b, 42, 80);
        // RFC 7748 test vector 1.
        let k: Vec<u8> = (0..32)
            .map(|i| {
                [
                    0xa5u8, 0x46, 0xe3, 0x6b, 0xf0, 0x52, 0x7c, 0x9d, 0x3b, 0x16, 0x15, 0x4b, 0x82,
                    0x46, 0x5e, 0xdd, 0x62, 0x14, 0x4c, 0x0a, 0xc1, 0xfc, 0x5a, 0x18, 0x50, 0x6a,
                    0x22, 0x44, 0xba, 0x44, 0x9a, 0xc4,
                ][i]
            })
            .collect();
        let u: Vec<u8> = (0..32)
            .map(|i| {
                [
                    0xe6u8, 0xdb, 0x68, 0x67, 0x58, 0x30, 0x30, 0xdb, 0x35, 0x94, 0xc1, 0xa4, 0x24,
                    0xb1, 0x5f, 0x7c, 0x72, 0x66, 0x24, 0xec, 0x26, 0xb3, 0x35, 0x3b, 0x10, 0xa9,
                    0x03, 0xa6, 0xd0, 0xab, 0x1c, 0x4c,
                ][i]
            })
            .collect();
        wbytes(&mut b, scalar_off, &k);
        wbytes(&mut b, point_off, &u);
        let mut p = [0u8; 32];
        put_be32(&mut p, 0, scalar);
        put_be32(&mut p, 8, point);
        put_be32(&mut p, 16, dst);
        put_be32(&mut p, 24, KX_X25519);
        put_be32(&mut p, 28, 0);
        let c = call(&mut b, 43, OP_CRYPTO_KX, &p);
        assert_eq!(c.status, STATUS_OK);
        assert_eq!(get_be32(&c.payload, 0), 32);
        assert_eq!(
            rbytes(&b, dst_off, 4),
            vec![0xc3, 0xda, 0x55, 0x37],
            "RFC 7748 shared secret head"
        );

        // P-256 keygen: point handle invalid by design.
        put_be32(&mut p, 8, INVALID_HANDLE);
        put_be32(&mut p, 24, KX_P256);
        put_be32(&mut p, 28, KX_FLAG_KEYGEN);
        let c = call(&mut b, 44, OP_CRYPTO_KX, &p);
        assert_eq!(c.status, STATUS_OK);
        assert_eq!(get_be32(&c.payload, 0), P256_POINT_BYTES);
        assert_eq!(
            rbytes(&b, dst_off, 1),
            vec![0x04],
            "uncompressed SEC1 point"
        );

        // X25519 with a nonzero flags word is UNSUPPORTED (KEYGEN is
        // P-256-only, matching the firmware).
        put_be32(&mut p, 8, point);
        put_be32(&mut p, 24, KX_X25519);
        put_be32(&mut p, 28, KX_FLAG_KEYGEN);
        assert_eq!(
            call(&mut b, 45, OP_CRYPTO_KX, &p).status,
            STATUS_UNSUPPORTED
        );
    }

    #[test]
    fn verify_reports_valid_flag_not_error() {
        let mut b = mk();
        let (hash_buf, hash_off) = alloc(&mut b, 50, 32);
        let (sig_buf, sig_off) = alloc(&mut b, 51, 512);
        let (key_buf, key_off) = alloc(&mut b, 52, 512);
        let digest = sha2::Sha256::digest(crate::rsa_kat::KAT_MSG);
        wbytes(&mut b, hash_off, &digest);
        wbytes(&mut b, sig_off, &crate::rsa_kat::KAT_RSA_SIG_PKCS1);
        let mut key = crate::rsa_kat::KAT_RSA_N.to_vec();
        key.extend_from_slice(&crate::rsa_kat::KAT_RSA_E.to_be_bytes());
        wbytes(&mut b, key_off, &key);

        let mut p = [0u8; 40];
        put_be32(&mut p, 0, VERIFY_RSA_PKCS1_2048_SHA256);
        put_be32(&mut p, 4, hash_buf);
        put_be32(&mut p, 8, 0);
        put_be32(&mut p, 12, 32);
        put_be32(&mut p, 16, sig_buf);
        put_be32(&mut p, 20, 0);
        put_be32(&mut p, 24, 256);
        put_be32(&mut p, 28, key_buf);
        put_be32(&mut p, 32, 0);
        put_be32(&mut p, 36, 260);
        let c = call(&mut b, 53, OP_CRYPTO_VERIFY, &p);
        assert_eq!(c.status, STATUS_OK);
        assert!(c.payload_len >= 48);
        assert_eq!(get_be32(&c.payload, 0), 1, "valid");

        // Corrupt the signature in place: still status OK, valid = 0.
        let sig0 = rbytes(&b, sig_off, 1)[0];
        b.write(sig_off as i32, 1, i32::from(sig0 ^ 1));
        let c = call(&mut b, 54, OP_CRYPTO_VERIFY, &p);
        assert_eq!(c.status, STATUS_OK);
        assert_eq!(get_be32(&c.payload, 0), 0, "invalid");
    }

    #[test]
    fn irq_protocol_int6_default_and_int2_selected() {
        for int2 in [false, true] {
            let mut b = Board::new_with(DEFAULT_BOARD_SIZE, int2, None);
            // The ZZ9000.CFG key group: key 5 present, value = the config.
            w16(&mut b, REG_CONFIG_KEY, CFG_KEY_INT2);
            assert!(r16(&b, REG_CONFIG_PRESENT) != 0);
            assert_eq!(r16(&b, REG_CONFIG_KEY) != 0, int2);
            // Unknown keys read absent.
            w16(&mut b, REG_CONFIG_KEY, 99);
            assert_eq!(r16(&b, REG_CONFIG_PRESENT), 0);

            // Stale ack then enable, like zz9k-irqtest.
            w16(&mut b, REG_CONFIG, 0x0088);
            w16(&mut b, REG_SDK_IRQ_CTRL, SDK_IRQ_ENABLE);
            submit(&mut b, 60, OP_PING, &[]);
            for _ in 0..1000 {
                b.tick(20);
            }
            // Completion published: line asserted on the selected line only.
            assert_eq!(b.int2_line() != 0, int2);
            assert_eq!(b.int6_line() != 0, !int2);
            assert_eq!(r16(&b, REG_CONFIG) & INTERRUPT_SDK, INTERRUPT_SDK);
            // ISR ack via CONFIG.
            w16(&mut b, REG_CONFIG, 0x0088);
            assert_eq!(b.int2_line() | b.int6_line(), 0);
            assert_eq!(r16(&b, REG_CONFIG) & INTERRUPT_SDK, 0);
            assert!(poll(&mut b).is_some());
            // Next completion re-asserts; the 0x010C ack form also clears.
            submit(&mut b, 61, OP_PING, &[]);
            for _ in 0..1000 {
                b.tick(20);
            }
            assert_eq!(b.int2_line() | b.int6_line(), 1);
            w16(&mut b, REG_SDK_IRQ_CTRL, SDK_IRQ_ACK);
            assert_eq!(b.int2_line() | b.int6_line(), 0);
            // Disable: further completions raise no line.
            w16(&mut b, REG_SDK_IRQ_CTRL, SDK_IRQ_DISABLE);
            assert!(poll(&mut b).is_some());
            submit(&mut b, 62, OP_PING, &[]);
            for _ in 0..1000 {
                b.tick(20);
            }
            assert_eq!(b.int2_line() | b.int6_line(), 0);
            assert!(poll(&mut b).is_some());
        }
    }

    #[test]
    fn batch_of_forty_completes_in_order_through_a_full_ring() {
        let mut b = mk();
        // More requests than the completion ring holds; consume lazily.
        let mut submitted = 0u32;
        let mut consumed = 0u32;
        while consumed < 40 {
            while submitted < 40 {
                let head = r32(&b, DESC + DESC_REQ_HEAD);
                let tail = r32(&b, DESC + DESC_REQ_TAIL);
                if (tail + 1) % RING_ENTRIES == head {
                    break; // request ring full; drain some first
                }
                submit(&mut b, 100 + submitted, OP_PING, &submitted.to_be_bytes());
                submitted += 1;
            }
            b.tick(2000);
            while let Some(c) = poll(&mut b) {
                assert_eq!(c.request_id, 100 + consumed, "in-order completion");
                assert_eq!(c.status, STATUS_OK);
                assert_eq!(&c.payload[..4], &consumed.to_be_bytes());
                consumed += 1;
            }
        }
        // DIAG_READ agrees on the counts (40 pings + this DIAG's own
        // predecessors: completed counts the pings and the allocs of other
        // tests do not exist here).
        let c = call(&mut b, 999, OP_DIAG_READ, &[]);
        assert_eq!(c.status, STATUS_OK);
        assert_eq!(get_be32(&c.payload, 0), 40);
        assert_eq!(get_be32(&c.payload, 4), 0);
        assert_eq!(get_be32(&c.payload, 32), MAILBOX_ARM_ADDRESS);
        assert_eq!(get_be32(&c.payload, 36), RING_ENTRIES);
    }

    #[test]
    fn deterministic_latency_and_identical_runs() {
        // The same submission timeline on two boards produces bit-identical
        // windows and identical completion timing.
        let run = || {
            let mut b = mk();
            let (src, src_off) = alloc(&mut b, 1, 64);
            let (dst, _dst_off) = alloc(&mut b, 2, 64);
            wbytes(&mut b, src_off, b"determinism");
            let p = hash_payload(
                HASH_SHA512,
                0,
                (src, 0, 11),
                (dst, 0),
                (INVALID_HANDLE, 0, 0),
            );
            submit(&mut b, 3, OP_CRYPTO_HASH, &p);
            let mut ticks = 0u32;
            loop {
                b.tick(20);
                ticks += 1;
                if let Some(c) = poll(&mut b) {
                    assert_eq!(c.status, STATUS_OK);
                    break;
                }
                assert!(ticks < 10_000);
            }
            (ticks, b.win)
        };
        let (ticks_a, win_a) = run();
        let (ticks_b, win_b) = run();
        assert_eq!(ticks_a, ticks_b);
        assert_eq!(win_a, win_b);
        // A hash op takes its deterministic latency: more than an admin op.
        assert!(ticks_a > 709 / 20, "latency model applied: {ticks_a} ticks");
    }

    #[test]
    fn hmac_and_poly1305_through_the_mailbox() {
        let mut b = mk();
        let (src, src_off) = alloc(&mut b, 70, 64);
        let (dst, dst_off) = alloc(&mut b, 71, 64);
        let (key, key_off) = alloc(&mut b, 72, 64);
        wbytes(&mut b, src_off, b"what do ya want for nothing?");
        wbytes(&mut b, key_off, b"Jefe");
        let p = hash_payload(
            HASH_SHA256,
            HASH_FLAG_HMAC,
            (src, 0, 28),
            (dst, 0),
            (key, 0, 4),
        );
        let c = call(&mut b, 73, OP_CRYPTO_HASH, &p);
        assert_eq!(c.status, STATUS_OK);
        assert_eq!(rbytes(&b, dst_off, 4), vec![0x5B, 0xDC, 0xC1, 0x46]);

        // Poly1305 needs a 32-byte key and rides algorithm 6.
        let poly_key: Vec<u8> = [
            0x85, 0xD6, 0xBE, 0x78, 0x57, 0x55, 0x6D, 0x33, 0x7F, 0x44, 0x52, 0xFE, 0x42, 0xD5,
            0x06, 0xA8, 0x01, 0x03, 0x80, 0x8A, 0xFB, 0x0D, 0xB2, 0xFD, 0x4A, 0xBF, 0xF6, 0xAF,
            0x41, 0x49, 0xF5, 0x1B,
        ]
        .to_vec();
        wbytes(&mut b, key_off, &poly_key);
        wbytes(&mut b, src_off, b"Cryptographic Forum Research Group");
        let p = hash_payload(HASH_POLY1305, 0, (src, 0, 34), (dst, 0), (key, 0, 32));
        let c = call(&mut b, 74, OP_CRYPTO_HASH, &p);
        assert_eq!(c.status, STATUS_OK);
        assert_eq!(get_be32(&c.payload, 0), 16);
        assert_eq!(rbytes(&b, dst_off, 4), vec![0xA8, 0x06, 0x1D, 0xC1]);
    }

    #[test]
    fn oversized_operations_report_bad_request_not_a_trap() {
        // Every variable-length input is capped at MAX_OP_BYTES: the work
        // runs synchronously inside one host call with a finite fuel
        // budget, so an uncapped length would fault the whole board.
        let mut b = Board::new_with(0x0080_0000, false, None);
        let (big, _) = alloc(&mut b, 1, 512 * 1024);
        let (dst, _) = alloc(&mut b, 2, 64);
        let over = (1 << 18) + 1;
        let p = hash_payload(
            HASH_SHA256,
            0,
            (big, 0, over),
            (dst, 0),
            (INVALID_HANDLE, 0, 0),
        );
        assert_eq!(
            call(&mut b, 3, OP_CRYPTO_HASH, &p).status,
            STATUS_BAD_REQUEST
        );
        // ... while the largest allowed length works.
        let p = hash_payload(
            HASH_SHA256,
            0,
            (big, 0, 1 << 18),
            (dst, 0),
            (INVALID_HANDLE, 0, 0),
        );
        assert_eq!(call(&mut b, 4, OP_CRYPTO_HASH, &p).status, STATUS_OK);
        // MEM_FILL and MEM_COPY take the same cap.
        let mut fill = [0u8; 13];
        put_be32(&mut fill, 0, big);
        put_be32(&mut fill, 8, over);
        assert_eq!(
            call(&mut b, 5, OP_MEM_FILL, &fill).status,
            STATUS_BAD_REQUEST
        );
        let mut copy = [0u8; 20];
        put_be32(&mut copy, 0, big);
        put_be32(&mut copy, 4, over);
        put_be32(&mut copy, 8, big);
        put_be32(&mut copy, 16, over);
        assert_eq!(
            call(&mut b, 6, OP_MEM_COPY, &copy).status,
            STATUS_BAD_REQUEST
        );
    }

    #[test]
    fn unknown_flag_bits_are_rejected() {
        let mut b = mk();
        let (src, _) = alloc(&mut b, 1, 64);
        let (dst, _) = alloc(&mut b, 2, 64);
        let (key, key_off) = alloc(&mut b, 3, 64);
        wbytes(&mut b, key_off, &[0u8; 32]);
        // HASH: an undefined flag bit is UNSUPPORTED, and Poly1305 with
        // the HMAC bit set is UNSUPPORTED too (its contract requires 0).
        let p = hash_payload(
            HASH_SHA256,
            0x2,
            (src, 0, 3),
            (dst, 0),
            (INVALID_HANDLE, 0, 0),
        );
        assert_eq!(
            call(&mut b, 4, OP_CRYPTO_HASH, &p).status,
            STATUS_UNSUPPORTED
        );
        let p = hash_payload(HASH_POLY1305, 0x2, (src, 0, 3), (dst, 0), (key, 0, 32));
        assert_eq!(
            call(&mut b, 5, OP_CRYPTO_HASH, &p).status,
            STATUS_UNSUPPORTED
        );
        let p = hash_payload(
            HASH_POLY1305,
            HASH_FLAG_HMAC,
            (src, 0, 3),
            (dst, 0),
            (key, 0, 32),
        );
        assert_eq!(
            call(&mut b, 6, OP_CRYPTO_HASH, &p).status,
            STATUS_UNSUPPORTED
        );
        // STREAM: no flag bits are defined at all.
        let (nonce, nonce_off) = alloc(&mut b, 7, 16);
        wbytes(&mut b, nonce_off, &[0u8; 12]);
        let mut p = [0u8; 48];
        put_be32(&mut p, 0, src);
        put_be32(&mut p, 8, 3);
        put_be32(&mut p, 12, dst);
        put_be32(&mut p, 20, key);
        put_be32(&mut p, 28, nonce);
        put_be32(&mut p, 40, STREAM_CHACHA20);
        put_be32(&mut p, 44, 1);
        assert_eq!(
            call(&mut b, 8, OP_CRYPTO_STREAM, &p).status,
            STATUS_UNSUPPORTED
        );
        // AEAD: bits outside DECRYPT and the algorithm byte are rejected.
        let mut p = [0u8; 48];
        put_be32(&mut p, 0, src);
        put_be32(&mut p, 8, 3);
        put_be32(&mut p, 12, dst);
        put_be32(&mut p, 20, INVALID_HANDLE);
        put_be32(&mut p, 32, key);
        put_be32(&mut p, 40, nonce);
        put_be32(&mut p, 44, 0x2);
        assert_eq!(
            call(&mut b, 9, OP_CRYPTO_AEAD, &p).status,
            STATUS_UNSUPPORTED
        );
    }

    #[test]
    fn chacha20_counter_near_the_keystream_end_is_a_wire_error() {
        // A counter near u32::MAX with a multi-block source runs off the
        // 32-bit-block-counter keystream; the board must answer with a
        // status, never trap (a trap faults the board permanently).
        let mut b = mk();
        let (src, _) = alloc(&mut b, 1, 256);
        let (dst, _) = alloc(&mut b, 2, 256);
        let (key, key_off) = alloc(&mut b, 3, 32);
        let (nonce, nonce_off) = alloc(&mut b, 4, 16);
        wbytes(&mut b, key_off, &[1u8; 32]);
        wbytes(&mut b, nonce_off, &[2u8; 12]);
        let mut p = [0u8; 48];
        put_be32(&mut p, 0, src);
        put_be32(&mut p, 8, 128);
        put_be32(&mut p, 12, dst);
        put_be32(&mut p, 20, key);
        put_be32(&mut p, 28, nonce);
        put_be32(&mut p, 36, 0xFFFF_FFFF);
        put_be32(&mut p, 40, STREAM_CHACHA20);
        assert_eq!(
            call(&mut b, 5, OP_CRYPTO_STREAM, &p).status,
            STATUS_BAD_REQUEST
        );
        // The board is still alive afterwards.
        let c = call(&mut b, 6, OP_PING, b"ok");
        assert_eq!(c.status, STATUS_OK);
    }

    #[test]
    fn full_completion_ring_applies_backpressure() {
        // With the guest never consuming completions, the board stops
        // consuming requests once a ring's worth of completions waits:
        // `pending` stays bounded and the request ring fills so the
        // guest-side transport reports BUSY, exactly like stalled real
        // hardware. Draining the completion ring un-wedges everything.
        let mut b = mk();
        let mut submitted = 0u32;
        for _ in 0..4 {
            loop {
                let head = r32(&b, DESC + DESC_REQ_HEAD);
                let tail = r32(&b, DESC + DESC_REQ_TAIL);
                if (tail + 1) % RING_ENTRIES == head {
                    break;
                }
                submit(&mut b, 100 + submitted, OP_PING, &[]);
                submitted += 1;
            }
            for _ in 0..200 {
                b.tick(2000);
            }
        }
        assert!(
            b.pending.len() <= RING_ENTRIES as usize,
            "pending grew unbounded: {}",
            b.pending.len()
        );
        // Request ring is left full (BUSY for the guest)...
        let head = r32(&b, DESC + DESC_REQ_HEAD);
        let tail = r32(&b, DESC + DESC_REQ_TAIL);
        assert_eq!((tail + 1) % RING_ENTRIES, head);
        // ...and draining completions lets every submission finish in
        // order with nothing lost.
        let mut consumed = 0u32;
        for _ in 0..200_000 {
            b.tick(200);
            if let Some(c) = poll(&mut b) {
                assert_eq!(c.request_id, 100 + consumed);
                consumed += 1;
                if consumed == submitted {
                    break;
                }
            }
        }
        assert_eq!(consumed, submitted);
    }

    #[test]
    fn chacha20_stream_with_counter_through_the_mailbox() {
        let mut b = mk();
        let (src, src_off) = alloc(&mut b, 80, 128);
        let (dst, dst_off) = alloc(&mut b, 81, 128);
        let (key, key_off) = alloc(&mut b, 82, 32);
        let (nonce, nonce_off) = alloc(&mut b, 83, 16);
        let keyb: Vec<u8> = (0u8..32).collect();
        wbytes(&mut b, key_off, &keyb);
        wbytes(&mut b, nonce_off, &[0, 0, 0, 0, 0, 0, 0, 0x4A, 0, 0, 0, 0]);
        let pt = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        wbytes(&mut b, src_off, pt);
        let mut p = [0u8; 48];
        put_be32(&mut p, 0, src);
        put_be32(&mut p, 8, pt.len() as u32);
        put_be32(&mut p, 12, dst);
        put_be32(&mut p, 20, key);
        put_be32(&mut p, 28, nonce);
        put_be32(&mut p, 36, 1); // initial block counter
        put_be32(&mut p, 40, STREAM_CHACHA20);
        let c = call(&mut b, 84, OP_CRYPTO_STREAM, &p);
        assert_eq!(c.status, STATUS_OK);
        assert_eq!(get_be32(&c.payload, 0), pt.len() as u32);
        assert_eq!(get_be32(&c.payload, 4), STREAM_CHACHA20);
        assert_eq!(
            rbytes(&b, dst_off, 4),
            vec![0x6E, 0x2E, 0x35, 0x9A],
            "RFC 8439 ciphertext head"
        );
    }
}
