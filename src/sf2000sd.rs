// SPDX-License-Identifier: GPL-3.0-or-later

//! SF2000 accelerator Zorro II SD card controller (`sdcard.v`): a 64K I/O
//! window over an SPI-mode SD card, register-compatible with the real
//! hardware's `sdcard.v`/`shifter.v`/`fifo.v`/`tx_cpu_buf.v`/`rx_cpu_buf.v`.
//!
//! ## Boot ROM: odd lane, stride 2, latched live on first write
//!
//! The RTL available for this board is a development build with no boot ROM
//! wired up; the production board carries one. Confirmed against the real
//! firmware's `bootrom/bootldr.S` and `bootrom/mungerom.py` (the `spisd2`
//! driver source): the flash image sits on the *odd* byte lane at stride 2
//! -- `window[2k+1] = rom[k]`, even lane floats -- exactly like
//! `ide_zorro.rs`'s AT-Bus 2008 personality, and a 32K image spans the
//! whole 64K window with no mirroring. `bootldr.S`'s own relocation code
//! computes the driver payload's window offset as the flash offset "times
//! 4 (nibble-wise DiagArea)": one factor of 2 is `mungerom.py` doubling the
//! DiagArea/bootstrap portion of the image into nibble-padded bytes for
//! Kickstart 1.3's nibble-only DiagArea scanner (baked into the ROM file's
//! own content -- Kickstart does the nibble reassembly in software, nothing
//! for Copperline to do there), and the other factor of 2 is this odd-lane
//! stride. `er_InitDiagVec` is `0x0001`, i.e. window offset 1 = `rom[0]`.
//!
//! Gated the same way `ide_zorro.rs`'s RIPPLE/RIDE personalities are, on top
//! of the lane split: before the first write anywhere in the window, the
//! odd lane reads the ROM and the even lane floats; that first write
//! latches the interface live, and from then on the whole window is the
//! register file described below (odd lane floats there too -- see the
//! data-port row), with no ROM visible anywhere. `rom` absent (or `""`) is
//! hardware-only mode: registers are live immediately, no autoboot, the
//! card still works under a disk-loaded driver.
//!
//! ## Register map
//!
//! The whole 64K window mirrors one 32-byte register block (`off & 0x1F`),
//! word-addressed:
//!
//! | Offset | Register | Access | Meaning |
//! |---|---|---|---|
//! | `0x00` | CLKDIV | rw | SPI clock divider (stored/read back; not used to pace timing -- see below) |
//! | `0x02` | SLAVE_SEL | rw, bit0 | chip select (stored/read back only) |
//! | `0x04` | CARD_DET | ro, bit0 | 1 iff a card image is attached |
//! | `0x06` | STATUS | ro | see [`Sf2000Sd::status`] |
//! | `0x08` | SHIFT_CTRL | wo | bits 15:14 mode (0 stop/1 rx/2 tx/3 both), bits 12:0 rx length; word access only |
//! | `0x0A` | INTREQ | rw, bit0 | card-detect-changed pending; write 1 to clear |
//! | `0x0C` | INTENA | rw, bit0 | enable that interrupt |
//! | `0x0E` | INTACT | ro | INTREQ & INTENA |
//! | `0x10`-`0x1E` | DATA | rw | TX/RX data port (byte: upper lane only, matching `ide_zorro.rs`'s task-file convention; word: hi byte first) |
//!
//! **Timing simplification**: `CLKDIV` paces real SCLK bit timing on
//! hardware; a polled register protocol has no need for cycle-accurate SPI
//! timing to behave correctly (`ide_zorro.rs`'s task-file registers already
//! respond instantly rather than modelling real ATA bus timing), so it is
//! stored/read back faithfully but otherwise unused. What *is* modelled
//! faithfully is the FIFO **backpressure contract**, since driver code loops
//! on `busy`/FIFO-empty to pace transfers: the RX queue is capacity-34 (32
//! FIFO + 2-stage CPU buffer, matching the RTL); a `SHIFT_CTRL` receive
//! request tops it up to capacity immediately and tracks the remainder,
//! refilling one byte per drained read until exhausted -- `busy` (STATUS bit
//! 6) is set exactly while bytes are still outstanding. The TX side keeps no
//! queue at all: a data-port write in TX/BOTH mode is handed to the card
//! synchronously (see `crate::sdcard::SdCard::clock_byte`), so TX is always
//! reported empty/not-full.
//!
//! Card detect has no debounce and never changes at runtime: the card is a
//! config-fixed attachment in this version (no hot-swap), so `CARD_DET`
//! simply reflects whether an image was configured, and the INTREQ/INTENA
//! card-detect-changed interrupt path exists for register-contract
//! completeness without ever actually firing.

use crate::sdcard::SdCard;
use anyhow::{bail, Context, Result};
use std::collections::VecDeque;
use std::path::Path;

