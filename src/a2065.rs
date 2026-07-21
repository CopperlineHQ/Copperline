// SPDX-License-Identifier: GPL-3.0-or-later

//! Commodore A2065 Ethernet board: a Zorro II autoconfig board carrying an
//! Am7990 LANCE and 32 KiB of on-board RAM, driven by the AmigaOS SANA-II
//! `a2065.device`.
//!
//! Unlike the A2091/CDTV DMACs, the LANCE does not master the Amiga bus: its
//! init block, descriptor rings, and packet buffers all live in the board's own
//! 32 KiB RAM, which the CPU also reaches through the board window. So this
//! board is self-contained -- it owns a [`crate::net::NetBackend`] for real
//! frames and never touches Amiga chip RAM.
//!
//! Board window layout (per the Linux `a2065` driver and WinUAE):
//! - `$0000..`     MAC address PROM (the station address the driver reads)
//! - `$4000`       LANCE RDP (register data port), `$4002` RAP (address port)
//! - `$8000..$FFFF` 32 KiB shared RAM (LANCE address 0 maps here)
//!
//! The LANCE programming model (Am7990 datasheet): the host selects a CSR via
//! RAP and reads/writes it via RDP. CSR1/CSR2 hold the init-block address; on
//! INIT the chip reads the init block (mode, MAC, RX/TX ring base + length) from
//! RAM, and on STRT it services the rings. Transmit descriptors the host hands
//! to the chip (OWN bit set) are sent; inbound frames are written into receive
//! descriptors the host left owned by the chip. RINT/TINT in CSR0 raise INT2
//! when interrupts are enabled.
//!
//! Word fields in the init block and descriptors are accessed big-endian, the
//! layout a 68000 driver writes with the LANCE's byte-swap (CSR3 BSWP) set;
//! packet payloads are byte streams and are not swapped. The engine models the
//! Am7990 features a real driver exercises -- TX/RX buffer chaining across
//! descriptors, the stored FCS trailer counted in MCNT, the init-block MODE
//! gates (DTX/DRX and the LOOP internal-loopback self-test), and MISS on an RX
//! ring overrun -- and has been validated end-to-end against the AmigaOS
//! SANA-II `a2065.device` driving AmiTCP over the userspace NAT backend
//! (`crate::net::nat`). See the tests and the Zorro chapter in the docs.

use crate::net::{make_backend, NetBackend, NetConfig};
use crate::zorro_device::{DeviceHost, ZorroDevice};
use serde::{Deserialize, Serialize};

const REG_RDP: u32 = 0x4000;
const REG_RAP: u32 = 0x4002;
const RAM_BASE: u32 = 0x8000;
const RAM_SIZE: usize = 0x8000; // 32 KiB on-board RAM

// CSR0 bits (Am7990).
const CSR0_INIT: u16 = 1 << 0;
const CSR0_STRT: u16 = 1 << 1;
const CSR0_STOP: u16 = 1 << 2;
const CSR0_TDMD: u16 = 1 << 3;
const CSR0_TXON: u16 = 1 << 4;
const CSR0_RXON: u16 = 1 << 5;
const CSR0_INEA: u16 = 1 << 6;
const CSR0_INTR: u16 = 1 << 7;
const CSR0_IDON: u16 = 1 << 8;
const CSR0_TINT: u16 = 1 << 9;
const CSR0_RINT: u16 = 1 << 10;
const CSR0_MISS: u16 = 1 << 12;
const CSR0_ERR: u16 = 1 << 15;
/// Status bits the host clears by writing 1 (the rest of CSR0 is control).
const CSR0_RC_BITS: u16 = CSR0_IDON | CSR0_TINT | CSR0_RINT | (0xF << 11) | CSR0_ERR;

// Init-block MODE word bits (Am7990). PROM (bit 15) and the LADRF hash are
// not modelled: the receiver accepts every frame the backend delivers, which
// behaves like PROM permanently set. A NAT/loopback backend only ever hands
// the board frames addressed to it (or broadcasts), so the filter would be
// a no-op anyway.
const MODE_DRX: u16 = 1 << 0;
const MODE_DTX: u16 = 1 << 1;
const MODE_LOOP: u16 = 1 << 2;

// Descriptor status byte (high byte of the second word).
const DESC_OWN: u8 = 1 << 7;
const DESC_ERR: u8 = 1 << 6;
const DESC_BUFF: u8 = 1 << 2;
const DESC_STP: u8 = 1 << 1;
const DESC_ENP: u8 = 1 << 0;

/// Largest Ethernet frame the chip will move (no jumbo frames).
const MAX_FRAME: usize = 1518;

/// The frame check sequence the chip stores after a received payload. The
/// receiver writes the frame as it came off the wire, FCS included, and MCNT
/// counts it; drivers recover the payload length as `MCNT - 4`. No CRC is
/// modelled (drivers strip the FCS, they never verify it), so zeros are
/// stored.
const FCS_LEN: usize = 4;

#[derive(Serialize, Deserialize)]
pub struct A2065 {
    /// Synthesized station MAC (locally administered).
    mac: [u8; 6],
    /// 32 KiB on-board RAM (LANCE address space and CPU window share it).
    ram: Vec<u8>,
    /// Which host backend to bring up (recorded so a save state is
    /// self-contained); the live backend is a non-serialized host resource.
    net_config: NetConfig,

    // LANCE registers.
    rap: u16,
    csr0: u16,
    csr1: u16,
    csr2: u16,
    csr3: u16,