/// RX queue capacity: 32-entry FIFO + 2-stage CPU buffer, matching `fifo.v`
/// + `rx_cpu_buf.v`.
const RX_CAPACITY: usize = 34;

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum ShiftMode {
    Stop,
    Rx,
    Tx,
    Both,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Sf2000Sd {
    /// Odd-lane boot overlay (stride 2): empty in hardware-only mode.
    rom: Vec<u8>,
    /// Latches true on the first write anywhere in the window (switching
    /// from the ROM overlay to the register file); starts true with no ROM
    /// fitted, since there is then nothing to latch.
    enabled: bool,
    card: Option<SdCard>,
    clkdiv: u8,
    slave_sel: bool,
    intreq: bool,
    intena: bool,
    mode: ShiftMode,
    rx_remaining: u16,
    rx_queue: VecDeque<u8>,
    #[serde(skip, default)]
    activity: bool,
}

impl Sf2000Sd {
    /// Build the board. `rom` is empty for hardware-only mode (no ROM, no
    /// autoboot -- the card still works under a disk-loaded driver), or an
    /// odd-lane image of at most 32K (`2×len` window bytes, the whole
    /// window at the largest size) -- see [`Self::load_rom`].
    pub fn new(rom: Vec<u8>, card: Option<SdCard>) -> Result<Self> {
        if rom.len() > 0x8000 {
            bail!(
                "sf2000sd ROM image is {} bytes; expected at most 32768 (the odd lane spans \
                 2 window bytes per ROM byte, so a larger image would not fit the 64K window)",
                rom.len()
            );
        }
        let enabled = rom.is_empty();
        Ok(Self {
            rom,
            enabled,
            card,
            clkdiv: 0,
            slave_sel: false,
            intreq: false,
            intena: false,
            mode: ShiftMode::Stop,
            rx_remaining: 0,
            rx_queue: VecDeque::new(),
            activity: false,
        })
    }

    /// Load a boot ROM image from disk: the flash content presented on the
    /// odd byte lane, at most 32K (a full-size image spans the whole 64K
    /// window at stride 2). There is no fixed/padded bank size to enforce,
    /// unlike `IdeZorro::load_rom` -- a short image is used as-is (the lane
    /// floats `0xFF` past its end) rather than padded.
    pub fn load_rom(path: &Path) -> Result<Vec<u8>> {
        let rom = std::fs::read(path)
            .with_context(|| format!("reading sf2000sd ROM {}", path.display()))?;
        if rom.len() > 0x8000 {
            bail!(
                "sf2000sd ROM {} is {} bytes; expected at most 32768 (the odd lane spans \
                 2 window bytes per ROM byte, so a larger image would not fit the 64K window)",
                path.display(),
                rom.len()
            );
        }
        Ok(rom)
    }

    /// System reset: clear the register file and shift engine, re-cover the
    /// window with ROM if one is fitted (matching a real board's power-on
    /// state), and reset the card's protocol state machine (its backing
    /// image is untouched).
    pub fn reset(&mut self) {
        self.enabled = self.rom.is_empty();
        self.clkdiv = 0;
        self.slave_sel = false;
        self.intreq = false;
        self.intena = false;
        self.mode = ShiftMode::Stop;
        self.rx_remaining = 0;
        self.rx_queue.clear();
        if let Some(card) = &mut self.card {
            card.reset();
        }
    }

    /// Drain the activity latch for the HDD LED.
    pub fn take_activity(&mut self) -> bool {
        std::mem::take(&mut self.activity)
    }

    /// INT2 (PORTS): card-detect-changed, gated by INTENA. Never actually
    /// asserts in this version -- see the module documentation.
    pub fn int2_line(&self) -> bool {
        self.intena && self.intreq
    }

    pub fn kind(&self) -> &'static str {
        "sf2000sd"
    }

    /// One SPI byte-time against the attached card, or `0xFF` (bus idle)
    /// with no card fitted -- a real init handshake against an empty socket
    /// just never gets a reply, which is the correct behavior for driver
    /// software to observe.
    fn clock(&mut self, tx: u8) -> u8 {
        self.activity = true;
        self.card.as_mut().map_or(0xFF, |c| c.clock_byte(tx))
    }

    /// Generate up to `RX_CAPACITY` bytes ahead of the driver's reads,
    /// tracking how many are still owed in `rx_remaining`. Called right
    /// after a `SHIFT_CTRL` receive request and after every data-port read
    /// that drains the queue, reproducing the real shifter's flow-controlled
    /// draining without needing per-SCLK-cycle timing.
    fn top_up_rx(&mut self) {
        while self.rx_remaining > 0 && self.rx_queue.len() < RX_CAPACITY {
            let b = self.clock(0xFF);
            self.rx_queue.push_back(b);
            self.rx_remaining -= 1;
        }
    }

    /// STATUS bit layout, matching `sdcard.v`'s concatenation order exactly:
    /// bit6 busy, bit5 tx-cb-half-empty, bit4 rx-half-full, bit3 tx-cb-full,
    /// bit2 tx-cb-empty, bit1 rx-cb-full, bit0 rx-cb-empty. The "cb" bits
    /// describe the real hardware's innermost 2-stage CPU buffer, not the
    /// backing FIFO -- verified against the real `spisd2` driver's
    /// `spi.c`, whose bulk-transfer loops wait on rx-cb-full/tx-cb-empty
    /// before *every* word, meaning "at least 2 bytes ready"/"room for
    /// more", not "the whole 34-byte queue is full/empty": treating them as
    /// whole-queue thresholds (an earlier version of this code did) hangs
    /// the driver on its first multi-byte transfer, since continuous
    /// draining rarely if ever lets the queue reach true capacity. TX has
    /// no backlog at all (writes are processed synchronously), so its two
    /// bits are simply constant.
    fn status(&self) -> u16 {
        let busy = self.rx_remaining > 0;
        // Matches the RTL's fixed threshold (`rx_len >= 6'd16`) literally,
        // not a fraction of RX_CAPACITY (34): real hardware compares
        // against half the 32-entry FIFO alone, not half the FIFO-plus-CPU-
        // buffer total.
        let rx_half_full = self.rx_queue.len() >= 16;
        let rx_empty = self.rx_queue.is_empty();
        // bit1 (rx_cb_full) is the real driver's actual bulk-transfer gate
        // (`spi_read`'s word-at-a-time loop waits on it before every
        // word), so it must match the RTL's real meaning: the 2-stage CPU
        // buffer holds a full word ready to hand over -- i.e. at least 2
        // bytes are queued -- not "the whole 34-byte queue is completely
        // full". The latter is a condition continuous draining make
        // reachable so rarely, if ever, that treating it as this bit's
        // meaning hangs the driver on its very first multi-byte read.
        let rx_cb_full = self.rx_queue.len() >= 2;
        // bit3 (tx_cb_full) is always 0: TX keeps no backlog, so it is
        // simply never set below.
        (u16::from(busy) << 6)
            | (1 << 5) // tx_atleast_half_empty: TX keeps no backlog, ever.
            | (u16::from(rx_half_full) << 4)
            | (1 << 2) // tx_cb_empty: always.
            | (u16::from(rx_cb_full) << 1)
            | u16::from(rx_empty)
    }

    fn read_fixed(&self, word_index: u32, size: usize) -> u32 {
        let word: u16 = match word_index {
            0 => u16::from(self.clkdiv),
            1 => u16::from(self.slave_sel),
            2 => u16::from(self.card.is_some()),
            3 => self.status(),
            4 => 0, // SHIFT_CTRL is write-only.
            5 => u16::from(self.intreq),
            6 => u16::from(self.intena),
            7 => u16::from(self.intena && self.intreq),
            _ => unreachable!("word_index is masked to 0..=7"),
        };
        if size == 1 {
            u32::from(word as u8)
        } else {
            u32::from(word)
        }
    }

    fn write_fixed(&mut self, word_index: u32, size: usize, value: u32) {
        match word_index {
            0 => self.clkdiv = (value & 0xFF) as u8,
            1 => self.slave_sel = value & 1 != 0,
            2 | 3 => {} // CARD_DET/STATUS are read-only.
            4 => {
                // SHIFT_CTRL is inherently a 16-bit control register (mode +
                // 13-bit length); a byte write can't express that sensibly
                // and no real driver issues one, so only a word write takes
                // effect.
                if size == 2 {
                    let word = (value & 0xFFFF) as u16;
                    self.mode = match (word >> 14) & 0x3 {
                        0 => ShiftMode::Stop,
                        1 => ShiftMode::Rx,
                        2 => ShiftMode::Tx,
                        _ => ShiftMode::Both,
                    };
                    match self.mode {
                        ShiftMode::Rx | ShiftMode::Both => {
                            self.rx_remaining = word & 0x1FFF;
                            self.top_up_rx();
                        }
                        ShiftMode::Stop | ShiftMode::Tx => self.rx_remaining = 0,
                    }
                }
            }
            5 => {
                if value & 1 != 0 {
                    self.intreq = false; // write 1 to clear
                }
            }
            6 => self.intena = value & 1 != 0,
            7 => {} // INTACT is read-only.
            _ => unreachable!("word_index is masked to 0..=7"),
        }
    }

    fn read_data_port(&mut self, off: u32, size: usize) -> u32 {
        if size == 1 && off & 1 != 0 {
            return 0xFF; // odd byte lane floats, as for the fixed registers' byte access
        }
        let count = if size == 1 { 1 } else { 2 };
        let mut word = 0u32;
        for _ in 0..count {
            let b = self.rx_queue.pop_front().unwrap_or(0xFF);
            word = (word << 8) | u32::from(b);
        }
        self.top_up_rx();
        word
    }

    fn write_data_port(&mut self, off: u32, size: usize, value: u32) {
        if self.mode != ShiftMode::Tx && self.mode != ShiftMode::Both {
            // No TX queue to land in while not sending: dropped, matching a
            // real FIFO byte nothing will ever drain because the shifter
            // isn't pulling from it in this mode.
            return;
        }
        if size == 1 && off & 1 != 0 {
            return; // odd byte lane: no register there
        }
        let bytes: &[u8] = if size == 1 {
            &[(value & 0xFF) as u8]
        } else {
            &[((value >> 8) & 0xFF) as u8, (value & 0xFF) as u8]
        };
        for &b in bytes {
            let resp = self.clock(b);
            if self.mode == ShiftMode::Both && self.rx_queue.len() < RX_CAPACITY {
                self.rx_queue.push_back(resp);
            }
        }
    }

    /// The ROM byte on the odd lane at window offset `off` (stride 2: `off
    /// == 2k+1` maps to `rom[k]`), or `0xFF` past the image's end or with
    /// none fitted. Only meaningful for odd `off`; callers gate on that.
    fn read_rom(&self, off: u32) -> u32 {
        let k = (off >> 1) as usize;
        u32::from(self.rom.get(k).copied().unwrap_or(0xFF))
    }

    pub fn read(&mut self, off: u32, size: usize) -> u32 {
        if size == 4 {
            let hi = self.read(off, 2);
            let lo = self.read(off.wrapping_add(2), 2);
            return (hi << 16) | lo;
        }
        let value = if !self.enabled {
            // Odd lane, byte access: the boot ROM (or float with none
            // fitted). Even lane floats too -- nothing drives it before the
            // latch -- and a word access (always even-based, by CPU
            // alignment) never exposes ROM even where it exists, matching
            // AT-Bus 2008's own odd-lane precedent.
            if size == 1 && off & 1 != 0 {
                self.read_rom(off)
            } else if size == 1 {
                0xFF
            } else {
                0xFFFF
            }
        } else {
            let local = off & 0x1F;
            if local & 0x10 != 0 {
                self.read_data_port(local, size)
            } else {
                self.read_fixed((local >> 1) & 0x7, size)
            }
        };
        if crate::envcfg::flag("COPPERLINE_DIAG_SF2000SD") {
            log::info!(
                "sf2000sd rd {off:#06X}/{size} -> {value:#06X} (enabled={})",
                self.enabled
            );
        }
        value
    }

    pub fn write(&mut self, off: u32, size: usize, value: u32) {
        if size == 4 {
            self.write(off, 2, value >> 16);
            self.write(off.wrapping_add(2), 2, value & 0xFFFF);
            return;
        }
        if crate::envcfg::flag("COPPERLINE_DIAG_SF2000SD") {
            log::info!(
                "sf2000sd wr {off:#06X}/{size} <- {value:#06X} (enabled={})",
                self.enabled
            );
        }
        // Any write anywhere in the window latches the interface live, and
        // itself lands as a genuine register write in the same call --
        // matching `IdeZorro::write`'s RIPPLE/RIDE latch handling.
        self.enabled = true;
        let local = off & 0x1F;
        if local & 0x10 != 0 {
            self.write_data_port(local, size, value);
        } else {
            self.write_fixed((local >> 1) & 0x7, size, value);
        }
    }
}