    // Ring state derived from the init block on INIT.
    mode: u16,
    rx_ring: u32,
    rx_len: u32,
    rx_mask: u32,
    rx_cur: u32,
    tx_ring: u32,
    tx_len: u32,
    tx_mask: u32,
    tx_cur: u32,
    running: bool,

    /// HDD/activity-style LED latch (board activity).
    activity: bool,

    /// Live host network backend; rebuilt from `net_config`, not serialized.
    #[serde(skip)]
    net: Option<Box<dyn NetBackend>>,
}

impl A2065 {
    pub fn new(net_config: NetConfig) -> Self {
        // A locally-administered MAC (02:..) derived from a fixed prefix; a
        // real board reads this from a PROM.
        let mac = [0x02, 0x00, 0x10, 0x00, 0x00, 0x01];
        Self {
            mac,
            ram: vec![0u8; RAM_SIZE],
            net_config,
            rap: 0,
            csr0: CSR0_STOP,
            csr1: 0,
            csr2: 0,
            csr3: 0,
            mode: 0,
            rx_ring: 0,
            rx_len: 0,
            rx_mask: 0,
            rx_cur: 0,
            tx_ring: 0,
            tx_len: 0,
            tx_mask: 0,
            tx_cur: 0,
            running: false,
            activity: false,
            net: make_backend(net_config),
        }
    }

    /// Reattach the live network backend after a save-state load (the backend
    /// is a host resource and is brought up fresh, like an audio sink).
    fn ensure_backend(&mut self) {
        if self.net.is_none() {
            self.net = make_backend(self.net_config);
        }
    }

    // ----- board RAM word access (big-endian) ----------------------------

    fn ram_word(&self, addr: u32) -> u16 {
        let a = (addr as usize) & (RAM_SIZE - 1);
        if a + 1 < RAM_SIZE {
            (u16::from(self.ram[a]) << 8) | u16::from(self.ram[a + 1])
        } else {
            0
        }
    }

    fn set_ram_word(&mut self, addr: u32, w: u16) {
        let a = (addr as usize) & (RAM_SIZE - 1);
        if a + 1 < RAM_SIZE {
            self.ram[a] = (w >> 8) as u8;
            self.ram[a + 1] = w as u8;
        }
    }

    // ----- LANCE engine ---------------------------------------------------

    fn init_block_addr(&self) -> u32 {
        (u32::from(self.csr1) | (u32::from(self.csr2 & 0xFF) << 16)) & !1
    }

    /// Read the init block (Am7990 layout) from board RAM and set up the rings.
    fn lance_init(&mut self) {
        let iadr = self.init_block_addr();
        // +0 MODE, +2..+8 PADR (MAC), +8..+10 LADRF (filter, ignored).
        self.mode = self.ram_word(iadr);
        for i in 0..3 {
            let w = self.ram_word(iadr + 2 + i * 2);
            // PADR is stored low-byte-first per word in LANCE order.
            self.mac[i as usize * 2] = w as u8;
            self.mac[i as usize * 2 + 1] = (w >> 8) as u8;
        }
        // +$10 RDRA[15:0], +$12 (RLEN<<13) | RDRA[23:16].
        let rdra_lo = self.ram_word(iadr + 0x10);
        let rdra_hi = self.ram_word(iadr + 0x12);
        self.rx_ring = u32::from(rdra_lo) | (u32::from(rdra_hi & 0xFF) << 16);
        self.rx_len = 1 << ((rdra_hi >> 13) & 0x7);
        self.rx_mask = self.rx_len - 1;
        self.rx_cur = 0;
        // +$14 TDRA[15:0], +$16 (TLEN<<13) | TDRA[23:16].
        let tdra_lo = self.ram_word(iadr + 0x14);
        let tdra_hi = self.ram_word(iadr + 0x16);
        self.tx_ring = u32::from(tdra_lo) | (u32::from(tdra_hi & 0xFF) << 16);
        self.tx_len = 1 << ((tdra_hi >> 13) & 0x7);
        self.tx_mask = self.tx_len - 1;
        self.tx_cur = 0;
        self.csr0 |= CSR0_IDON;
    }

    /// Address of descriptor `n` (8 bytes each) in a ring.
    fn desc_addr(ring: u32, n: u32) -> u32 {
        ring + n * 8
    }

    fn desc_buffer(&self, dbase: u32) -> (u32, usize) {
        let ladr = self.ram_word(dbase);
        let mid = self.ram_word(dbase + 2); // status<<8 | HADR
        let hadr = (mid & 0xFF) as u32;
        let addr = u32::from(ladr) | (hadr << 16);
        // BCNT at +4 is a two's-complement negative count in bits 0..11.
        let bcnt = self.ram_word(dbase + 4);
        let len = (((!bcnt) & 0x0FFF) + 1) as usize;
        (addr, len)
    }

    fn desc_status(&self, dbase: u32) -> u8 {
        (self.ram_word(dbase + 2) >> 8) as u8
    }

    fn set_desc_status(&mut self, dbase: u32, status: u8) {
        let w = self.ram_word(dbase + 2);
        self.set_ram_word(dbase + 2, (u16::from(status) << 8) | (w & 0xFF));
    }