impl crate::zorro_device::ZorroDevice for Sf2000Sd {
    fn read(&mut self, off: u32, size: usize, _host: &mut crate::zorro_device::DeviceHost) -> u32 {
        Self::read(self, off, size)
    }

    fn write(
        &mut self,
        off: u32,
        size: usize,
        value: u32,
        _host: &mut crate::zorro_device::DeviceHost,
    ) {
        Self::write(self, off, size, value)
    }

    fn tick(&mut self, _cck: u32, _host: &mut crate::zorro_device::DeviceHost) {
        // Register access alone drives the SPI/card state machine; there is
        // no time-driven work (no hot-swap, no debounce -- see the module
        // documentation).
    }

    fn int2_line(&self) -> bool {
        Self::int2_line(self)
    }

    fn take_activity(&mut self) -> bool {
        Self::take_activity(self)
    }

    fn reset(&mut self) {
        Self::reset(self)
    }

    fn kind(&self) -> &'static str {
        Self::kind(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diskimage::FileSystem;
    use crate::harddrive::HardDriveImage;

    fn temp_image(sectors: u64) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "copperline-sf2000sd-test-{}-{}.img",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, vec![0u8; (sectors * 512) as usize]).unwrap();
        path
    }

    fn board_with_card(sectors: u64) -> (Sf2000Sd, std::path::PathBuf) {
        let path = temp_image(sectors);
        let image =
            HardDriveImage::open(&path, "DH0", "sf2000sd", None, 0, FileSystem::FFS).unwrap();
        (
            Sf2000Sd::new(Vec::new(), Some(SdCard::new(image))).unwrap(),
            path,
        )
    }

    #[test]
    fn card_det_reflects_attachment() {
        let mut empty = Sf2000Sd::new(Vec::new(), None).unwrap();
        assert_eq!(empty.read(0x04, 1), 0);
        let (mut fitted, path) = board_with_card(16);
        assert_eq!(fitted.read(0x04, 1), 1);
        std::fs::remove_file(&path).ok();
    }

    /// Verified against the real `spisd2` firmware's `bootrom/bootldr.S` +
    /// `bootrom/mungerom.py`: the flash image sits on the odd byte lane at
    /// stride 2 (`window[2k+1] = rom[k]`, even lane floats), matching
    /// `ide_zorro.rs`'s AT-Bus 2008 personality -- not a flat byte-per-offset
    /// overlay. `bootldr.S`'s own relocation code confirms the stride: it
    /// computes the driver payload's window offset as the flash-file offset
    /// "times 4 (nibble-wise DiagArea)", where one factor of 2 is
    /// `mungerom.py`'s own nibble-doubling of the DiagArea/bootstrap portion
    /// (baked into the ROM file's content, nothing Copperline does) and the
    /// other factor of 2 is this lane stride.
    #[test]
    fn rom_lives_on_the_odd_lane_at_stride_2_until_the_first_write_latches_registers_live() {
        let mut rom = vec![0xFFu8; 0x8000]; // a full-size, 32K image
        rom[0] = 0x42; // window offset 1 (er_InitDiagVec) = rom[0]
        rom[0x7FFF] = 0x99; // window offset 0xFFFF = rom[0x7FFF]: spans the whole window
        let mut board = Sf2000Sd::new(rom, None).unwrap();

        assert_eq!(board.read(0x0001, 1), 0x42, "ROM at the DiagArea offset");
        assert_eq!(
            board.read(0xFFFF, 1),
            0x99,
            "the odd lane spans the whole window"
        );
        // The even lane floats throughout -- nothing drives it pre-latch.
        assert_eq!(board.read(0x0000, 1), 0xFF, "even lane floats, not ROM");
        assert_eq!(board.read(0x0002, 1), 0xFF, "even lane floats, not ROM");
        // A word access never exposes ROM, even where it exists on the odd
        // lane -- matching AT-Bus 2008's own precedent.
        assert_eq!(board.read(0x0000, 2), 0xFFFF);

        // Any write anywhere latches the interface live, and that same
        // write also lands as a genuine register write.
        board.write(0x00, 1, 0x77);
        assert_eq!(board.read(0x00, 1), 0x77, "CLKDIV now live");
        // ROM is gone everywhere, including bytes never written to.
        assert_eq!(
            board.read(0xFFFF, 1),
            0xFF,
            "no ROM left once registers are mapped (odd lane floats post-latch too)"
        );

        // Hardware-only mode (no ROM configured): registers are live from
        // power-on, nothing to latch.
        let mut hw_only = Sf2000Sd::new(Vec::new(), None).unwrap();
        assert_eq!(
            hw_only.read(0x04, 1),
            0,
            "CARD_DET, not ROM, with no rom fitted"
        );
    }

    /// Mirrors `spisd2`'s real `spi.c`: `spi_read`'s word-at-a-time loop
    /// polls STATUS_RX_CB_FULL before every word, expecting it to mean "at
    /// least 2 bytes are ready" -- not "the queue is at its full 34-byte
    /// capacity", a condition continuous draining essentially never
    /// reaches. Regression test for exactly the hang this caused against
    /// the real driver (verified against `~/git/spisd2` while diagnosing a
    /// live boot with the user).
    #[test]
    fn rx_cb_full_means_a_word_is_ready_not_that_the_queue_is_at_capacity() {
        let mut board = Sf2000Sd::new(Vec::new(), None).unwrap();
        // Arm a 4-byte receive -- e.g. sd_open()'s "96 dummy clocks" reads
        // 4 bytes at a time via spi_read, well under the queue's 34-byte
        // capacity, so it would never fill the queue to capacity while
        // being drained concurrently.
        board.write(0x08, 2, 0x4000 | 4); // SHIFT_CTRL: mode=RX, length=4
        for _ in 0..2 {
            let mut spins = 0;
            while board.read(0x06, 1) & 0x02 == 0 {
                spins += 1;
                assert!(
                    spins < 1000,
                    "STATUS_RX_CB_FULL never set: driver would hang here"
                );
            }
            board.read(0x10, 2); // word read, draining 2 bytes at once
        }
        assert_eq!(board.read(0x06, 1) & 0x40, 0, "not busy once fully drained");
    }

    #[test]
    fn mirrored_register_decode_across_the_window() {
        let mut board = Sf2000Sd::new(Vec::new(), None).unwrap();
        board.write(0x00, 1, 0x42);
        for mirror in [0x00u32, 0x20, 0x40, 0xFFE0] {
            assert_eq!(
                board.read(mirror, 1),
                0x42,
                "CLKDIV mirror at {mirror:#06X}"
            );
        }
    }

    /// Drives a full CMD0 init and a single-block write+read through the
    /// register interface exactly as a driver would: SHIFT_CTRL to arm a
    /// direction, data-port writes/reads to move bytes, STATUS/busy polling
    /// to pace it -- proving the register layer and `SdCard` work together,
    /// not just each in isolation.
    ///
    /// A byte-sized data-port access carries the byte itself in the low 8
    /// bits of `value` regardless of lane (matching `ide_zorro.rs`'s
    /// task-file convention) -- `off`'s parity, not `value`'s high byte,
    /// says which lane a mirrored register lives on.
    #[test]
    fn end_to_end_command_and_block_round_trip_through_registers() {
        let (mut board, path) = board_with_card(64);

        // Arm TX mode and clock out CMD0 (6 bytes), discarding replies.
        board.write(0x08, 2, 0x8000); // mode=TX (10), length irrelevant
        for &b in &[0x40u8, 0x00, 0x00, 0x00, 0x00, 0x95] {
            board.write(0x10, 1, u32::from(b));
        }

        // Switch to RX mode and pull the R1 byte back out. The request (1
        // byte) fits entirely within the RX queue's capacity, so it is
        // filled synchronously within the write itself -- `busy` only stays
        // observably true across a request larger than that capacity (SPI
        // bit timing isn't modeled; see the module documentation).
        board.write(0x08, 2, 0x4001); // mode=RX (01), length=1
        assert_eq!(board.read(0x10, 1), 0x01, "CMD0 -> R1 idle");
        assert_eq!(board.read(0x06, 1) & 0x40, 0, "not busy once drained");

        // APP_CMD + ACMD41: clear idle, same as any real driver's init.
        board.write(0x08, 2, 0x8000); // TX: CMD55
        for &b in &[0x77u8, 0x00, 0x00, 0x00, 0x00, 0x00] {
            board.write(0x10, 1, u32::from(b));
        }
        board.write(0x08, 2, 0x4001); // RX: CMD55's R1
        assert_eq!(board.read(0x10, 1), 0x01);
        board.write(0x08, 2, 0x8000); // TX: ACMD41 with HCS
        for &b in &[0x69u8, 0x40, 0x00, 0x00, 0x00, 0x00] {
            board.write(0x10, 1, u32::from(b));
        }
        board.write(0x08, 2, 0x4001); // RX: ACMD41's R1
        assert_eq!(board.read(0x10, 1), 0x00, "ACMD41 -> R1 not idle");

        // WRITE_BLOCK to LBA 3.
        board.write(0x08, 2, 0x8000); // TX: send CMD24
        for &b in &[0x58u8, 0x00, 0x00, 0x00, 0x03, 0x00] {
            board.write(0x10, 1, u32::from(b));
        }
        board.write(0x08, 2, 0x4001); // RX: CMD24's R1
        assert_eq!(board.read(0x10, 1), 0x00);

        board.write(0x08, 2, 0x8000); // TX: data token + block + CRC
        board.write(0x10, 1, 0xFE);
        let mut pattern = [0u8; 512];
        for (i, b) in pattern.iter_mut().enumerate() {
            *b = (i % 200) as u8;
        }
        for &b in &pattern {
            board.write(0x10, 1, u32::from(b));
        }
        board.write(0x10, 1, 0x00); // CRC hi
        board.write(0x10, 1, 0x00); // CRC lo

        // RX mode: pull the data-response token, then poll busy bytes until
        // the card leaves the busy state.
        board.write(0x08, 2, 0x4001);
        assert_eq!(board.read(0x10, 1) & 0x1F, 0x05, "data accepted token");
        let mut ready = false;
        for _ in 0..16 {
            board.write(0x08, 2, 0x4001);
            if board.read(0x10, 1) == 0xFF {
                ready = true;
                break;
            }
        }
        assert!(ready, "card must leave the busy state");

        // READ_SINGLE_BLOCK from LBA 3 must return the pattern just written.
        board.write(0x08, 2, 0x8000); // TX: send CMD17
        for &b in &[0x51u8, 0x00, 0x00, 0x00, 0x03, 0x00] {
            board.write(0x10, 1, u32::from(b));
        }
        // RX: R1 + start token + 512 data bytes + 2 CRC bytes in one
        // request (516 bytes, comfortably over the 34-byte RX capacity, so
        // this exercises the busy/top-up backpressure path for real).
        board.write(0x08, 2, 0x4000 | 516);
        assert_ne!(
            board.read(0x06, 1) & 0x40,
            0,
            "busy: most of the block is still outstanding"
        );
        assert_eq!(board.read(0x10, 1), 0x00, "R1");
        assert_eq!(board.read(0x10, 1), 0xFE, "start token");
        let mut read_back = [0u8; 512];
        for b in &mut read_back {
            *b = board.read(0x10, 1) as u8;
        }
        assert_eq!(board.read(0x10, 1), 0x00); // CRC hi
        assert_eq!(board.read(0x10, 1), 0x00); // CRC lo
        assert_eq!(
            board.read(0x06, 1) & 0x40,
            0,
            "not busy once the whole block is drained"
        );
        assert_eq!(read_back, pattern);
        std::fs::remove_file(&path).ok();
    }

    /// Mirrors `spisd2`'s real `spi.c` `spi_read()` exactly: one SHIFT_CTRL
    /// covering the whole request, then odd-byte alignment, 16-byte chunks
    /// via 4 back-to-back longword (size-4) reads gated on
    /// STATUS_RX_HALF_FULL, remaining words gated on STATUS_RX_CB_FULL, and
    /// a final byte gated on STATUS_RX_CB_EMPTY -- the exact mixed
    /// byte/word/longword access pattern real driver code uses, which the
    /// smaller hand-written tests above don't exercise (they only ever do
    /// one access size at a time).
    fn spi_read_via_registers(board: &mut Sf2000Sd, size: usize) -> Vec<u8> {
        board.write(0x08, 2, 0x4000 | (size as u32 & 0x1FFF));
        let mut buf = Vec::with_capacity(size);
        let mut remaining = size;
        // (buf is always even-aligned here, so the odd-alignment branch
        // never triggers -- matching a `uint8_t[512]` stack buffer.)
        let chunk_count = remaining >> 4;
        for _ in 0..chunk_count {
            let mut spins = 0;
            while board.read(0x06, 1) & 0x10 == 0 {
                spins += 1;
                assert!(spins < 10_000, "STATUS_RX_HALF_FULL never set");
            }
            for _ in 0..4 {
                let word = board.read(0x10, 4);
                buf.extend_from_slice(&word.to_be_bytes());
            }
        }
        remaining -= chunk_count << 4;
        for _ in 0..(remaining >> 1) {
            let mut spins = 0;
            while board.read(0x06, 1) & 0x02 == 0 {
                spins += 1;
                assert!(spins < 10_000, "STATUS_RX_CB_FULL never set");
            }
            let word = board.read(0x10, 2);
            buf.extend_from_slice(&(word as u16).to_be_bytes());
        }
        if remaining & 1 != 0 {
            let mut spins = 0;
            while board.read(0x06, 1) & 0x01 != 0 {
                spins += 1;
                assert!(spins < 10_000, "STATUS_RX_CB_EMPTY never clears");
            }
            buf.push(board.read(0x10, 1) as u8);
        }
        buf
    }

    /// Mirrors `spi_write()` just as precisely: one SHIFT_CTRL with no
    /// length (TX mode ignores it), 16-byte chunks via 4 longword writes
    /// gated on STATUS_TX_HALF_EMPTY, remaining words gated on
    /// STATUS_TX_CB_EMPTY, a final byte gated on STATUS_TX_CB_FULL, then a
    /// wait for STATUS_SHIFTER_BUSY to clear before returning.
    fn spi_write_via_registers(board: &mut Sf2000Sd, buf: &[u8]) {
        board.write(0x08, 2, 0x8000);
        let mut remaining = buf.len();
        let mut pos = 0;
        let chunk_count = remaining >> 4;
        for _ in 0..chunk_count {
            let mut spins = 0;
            while board.read(0x06, 1) & 0x20 == 0 {
                spins += 1;
                assert!(spins < 10_000, "STATUS_TX_HALF_EMPTY never set");
            }
            for _ in 0..4 {
                let word = u32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap());
                board.write(0x10, 4, word);
                pos += 4;
            }
        }
        remaining -= chunk_count << 4;
        for _ in 0..(remaining >> 1) {
            let mut spins = 0;
            while board.read(0x06, 1) & 0x04 == 0 {
                spins += 1;
                assert!(spins < 10_000, "STATUS_TX_CB_EMPTY never set");
            }
            let word = u16::from_be_bytes(buf[pos..pos + 2].try_into().unwrap());
            board.write(0x10, 2, u32::from(word));
            pos += 2;
        }
        if remaining & 1 != 0 {
            let mut spins = 0;
            while board.read(0x06, 1) & 0x08 != 0 {
                spins += 1;
                assert!(spins < 10_000, "STATUS_TX_CB_FULL never clears");
            }
            board.write(0x10, 1, u32::from(buf[pos]));
        }
        let mut spins = 0;
        while board.read(0x06, 1) & 0x40 != 0 {
            spins += 1;
            assert!(spins < 10_000, "STATUS_SHIFTER_BUSY never clears");
        }
    }

    /// Large-scale, register-level version of `sdcard.rs`'s own bulk-scale
    /// test: the `SdCard`-level test drives `clock_byte` directly, bypassing
    /// the FIFO/backpressure/STATUS-polling logic in this file entirely --
    /// exactly the layer the real STATUS_RX_CB_FULL bug lived in. This one
    /// drives the actual register interface with the real driver's precise
    /// mixed byte/word/longword access pattern (`spi_read_via_registers`/
    /// `spi_write_via_registers`), at a scale representative of a real
    /// bulk filesystem transfer (PFS3 in particular issues large HD_SCSICMD
    /// READ_10/WRITE_10 requests), to catch anything that only breaks once
    /// backpressure has cycled many times or a chunk boundary lands
    /// unusually.
    #[test]
    fn large_bulk_transfer_matches_the_real_drivers_mixed_access_pattern() {
        const BLOCKS: u64 = 64;
        let (mut board, path) = board_with_card(4096);
        let init = |board: &mut Sf2000Sd| {
            let frame = |cmd: u8, arg: u32, crc: u8| {
                let mut f = [0u8; 6];
                f[0] = 0x40 | (cmd & 0x3F);
                f[1..5].copy_from_slice(&arg.to_be_bytes());
                f[5] = crc;
                f
            };
            let send = |board: &mut Sf2000Sd, frame: [u8; 6], want: usize| -> Vec<u8> {
                board.write(0x08, 2, 0x8000);
                for b in frame {
                    board.write(0x10, 1, u32::from(b));
                }
                board.write(0x08, 2, 0x4000 | want as u32);
                (0..want).map(|_| board.read(0x10, 1) as u8).collect()
            };
            send(board, frame(0, 0, 0x95), 1);
            send(board, frame(55, 0, 0), 1);
            send(board, frame(41, 1 << 30, 0), 1);
        };
        init(&mut board);

        let pattern_byte =
            |block: u64, i: usize| -> u8 { ((block * 197 + i as u64 * 29) % 251) as u8 };

        // Write BLOCKS sectors via CMD25, matching sd_write_block's
        // 0xFC-token + spi_write(512) + spi_write(crc,2) structure, driven
        // through the real register access pattern.
        board.write(0x08, 2, 0x8000);
        for &b in &[0x59u8, 0x00, 0x00, 0x00, 0x64, 0x00] {
            board.write(0x10, 1, u32::from(b)); // CMD25 to LBA 100
        }
        board.write(0x08, 2, 0x4001);
        assert_eq!(board.read(0x10, 1), 0x00);
        for block in 0..BLOCKS {
            // Mirrors `sd_wait_ready()`, called at the top of every real
            // `sd_write_block()` (including the first): poll single bytes
            // until the card stops driving busy (0x00) and returns 0xFF.
            // This is also what drains the previous block's data-response
            // token plus its WRITE_BUSY_CLOCKS busy run before the next
            // block's start token is sent.
            let mut spins = 0;
            loop {
                board.write(0x08, 2, 0x4001);
                if board.read(0x10, 1) as u8 == 0xFF {
                    break;
                }
                spins += 1;
                assert!(spins < 1000, "block {block}: card never became ready");
            }
            board.write(0x08, 2, 0x8000);
            board.write(0x10, 1, 0xFC);
            let data: Vec<u8> = (0..512).map(|i| pattern_byte(block, i)).collect();
            spi_write_via_registers(&mut board, &data);
            spi_write_via_registers(&mut board, &[0x00, 0x00]); // CRC
            board.write(0x08, 2, 0x4001);
            assert_eq!(
                board.read(0x10, 1) & 0x1F,
                0x05,
                "block {block}: data accepted"
            );
        }
        // Same `sd_wait_ready()` poll before STOP_TRAN's own `sd_write_block`
        // call, draining the last block's data-response/busy run.
        let mut spins = 0;
        loop {
            board.write(0x08, 2, 0x4001);
            if board.read(0x10, 1) as u8 == 0xFF {
                break;
            }
            spins += 1;
            assert!(spins < 1000, "STOP_TRAN: card never became ready");
        }
        board.write(0x08, 2, 0x8000);
        board.write(0x10, 1, 0xFD); // STOP_TRAN
        board.write(0x08, 2, 0x4001);
        board.read(0x10, 1);

        // Read them all back via CMD18, matching sd_read_block's token-poll
        // + spi_read(512) + spi_read(crc,2) structure.
        board.write(0x08, 2, 0x8000);
        for &b in &[0x52u8, 0x00, 0x00, 0x00, 0x64, 0x00] {
            board.write(0x10, 1, u32::from(b)); // CMD18 from LBA 100
        }
        board.write(0x08, 2, 0x4001);
        assert_eq!(board.read(0x10, 1), 0x00);
        for block in 0..BLOCKS {
            let mut spins = 0;
            let token = loop {
                board.write(0x08, 2, 0x4001);
                let b = board.read(0x10, 1) as u8;
                if b != 0xFF {
                    break b;
                }
                spins += 1;
                assert!(spins < 1000, "block {block}: start token never arrived");
            };
            assert_eq!(token, 0xFE, "block {block}: start token");
            board.write(0x08, 2, 0x4000 | 512);
            let read_back = spi_read_via_registers(&mut board, 512);
            let expected: Vec<u8> = (0..512).map(|i| pattern_byte(block, i)).collect();
            assert_eq!(read_back, expected, "block {block} data mismatch");
            spi_read_via_registers(&mut board, 2); // CRC
        }
        let stop = [0x40u8 | 12, 0x00, 0x00, 0x00, 0x00, 0x01];
        board.write(0x08, 2, 0x8000);
        for b in stop {
            board.write(0x10, 1, u32::from(b));
        }
        board.write(0x08, 2, 0x4002);
        board.read(0x10, 1); // stuff byte
        assert_eq!(board.read(0x10, 1), 0x00, "R1 after STOP_TRANSMISSION");

        std::fs::remove_file(&path).ok();
    }

    /// Reproduces the exact shape of the multi-block reads a real PFS3
    /// mount captured against this board actually issues: a single,
    /// uninterrupted CMD18 stream of 256 consecutive sectors (one more than
    /// the 255-sector `de_MaxTransfer` this driver's callers commonly use,
    /// suspiciously landing right on a `2^8` boundary). Runs the stream to
    /// 300 blocks -- comfortably bracketing 255/256/257 on both sides -- to
    /// rule out an 8-bit counter/index wrapping at exactly this scale
    /// anywhere in the register/backpressure path, matching
    /// `spi-lib-sf2000/spi.c`'s real per-sector access pattern throughout
    /// (that file's own `spi_read`/`spi_write` were independently checked
    /// and found to carry no cross-call state that could accumulate to such
    /// a boundary -- this test is the register-emulation-side half of that
    /// check). The backing image is pre-seeded with a known pattern on disk
    /// directly, bypassing the write path (already covered elsewhere), so
    /// every one of the 300 blocks' content is verified, not just that the
    /// stream completes.
    #[test]
    fn continuous_read_stream_survives_well_past_the_real_drivers_256_sector_transfers() {
        const START_LBA: u64 = 2018; // matches the real capture's second stream
        const BLOCKS: u64 = 300;
        let pattern_byte =
            |block: u64, i: usize| -> u8 { ((block * 89 + i as u64 * 53) % 251) as u8 };

        let path = temp_image(START_LBA + BLOCKS + 8);
        {
            let mut disk = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            use std::io::{Seek, SeekFrom, Write};
            for block in 0..BLOCKS {
                let data: Vec<u8> = (0..512).map(|i| pattern_byte(block, i)).collect();
                disk.seek(SeekFrom::Start((START_LBA + block) * 512))
                    .unwrap();
                disk.write_all(&data).unwrap();
            }
        }
        let image =
            HardDriveImage::open(&path, "DH0", "sf2000sd", None, 0, FileSystem::FFS).unwrap();
        let mut board = Sf2000Sd::new(Vec::new(), Some(SdCard::new(image))).unwrap();

        let frame = |cmd: u8, arg: u32, crc: u8| {
            let mut f = [0u8; 6];
            f[0] = 0x40 | (cmd & 0x3F);
            f[1..5].copy_from_slice(&arg.to_be_bytes());
            f[5] = crc;
            f
        };
        let send = |board: &mut Sf2000Sd, frame: [u8; 6], want: usize| -> Vec<u8> {
            board.write(0x08, 2, 0x8000);
            for b in frame {
                board.write(0x10, 1, u32::from(b));
            }
            board.write(0x08, 2, 0x4000 | want as u32);
            (0..want).map(|_| board.read(0x10, 1) as u8).collect()
        };
        send(&mut board, frame(0, 0, 0x95), 1);
        send(&mut board, frame(55, 0, 0), 1);
        send(&mut board, frame(41, 1 << 30, 0), 1);

        let r1 = send(&mut board, frame(18, START_LBA as u32, 0), 1); // CMD18
        assert_eq!(r1, [0x00]);

        for block in 0..BLOCKS {
            let mut spins = 0;
            let token = loop {
                board.write(0x08, 2, 0x4001);
                let b = board.read(0x10, 1) as u8;
                if b != 0xFF {
                    break b;
                }
                spins += 1;
                assert!(spins < 1000, "block {block}: start token never arrived");
            };
            assert_eq!(token, 0xFE, "block {block}: start token");
            board.write(0x08, 2, 0x4000 | 512);
            let read_back = spi_read_via_registers(&mut board, 512);
            let expected: Vec<u8> = (0..512).map(|i| pattern_byte(block, i)).collect();
            assert_eq!(read_back, expected, "block {block} data mismatch");
            spi_read_via_registers(&mut board, 2); // CRC
        }

        let stop = frame(12, 0, 0x01);
        board.write(0x08, 2, 0x8000);
        for b in stop {
            board.write(0x10, 1, u32::from(b));
        }
        board.write(0x08, 2, 0x4002);
        board.read(0x10, 1); // stuff byte
        assert_eq!(board.read(0x10, 1), 0x00, "R1 after STOP_TRANSMISSION");

        std::fs::remove_file(&path).ok();
    }
}