    /// Walk the TX ring, sending every complete chip-owned frame. A frame may
    /// span several descriptors (Am7990 buffer chaining: STP marks the first
    /// buffer, ENP the last); the chip only takes a frame once the whole
    /// STP..ENP span is chip-owned, so a frame the driver is still handing
    /// over stays untouched until the ENP descriptor's OWN bit lands.
    fn poll_tx(&mut self) {
        if !self.running || self.tx_len == 0 || self.mode & MODE_DTX != 0 {
            return;
        }
        'frames: loop {
            // Scan the frame without consuming anything first.
            let mut span: Vec<(u32, u32, usize)> = Vec::new(); // (desc, buf, len)
            let mut desc = self.tx_cur;
            loop {
                if span.len() as u32 >= self.tx_len {
                    // The whole ring is chip-owned with no ENP anywhere: a
                    // malformed ring the real chip would flag BUFF on. Stall
                    // rather than spin.
                    break 'frames;
                }
                let dbase = Self::desc_addr(self.tx_ring, desc);
                let status = self.desc_status(dbase);
                if status & DESC_OWN == 0 {
                    break 'frames; // frame not fully handed over yet
                }
                if span.is_empty() && status & DESC_STP == 0 {
                    // Chip-owned buffer with no STP: the chip skips it while
                    // hunting for a start of packet, handing it back.
                    self.set_desc_status(dbase, status & !DESC_OWN);
                    self.tx_cur = (self.tx_cur + 1) & self.tx_mask;
                    continue 'frames;
                }
                let (buf_addr, len) = self.desc_buffer(dbase);
                span.push((dbase, buf_addr, len));
                if status & DESC_ENP != 0 {
                    break;
                }
                desc = (desc + 1) & self.tx_mask;
            }
            let mut frame = Vec::new();
            for &(_, buf_addr, len) in &span {
                for i in 0..len {
                    if frame.len() >= MAX_FRAME {
                        break;
                    }
                    frame.push(self.ram[(buf_addr as usize + i) & (RAM_SIZE - 1)]);
                }
            }
            if self.mode & MODE_LOOP != 0 {
                // Internal loopback: the frame goes straight to our own
                // receiver and never reaches the wire. SANA-II drivers run a
                // power-up self-test this way before going online.
                self.receive_frame(&frame);
            } else if let Some(net) = self.net.as_mut() {
                net.send(&frame);
            }
            self.activity = true;
            for &(dbase, _, _) in &span {
                let status = self.desc_status(dbase);
                self.set_desc_status(dbase, status & !DESC_OWN);
            }
            self.tx_cur = (desc + 1) & self.tx_mask;
            self.csr0 |= CSR0_TINT;
        }
    }

    /// Deliver one inbound frame into chip-owned RX descriptors, chaining
    /// across buffers when the frame is longer than one buffer (Am7990: STP
    /// set in the first descriptor, ENP and MCNT written in the last). The
    /// stored message is payload plus [`FCS_LEN`] trailer bytes, as received
    /// off the wire.
    fn receive_frame(&mut self, frame: &[u8]) -> bool {
        if !self.running || self.rx_len == 0 || self.mode & MODE_DRX != 0 {
            return false;
        }
        let total = (frame.len() + FCS_LEN).min(MAX_FRAME);
        let mut remaining = total;
        let mut copied = 0usize;
        let mut desc = self.rx_cur;
        let mut first = true;
        loop {
            let dbase = Self::desc_addr(self.rx_ring, desc);
            let status = self.desc_status(dbase);
            if status & DESC_OWN == 0 {
                if first {
                    // No descriptor to start the frame in: missed frame.
                    self.csr0 |= CSR0_MISS;
                    return false;
                }
                // Ran out of chained buffers mid-frame: the last buffer the
                // chip filled gets BUFF, the frame is truncated, no ENP.
                let prev = (desc + self.rx_mask) & self.rx_mask;
                let pbase = Self::desc_addr(self.rx_ring, prev);
                let pstatus = self.desc_status(pbase);
                self.set_desc_status(pbase, (pstatus & !DESC_ENP) | DESC_BUFF | DESC_ERR);
                self.rx_cur = desc;
                self.activity = true;
                self.csr0 |= CSR0_RINT;
                return true;
            }
            let (buf_addr, cap) = self.desc_buffer(dbase);
            let n = remaining.min(cap);
            for i in 0..n {
                let a = (buf_addr as usize + i) & (RAM_SIZE - 1);
                self.ram[a] = *frame.get(copied + i).unwrap_or(&0);
            }
            copied += n;
            remaining -= n;
            let mut st = status & !(DESC_OWN | DESC_ERR | DESC_BUFF | DESC_STP | DESC_ENP);
            if first {
                st |= DESC_STP;
            }
            if remaining == 0 {
                // MCNT (message byte count, FCS included) is only valid in
                // the descriptor carrying ENP.
                st |= DESC_ENP;
                self.set_ram_word(dbase + 6, total as u16);
            }
            self.set_desc_status(dbase, st);
            desc = (desc + 1) & self.rx_mask;
            first = false;
            if remaining == 0 {
                break;
            }
        }
        self.rx_cur = desc;
        self.activity = true;
        self.csr0 |= CSR0_RINT;
        true
    }

    // ----- CSR access -----------------------------------------------------

    fn read_csr(&self) -> u16 {
        match self.rap & 3 {
            0 => self.csr0_value(),
            1 => self.csr1,
            2 => self.csr2,
            _ => self.csr3,
        }
    }

    /// CSR0 with its derived summary bits (INTR, ERR) folded in.
    fn csr0_value(&self) -> u16 {
        let mut v = self.csr0;
        let err = v & (CSR0_ERR | (0xF << 11)); // BABL|CERR|MISS|MERR rolled up
        if err != 0 {
            v |= CSR0_ERR;
        }
        if (v & (CSR0_ERR | CSR0_IDON | CSR0_TINT | CSR0_RINT)) != 0 && (v & CSR0_INEA) != 0 {
            v |= CSR0_INTR;
        }
        v
    }

    fn write_csr(&mut self, val: u16) {
        match self.rap & 3 {
            0 => self.write_csr0(val),
            1 => self.csr1 = val & !1,
            2 => self.csr2 = val & 0xFF,
            3 => self.csr3 = val & 0x7,
            _ => {}
        }
    }

    fn write_csr0(&mut self, val: u16) {
        if val & CSR0_STOP != 0 {
            // STOP resets the chip to idle.
            self.csr0 = CSR0_STOP;
            self.running = false;
            return;
        }
        // Clear the status bits the host wrote 1 to.
        self.csr0 &= !(val & CSR0_RC_BITS);
        // INEA is read/write.
        if val & CSR0_INEA != 0 {
            self.csr0 |= CSR0_INEA;
        } else {
            self.csr0 &= !CSR0_INEA;
        }
        self.csr0 &= !CSR0_STOP;
        if val & CSR0_INIT != 0 {
            self.lance_init();
        }
        if val & CSR0_STRT != 0 {
            self.running = true;
            self.csr0 |= CSR0_TXON | CSR0_RXON;
        }
        if val & CSR0_TDMD != 0 {
            self.poll_tx();
        }
    }

    fn int_line(&self) -> bool {
        self.csr0_value() & CSR0_INTR != 0
    }
}

impl ZorroDevice for A2065 {
    fn read(&mut self, off: u32, size: usize, _host: &mut DeviceHost) -> u32 {
        let mut value = 0u32;
        for i in 0..size as u32 {
            value = (value << 8) | u32::from(self.read_byte(off + i));
        }
        value
    }

    fn write(&mut self, off: u32, size: usize, value: u32, _host: &mut DeviceHost) {
        // The LANCE registers are word ports; gather a word write and apply it
        // once. RAM is byte-addressable.
        if off == REG_RDP || off == REG_RAP {
            let w = value as u16;
            if off == REG_RDP {
                self.write_csr(w);
            } else {
                self.rap = w & 3;
            }
            return;
        }
        for i in 0..size as u32 {
            let shift = 8 * (size as u32 - 1 - i);
            self.write_byte(off + i, (value >> shift) as u8);
        }
    }

    fn tick(&mut self, _cck: u32, _host: &mut DeviceHost) {
        // Pull any inbound frames the backend has queued into the RX ring.
        self.ensure_backend();
        while self.running {
            let frame = match self.net.as_mut().and_then(|n| n.poll()) {
                Some(f) => f,
                None => break,
            };
            if !self.receive_frame(&frame) {
                break; // no RX descriptor (MISS latched) or receiver off:
                       // the frame is lost, as on the real chip
            }
        }
    }

    fn int2_line(&self) -> bool {
        self.int_line()
    }

    fn is_idle(&self) -> bool {
        false
    }

    fn take_activity(&mut self) -> bool {
        std::mem::take(&mut self.activity)
    }

    fn reset(&mut self) {
        self.rap = 0;
        self.csr0 = CSR0_STOP;
        self.csr1 = 0;
        self.csr2 = 0;
        self.csr3 = 0;
        self.mode = 0;
        self.running = false;
        self.ram.fill(0);
        self.net = make_backend(self.net_config);
    }

    fn kind(&self) -> &'static str {
        "a2065"
    }
}

impl A2065 {
    fn read_byte(&self, off: u32) -> u8 {
        match off {
            // RDP/RAP are word ports; a byte read takes the relevant half.
            REG_RDP | 0x4001 => {
                let w = self.read_csr();
                if off & 1 == 0 {
                    (w >> 8) as u8
                } else {
                    w as u8
                }
            }
            REG_RAP | 0x4003 => {
                if off & 1 == 0 {
                    (self.rap >> 8) as u8
                } else {
                    self.rap as u8
                }
            }
            _ if off < 12 => {
                // MAC PROM at even byte offsets 0,2,4,...; odd bytes read 0.
                if off & 1 == 0 {
                    self.mac[(off / 2) as usize]
                } else {
                    0
                }
            }
            _ if (RAM_BASE..RAM_BASE + RAM_SIZE as u32).contains(&off) => {
                self.ram[(off - RAM_BASE) as usize]
            }
            _ => 0xFF,
        }
    }

    fn write_byte(&mut self, off: u32, b: u8) {
        if (RAM_BASE..RAM_BASE + RAM_SIZE as u32).contains(&off) {
            self.ram[(off - RAM_BASE) as usize] = b;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_mem() -> crate::memory::Memory {
        crate::memory::Memory {
            chip_ram: vec![0u8; 0x100],
            slow_ram: Vec::new(),
            mb_ram: Vec::new(),
            accel_ram: Vec::new(),
            rom: Vec::new(),
            overlay: false,
            zorro: crate::zorro::ZorroChain::default(),
            extended_rom: Vec::new(),
            extended_rom_base: 0,
            wcs: Vec::new(),
            wcs_write_protected: false,
        }
    }

    /// Drive a CSR write the way the driver does: RAP <- n, then RDP <- value.
    fn write_csr(board: &mut A2065, mem: &mut crate::memory::Memory, csr: u16, val: u16) {
        let mut host = DeviceHost::new(mem);
        board.write(REG_RAP, 2, u32::from(csr), &mut host);
        board.write(REG_RDP, 2, u32::from(val), &mut host);
    }

    fn read_csr(board: &mut A2065, mem: &mut crate::memory::Memory, csr: u16) -> u16 {
        let mut host = DeviceHost::new(mem);
        board.write(REG_RAP, 2, u32::from(csr), &mut host);
        board.read(REG_RDP, 2, &mut host) as u16
    }

    /// Lay an init block + a 1-entry RX ring and 1-entry TX ring into board RAM,
    /// with buffers, and point CSR1/CSR2 at the init block. Returns the LANCE
    /// addresses of the TX and RX buffers.
    fn set_up_rings(board: &mut A2065, mem: &mut crate::memory::Memory) -> (u32, u32) {
        // Layout in board RAM (LANCE addresses):
        //   init block @ 0x000, RX ring @ 0x100, TX ring @ 0x200,
        //   RX buffer @ 0x400, TX buffer @ 0x600.
        let iadr = 0x000u32;
        let rx_ring = 0x100u32;
        let tx_ring = 0x200u32;
        let rx_buf = 0x400u32;
        let tx_buf = 0x600u32;

        // helper to write a word at a LANCE address through the CPU window.
        let put = |board: &mut A2065, mem: &mut crate::memory::Memory, addr: u32, w: u16| {
            let mut host = DeviceHost::new(mem);
            board.write(RAM_BASE + addr, 2, u32::from(w), &mut host);
        };

        // Init block: MODE=0, PADR (MAC), LADRF=0, RDRA/RLEN, TDRA/TLEN.
        put(board, mem, iadr, 0); // mode
        put(board, mem, iadr + 2, 0x0201); // PADR word0 (low-first): 02,01? test only
        put(board, mem, iadr + 4, 0x0403);
        put(board, mem, iadr + 6, 0x0605);
        // RX ring: RLEN log2 = 0 (1 entry) in bits 13-15.
        put(board, mem, iadr + 0x10, rx_ring as u16);
        // High word: RLEN log2 = 0 (1 entry) in bits 13-15, RDRA[23:16] in 0-7.
        put(board, mem, iadr + 0x12, (rx_ring >> 16) as u16 & 0xFF);
        // TX ring: TLEN log2 = 0.
        put(board, mem, iadr + 0x14, tx_ring as u16);
        put(board, mem, iadr + 0x16, (tx_ring >> 16) as u16 & 0xFF);

        // RX descriptor 0: buffer @ rx_buf, OWN=chip, BCNT = -256.
        put(board, mem, rx_ring, rx_buf as u16);
        put(
            board,
            mem,
            rx_ring + 2,
            (u16::from(DESC_OWN) << 8) | ((rx_buf >> 16) as u16 & 0xFF),
        );
        put(board, mem, rx_ring + 4, 0xF000 | (((!256u16) + 1) & 0x0FFF));

        // TX descriptor 0: host-owned initially (OWN=0).
        put(board, mem, tx_ring, tx_buf as u16);
        put(board, mem, tx_ring + 2, (tx_buf >> 16) as u16 & 0xFF);

        // Point CSR1/CSR2 at the init block.
        write_csr(board, mem, 1, iadr as u16);
        write_csr(board, mem, 2, (iadr >> 16) as u16);
        (tx_buf, rx_buf)
    }

    #[test]
    fn rap_selects_csr_and_init_done_sets_idon() {
        let mut board = A2065::new(NetConfig::Loopback);
        let mut mem = host_mem();
        set_up_rings(&mut board, &mut mem);

        // INIT then STRT.
        write_csr(&mut board, &mut mem, 0, CSR0_INIT);
        assert_ne!(read_csr(&mut board, &mut mem, 0) & CSR0_IDON, 0);
        write_csr(&mut board, &mut mem, 0, CSR0_STRT);
        assert_ne!(read_csr(&mut board, &mut mem, 0) & CSR0_RXON, 0);
    }

    #[test]
    fn transmit_descriptor_is_sent_to_the_backend() {
        let mut board = A2065::new(NetConfig::Loopback);
        let mut mem = host_mem();
        let (tx_buf, _rx) = set_up_rings(&mut board, &mut mem);
        write_csr(&mut board, &mut mem, 0, CSR0_INIT);
        write_csr(&mut board, &mut mem, 0, CSR0_STRT | CSR0_INEA);

        // Put a 64-byte frame in the TX buffer, count = -64, OWN=chip, STP|ENP.
        {
            let mut host = DeviceHost::new(&mut mem);
            for i in 0..64u32 {
                board.write(
                    RAM_BASE + tx_buf + i,
                    1,
                    u32::from(0x40 + i as u8),
                    &mut host,
                );
            }
        }
        // TX descriptor 0 at LANCE 0x200: BCNT=-64, status OWN|STP|ENP.
        {
            let mut host = DeviceHost::new(&mut mem);
            board.write(
                RAM_BASE + 0x204,
                2,
                u32::from(0xF000 | (((!64u16) + 1) & 0x0FFF)),
                &mut host,
            );
            // Status byte in the high byte; HADR (low byte) is 0.
            board.write(
                RAM_BASE + 0x202,
                2,
                u32::from(u16::from(DESC_OWN | DESC_STP | DESC_ENP) << 8),
                &mut host,
            );
        }
        // Transmit demand.
        write_csr(&mut board, &mut mem, 0, CSR0_TDMD);

        // The loopback backend should now hold the 64-byte frame, and TINT set.
        assert_ne!(read_csr(&mut board, &mut mem, 0) & CSR0_TINT, 0);
        let frame = board
            .net
            .as_mut()
            .unwrap()
            .poll()
            .expect("frame transmitted");
        assert_eq!(frame.len(), 64);
        assert_eq!(frame[0], 0x40);
        assert_eq!(frame[63], 0x40 + 63);
    }

    #[test]
    fn inbound_frame_lands_in_rx_ring_and_raises_int() {
        let mut board = A2065::new(NetConfig::Loopback);
        let mut mem = host_mem();
        set_up_rings(&mut board, &mut mem);
        write_csr(&mut board, &mut mem, 0, CSR0_INIT);
        write_csr(&mut board, &mut mem, 0, CSR0_STRT | CSR0_INEA);

        // Inject a frame into the backend, then tick to deliver it.
        board.net.as_mut().unwrap().send(&[0xAA, 0xBB, 0xCC, 0xDD]);
        {
            let mut host = DeviceHost::new(&mut mem);
            board.tick(1, &mut host);
        }

        // RINT set and INT2 asserted (INEA on).
        assert_ne!(read_csr(&mut board, &mut mem, 0) & CSR0_RINT, 0);
        assert!(board.int2_line());

        // The frame bytes are in the RX buffer (LANCE 0x400 -> window 0x8400).
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(RAM_BASE + 0x400, 1, &mut host), 0xAA);
        assert_eq!(board.read(RAM_BASE + 0x403, 1, &mut host), 0xDD);
    }

    #[test]
    fn stop_clears_running_and_int() {
        let mut board = A2065::new(NetConfig::Loopback);
        let mut mem = host_mem();
        set_up_rings(&mut board, &mut mem);
        write_csr(&mut board, &mut mem, 0, CSR0_INIT);
        write_csr(&mut board, &mut mem, 0, CSR0_STRT | CSR0_INEA);
        board.net.as_mut().unwrap().send(&[1, 2, 3, 4]);
        {
            let mut host = DeviceHost::new(&mut mem);
            board.tick(1, &mut host);
        }
        assert!(board.int2_line());

        write_csr(&mut board, &mut mem, 0, CSR0_STOP);
        assert!(!board.int2_line());
        assert!(!board.running);
    }

    /// Lay an init block with the given MODE word plus multi-entry rings into
    /// board RAM: RX ring at 0x100 (2^rx_log2 entries, chip-owned, each with a
    /// rx_cap-byte buffer at 0x1000 + n*rx_cap), TX ring at 0x200 (2^tx_log2
    /// entries, host-owned, buffers at 0x3000 + n*0x200).
    fn set_up_rings_geom(
        board: &mut A2065,
        mem: &mut crate::memory::Memory,
        mode: u16,
        rx_log2: u16,
        rx_cap: u16,
        tx_log2: u16,
    ) {
        let iadr = 0x000u32;
        let rx_ring = 0x100u32;
        let tx_ring = 0x200u32;
        let put = |board: &mut A2065, mem: &mut crate::memory::Memory, addr: u32, w: u16| {
            let mut host = DeviceHost::new(mem);
            board.write(RAM_BASE + addr, 2, u32::from(w), &mut host);
        };
        put(board, mem, iadr, mode);
        put(board, mem, iadr + 2, 0x0201);
        put(board, mem, iadr + 4, 0x0403);
        put(board, mem, iadr + 6, 0x0605);
        put(board, mem, iadr + 0x10, rx_ring as u16);
        put(
            board,
            mem,
            iadr + 0x12,
            (rx_log2 << 13) | ((rx_ring >> 16) as u16 & 0xFF),
        );
        put(board, mem, iadr + 0x14, tx_ring as u16);
        put(
            board,
            mem,
            iadr + 0x16,
            (tx_log2 << 13) | ((tx_ring >> 16) as u16 & 0xFF),
        );
        for n in 0..(1u32 << rx_log2) {
            let dbase = rx_ring + n * 8;
            let buf = 0x1000 + n * u32::from(rx_cap);
            put(board, mem, dbase, buf as u16);
            put(
                board,
                mem,
                dbase + 2,
                (u16::from(DESC_OWN) << 8) | ((buf >> 16) as u16 & 0xFF),
            );
            put(
                board,
                mem,
                dbase + 4,
                0xF000 | ((!rx_cap).wrapping_add(1) & 0x0FFF),
            );
        }
        for n in 0..(1u32 << tx_log2) {
            let dbase = tx_ring + n * 8;
            let buf = 0x3000 + n * 0x200;
            put(board, mem, dbase, buf as u16);
            put(board, mem, dbase + 2, (buf >> 16) as u16 & 0xFF);
        }
        write_csr(board, mem, 1, iadr as u16);
        write_csr(board, mem, 2, (iadr >> 16) as u16);
    }

    /// Hand TX descriptor `n` to the chip with a byte count and status bits.
    fn arm_tx_desc(
        board: &mut A2065,
        mem: &mut crate::memory::Memory,
        n: u32,
        len: u16,
        status: u8,
    ) {
        let dbase = 0x200 + n * 8;
        let mut host = DeviceHost::new(mem);
        board.write(
            RAM_BASE + dbase + 4,
            2,
            u32::from(0xF000 | ((!len).wrapping_add(1) & 0x0FFF)),
            &mut host,
        );
        board.write(
            RAM_BASE + dbase + 2,
            2,
            u32::from(u16::from(status) << 8),
            &mut host,
        );
    }

    fn rx_desc_status(board: &mut A2065, mem: &mut crate::memory::Memory, n: u32) -> u8 {
        let mut host = DeviceHost::new(mem);
        (board.read(RAM_BASE + 0x100 + n * 8 + 2, 2, &mut host) >> 8) as u8
    }

    fn fill_tx_buf(board: &mut A2065, mem: &mut crate::memory::Memory, n: u32, data: &[u8]) {
        let mut host = DeviceHost::new(mem);
        for (i, b) in data.iter().enumerate() {
            board.write(
                RAM_BASE + 0x3000 + n * 0x200 + i as u32,
                1,
                u32::from(*b),
                &mut host,
            );
        }
    }

    #[test]
    fn tx_frame_chains_across_descriptors() {
        let mut board = A2065::new(NetConfig::Loopback);
        let mut mem = host_mem();
        set_up_rings_geom(&mut board, &mut mem, 0, 0, 256, 1);
        write_csr(&mut board, &mut mem, 0, CSR0_INIT);
        write_csr(&mut board, &mut mem, 0, CSR0_STRT | CSR0_INEA);

        let part0: Vec<u8> = (0..40u8).collect();
        let part1: Vec<u8> = (40..64u8).collect();
        fill_tx_buf(&mut board, &mut mem, 0, &part0);
        fill_tx_buf(&mut board, &mut mem, 1, &part1);

        // Hand over only the STP half: the chip must not touch the frame yet.
        arm_tx_desc(&mut board, &mut mem, 0, 40, DESC_OWN | DESC_STP);
        write_csr(&mut board, &mut mem, 0, CSR0_TDMD);
        assert_eq!(read_csr(&mut board, &mut mem, 0) & CSR0_TINT, 0);
        assert!(board.net.as_mut().unwrap().poll().is_none());

        // Complete the span with the ENP half: one 64-byte frame goes out.
        arm_tx_desc(&mut board, &mut mem, 1, 24, DESC_OWN | DESC_ENP);
        write_csr(&mut board, &mut mem, 0, CSR0_TDMD);
        assert_ne!(read_csr(&mut board, &mut mem, 0) & CSR0_TINT, 0);
        let frame = board.net.as_mut().unwrap().poll().expect("chained frame");
        assert_eq!(frame.len(), 64);
        assert!(frame.iter().enumerate().all(|(i, b)| *b == i as u8));
        // Both descriptors handed back to the host.
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(RAM_BASE + 0x202, 2, &mut host) >> 8 & 0x80, 0);
        assert_eq!(board.read(RAM_BASE + 0x20A, 2, &mut host) >> 8 & 0x80, 0);
    }

    #[test]
    fn rx_frame_chains_across_buffers() {
        let mut board = A2065::new(NetConfig::Loopback);
        let mut mem = host_mem();
        set_up_rings_geom(&mut board, &mut mem, 0, 2, 128, 0);
        write_csr(&mut board, &mut mem, 0, CSR0_INIT);
        write_csr(&mut board, &mut mem, 0, CSR0_STRT | CSR0_INEA);

        let frame: Vec<u8> = (0..300u16).map(|i| i as u8).collect();
        board.net.as_mut().unwrap().send(&frame);
        {
            let mut host = DeviceHost::new(&mut mem);
            board.tick(1, &mut host);
        }

        // 300 + 4 FCS = 304 bytes across three 128-byte buffers.
        let d0 = rx_desc_status(&mut board, &mut mem, 0);
        let d1 = rx_desc_status(&mut board, &mut mem, 1);
        let d2 = rx_desc_status(&mut board, &mut mem, 2);
        let d3 = rx_desc_status(&mut board, &mut mem, 3);
        assert_eq!(d0 & (DESC_OWN | DESC_STP | DESC_ENP), DESC_STP);
        assert_eq!(d1 & (DESC_OWN | DESC_STP | DESC_ENP), 0);
        assert_eq!(d2 & (DESC_OWN | DESC_STP | DESC_ENP), DESC_ENP);
        assert_ne!(d3 & DESC_OWN, 0, "fourth descriptor stays chip-owned");
        let mut host = DeviceHost::new(&mut mem);
        // MCNT lives in the ENP descriptor and counts the FCS.
        assert_eq!(
            board.read(RAM_BASE + 0x100 + 2 * 8 + 6, 2, &mut host) & 0xFFF,
            304
        );
        // Payload spans the buffers; the FCS trailer bytes are zeros.
        assert_eq!(board.read(RAM_BASE + 0x1000, 1, &mut host), 0);
        assert_eq!(board.read(RAM_BASE + 0x1080, 1, &mut host), 128);
        assert_eq!(
            board.read(RAM_BASE + 0x1100 + 43, 1, &mut host),
            299u32 & 0xFF
        );
        assert_eq!(board.read(RAM_BASE + 0x1100 + 44, 1, &mut host), 0);
        assert_ne!(read_csr(&mut board, &mut mem, 0) & CSR0_RINT, 0);
    }

    #[test]
    fn rx_without_free_descriptor_sets_miss() {
        let mut board = A2065::new(NetConfig::Loopback);
        let mut mem = host_mem();
        set_up_rings_geom(&mut board, &mut mem, 0, 0, 256, 0);
        write_csr(&mut board, &mut mem, 0, CSR0_INIT);
        write_csr(&mut board, &mut mem, 0, CSR0_STRT | CSR0_INEA);
        // Take the only RX descriptor away from the chip.
        {
            let mut host = DeviceHost::new(&mut mem);
            board.write(RAM_BASE + 0x102, 2, 0, &mut host);
        }
        board.net.as_mut().unwrap().send(&[1, 2, 3]);
        {
            let mut host = DeviceHost::new(&mut mem);
            board.tick(1, &mut host);
        }
        let csr0 = read_csr(&mut board, &mut mem, 0);
        assert_ne!(csr0 & CSR0_MISS, 0);
        assert_ne!(csr0 & CSR0_ERR, 0, "MISS rolls up into ERR");
        assert_eq!(csr0 & CSR0_RINT, 0);
    }

    #[test]
    fn rx_out_of_chained_buffers_sets_buff() {
        let mut board = A2065::new(NetConfig::Loopback);
        let mut mem = host_mem();
        set_up_rings_geom(&mut board, &mut mem, 0, 0, 128, 0);
        write_csr(&mut board, &mut mem, 0, CSR0_INIT);
        write_csr(&mut board, &mut mem, 0, CSR0_STRT | CSR0_INEA);
        board.net.as_mut().unwrap().send(&[0x55u8; 200]);
        {
            let mut host = DeviceHost::new(&mut mem);
            board.tick(1, &mut host);
        }
        let d0 = rx_desc_status(&mut board, &mut mem, 0);
        assert_eq!(d0 & DESC_OWN, 0);
        assert_ne!(d0 & DESC_STP, 0);
        assert_eq!(d0 & DESC_ENP, 0, "truncated frame has no end of packet");
        assert_ne!(d0 & DESC_BUFF, 0);
        assert_ne!(d0 & DESC_ERR, 0);
        assert_ne!(read_csr(&mut board, &mut mem, 0) & CSR0_RINT, 0);
    }

    #[test]
    fn mode_loop_returns_tx_frames_to_own_receiver() {
        // No backend at all: internal loopback never reaches the wire.
        let mut board = A2065::new(NetConfig::None);
        let mut mem = host_mem();
        set_up_rings_geom(&mut board, &mut mem, MODE_LOOP, 0, 256, 0);
        write_csr(&mut board, &mut mem, 0, CSR0_INIT);
        write_csr(&mut board, &mut mem, 0, CSR0_STRT | CSR0_INEA);

        let data: Vec<u8> = (0..32u8).map(|i| i ^ 0x5A).collect();
        fill_tx_buf(&mut board, &mut mem, 0, &data);
        arm_tx_desc(&mut board, &mut mem, 0, 32, DESC_OWN | DESC_STP | DESC_ENP);
        write_csr(&mut board, &mut mem, 0, CSR0_TDMD);

        let csr0 = read_csr(&mut board, &mut mem, 0);
        assert_ne!(csr0 & CSR0_TINT, 0);
        assert_ne!(csr0 & CSR0_RINT, 0, "looped frame lands in the RX ring");
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(RAM_BASE + 0x1000, 1, &mut host), 0x5A);
        assert_eq!(board.read(RAM_BASE + 0x100 + 6, 2, &mut host) & 0xFFF, 36);
    }

    #[test]
    fn mode_dtx_drx_disable_the_engines() {
        let mut board = A2065::new(NetConfig::Loopback);
        let mut mem = host_mem();
        set_up_rings_geom(&mut board, &mut mem, MODE_DTX | MODE_DRX, 0, 256, 0);
        write_csr(&mut board, &mut mem, 0, CSR0_INIT);
        write_csr(&mut board, &mut mem, 0, CSR0_STRT | CSR0_INEA);

        fill_tx_buf(&mut board, &mut mem, 0, &[1, 2, 3, 4]);
        arm_tx_desc(&mut board, &mut mem, 0, 4, DESC_OWN | DESC_STP | DESC_ENP);
        write_csr(&mut board, &mut mem, 0, CSR0_TDMD);
        assert_eq!(read_csr(&mut board, &mut mem, 0) & CSR0_TINT, 0);
        assert!(board.net.as_mut().unwrap().poll().is_none());

        board.net.as_mut().unwrap().send(&[9, 9, 9]);
        {
            let mut host = DeviceHost::new(&mut mem);
            board.tick(1, &mut host);
        }
        assert_eq!(read_csr(&mut board, &mut mem, 0) & CSR0_RINT, 0);
        assert_ne!(rx_desc_status(&mut board, &mut mem, 0) & DESC_OWN, 0);
    }

    #[test]
    fn rx_appends_fcs_to_short_frame() {
        let mut board = A2065::new(NetConfig::Loopback);
        let mut mem = host_mem();
        set_up_rings_geom(&mut board, &mut mem, 0, 0, 256, 0);
        write_csr(&mut board, &mut mem, 0, CSR0_INIT);
        write_csr(&mut board, &mut mem, 0, CSR0_STRT | CSR0_INEA);
        board.net.as_mut().unwrap().send(&[0xAA, 0xBB, 0xCC, 0xDD]);
        {
            let mut host = DeviceHost::new(&mut mem);
            board.tick(1, &mut host);
        }
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(RAM_BASE + 0x100 + 6, 2, &mut host) & 0xFFF, 8);
        assert_eq!(board.read(RAM_BASE + 0x1000 + 3, 1, &mut host), 0xDD);
        assert_eq!(board.read(RAM_BASE + 0x1000 + 4, 1, &mut host), 0);
        assert_eq!(board.read(RAM_BASE + 0x1000 + 7, 1, &mut host), 0);
        let d0 = rx_desc_status(&mut board, &mut mem, 0);
        assert_eq!(d0 & (DESC_STP | DESC_ENP), DESC_STP | DESC_ENP);
    }

    #[test]
    fn mac_prom_is_readable_at_even_bytes() {
        let mut board = A2065::new(NetConfig::None);
        let mut mem = host_mem();
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(0, 1, &mut host), 0x02);
        assert_eq!(board.read(2, 1, &mut host), 0x00);
        assert_eq!(board.read(4, 1, &mut host), 0x10);
    }
}
