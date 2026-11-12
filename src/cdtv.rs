// SPDX-License-Identifier: GPL-3.0-or-later

//! CDTV DMAC / CD-ROM controller.
//!
//! The CDTV's CD subsystem is a Zorro II autoconfig board (Commodore
//! product 3) that the ROM maps during autoconfig (conventionally at
//! $E90000). The 64 KiB window carries, per WinUAE's cdtv.cpp (the
//! reference for this hardware):
//!
//! - +$00..$3F  the board's autoconfig ROM, still readable when mapped;
//! - +$41/$43   DMAC ISTR (interrupt status) / CNTR (control);
//! - +$80..$87  DMAC WTC (word transfer count) and ACR (address);
//! - +$A1       the Matshita drive's command/response byte port;
//! - +$B0..$BF  a 6525 TPI (tri-port interface) whose port C carries
//!   the drive handshake lines: SBCP (subcode byte), SCOR (subcode
//!   frame), STCH (status change), STEN (status byte available); port
//!   B drives CMD/ENABLE/XAEN/DTEN and the DAC volume strobes;
//! - +$E0/$E2/$E4  DMA start / stop / clear-interrupt strobes.
//!
//! Drive commands are fixed-length byte strings (no checksum): seek,
//! read, motor, play (LSN/MSF/track), status, error, model, mode
//! set/sense, capacity, SubQ, info, TOC, pause, front panel. Responses
//! return one byte at a time through +$A1, with STEN pulsing per byte.
//! Data sectors DMA onto the system bus at the ACR address, paced at
//! single speed; completion raises the DMAC E_INT interrupt. The DMAC is
//! a 24-bit bus master like the A2091's: targets can be chip RAM, slow
//! RAM, or Zorro II board RAM (the CD driver allocates CD buffers in
//! fast RAM when a board is fitted).
//!
//! CD audio streams into the shared host mixer ring like the CD32's
//! Akiko.
//!
//! The drive's subcode output is wired to the 6525: SCOR marks the start
//! of each subcode frame and SBCP clocks its bytes into port A. While a
//! sector plays, its 96-byte frame (`CdImage::subcode_frame`: the
//! position in Q, and R-W when the image stored them, which is what CD+G
//! is made of) follows SCOR one byte per subcode symbol period -- 98
//! symbols to the 1/75 s frame, so 7350 a second. SBCP rises with each
//! byte and falls when port A is read; a byte nobody read is replaced by
//! the next, as the shift register would. Port A presents the symbol
//! bit-reversed (P in bit 0 ... W in bit 7), per the reference. With
//! nothing playing SCOR still pulses at 75 Hz and SBCP stays quiet.
//!
//! Not verified against hardware or a real CD+G disc: the port A bit
//! order, the symbol-per-byte layout of stored subcode, and what a paused
//! drive does with SCOR (see `reset_subcode`). A frame is produced only
//! for a sector that actually plays, so while the host mixer's ring is
//! full and the play is held back, subcode is held back with it.
//!
//! The battery-backed bookmark RAM at $DC8000 (16 KiB) lives here too. It
//! persists to a raw image file when one is configured (`[cd] nvram`),
//! written a quarter of an emulated second after a change, on reset, and
//! when the controller is dropped.

use crate::cdrom::{to_bcd, CdImage, LEADIN_SECTORS, RAW_SECTOR_BYTES, SUBCODE_FRAME_BYTES};
use crate::chipset::paula::CdAudioRing;
use crate::memory::Memory;

// DMAC CNTR bits.
const CNTR_TCEN: u8 = 1 << 7;
const CNTR_PREST: u8 = 1 << 6;
const CNTR_PDMD: u8 = 1 << 5;
const CNTR_INTEN: u8 = 1 << 4;
#[allow(dead_code)]
const CNTR_DDIR: u8 = 1 << 3;

// DMAC ISTR bits.
const ISTR_INT_P: u8 = 1 << 4;
const ISTR_E_INT: u8 = 1 << 5;
const ISTR_FE_FLG: u8 = 1 << 0;

const MODEL_NAME: &[u8] = b"MATSHITA0.96";

// CD audio status values (SCSI-style, as cdtv.device expects).
const AUDIO_STATUS_IN_PROGRESS: u8 = 0x11;
const AUDIO_STATUS_PAUSED: u8 = 0x12;
const AUDIO_STATUS_PLAY_COMPLETE: u8 = 0x13;
const AUDIO_STATUS_NOT_SUPPORTED: u8 = 0x00;
const AUDIO_STATUS_NO_STATUS: u8 = 0x15;

/// Colour clocks per scanline (the reference paces in hsyncs).
const CCK_PER_LINE: u32 = 227;
/// Colour clocks per 1/75th second (one single-speed CD frame).
const CCK_PER_CD_FRAME: u32 = crate::chipset::paula::PAULA_CLOCK_HZ / 75;

/// Colour clocks per subcode symbol: a sector's 98 symbols (two sync
/// patterns and 96 data bytes) span one CD frame.
const CCK_PER_SUBCODE_SYMBOL: u32 = CCK_PER_CD_FRAME / 98;
/// Size of the battery-backed bookmark RAM, and of its backing file.
pub const BOOKMARK_RAM_BYTES: usize = 16 * 1024;
/// How long a change to the bookmark RAM waits before it is written to the
/// backing file: a quarter of an emulated second. The RAM is plain SRAM with
/// no transaction to mark the end of a save, so the file follows the first
/// unwritten change after this long -- one host write for a burst of guest
/// writes, and at most four a second from a guest that never stops.
const BOOKMARK_FLUSH_CCK: u32 = crate::chipset::paula::PAULA_CLOCK_HZ / 4;

fn lsn_to_msf(lsn: u32) -> u32 {
    let msf = lsn + LEADIN_SECTORS;
    ((msf / (60 * 75)) << 16) | (((msf / 75) % 60) << 8) | (msf % 75)
}

fn msf_to_lsn(msf: u32) -> u32 {
    let m = (msf >> 16) & 0xFF;
    let s = (msf >> 8) & 0xFF;
    let f = msf & 0xFF;
    ((m * 60 + s) * 75 + f).saturating_sub(LEADIN_SECTORS)
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct CdtvController {
    /// Autoconfig: base address once the ROM configures the board.
    configured_base: Option<u32>,
    shut_up: bool,
    #[serde(with = "serde_big_array::BigArray")]
    config_rom: [u8; 0x40],

    // DMAC.
    istr: u8,
    cntr: u8,
    wtc: u32,
    acr: u32,
    dawr: u16,
    dma_on: bool,
    /// One-time warning latch for DMA to an unmapped bus address.
    dma_warned: bool,
    /// Pending DMA-complete interrupt, modelled as a colour-clock delay
    /// matching the single-speed transfer time.
    dma_done_delay_cck: i64,

    // 6525 TPI.
    tp_a: u8,
    tp_b: u8,
    tp_c: u8,
    tp_ad: u8,
    tp_bd: u8,
    tp_cd: u8,
    tp_imask: u8,
    tp_cr: u8,
    tp_air: u8,
    tp_ilatch: u8,
    tp_ilatch2: u8,

    // Handshake lines. STEN uses the reference's small negative
    // countdown so each response byte produces one clean edge.
    stch: i8,
    sten: i8,
    scor: i8,
    sbcp: i8,
    activate_stch: bool,

    // Subcode output.
    /// The frame of the sector that just played, still being clocked out
    /// (96 symbols); empty when no frame is in transit.
    #[serde(default)]
    sub_frame: Vec<u8>,
    /// The next symbol of `sub_frame` to reach the shift register.
    #[serde(default)]
    sub_index: u8,
    /// Colour clocks until that symbol has been assembled.
    #[serde(default)]
    sub_symbol_cck: i32,
    /// The symbol last assembled: what port A reads.
    #[serde(default)]
    sub_latch: u8,

    // Drive command channel.
    command_in: [u8; 16],
    command_cnt_in: usize,
    command_out: [u8; 16],
    command_cnt_out: i32,
    command_size_out: usize,
    command_done: bool,

    // Drive/disc state.
    disc: Option<CdImage>,
    cd_media: bool,
    cd_motor: bool,
    cd_playing: bool,
    cd_paused: bool,
    cd_error: bool,
    cd_finished: bool,
    cd_isready: bool,
    sector_size: u32,

    // Data read state.
    read_offset: u64,
    read_length: u64,

    // Audio playback (streams into the host mixer ring).
    play_position: i64,
    play_end: i64,
    last_play_pos: i64,
    audio_status: u8,
    flush_cd_audio: bool,

    // Pacing.
    line_counter_cck: i32,
    lines_until_frame: i32,
    audio_counter_cck: i32,
    booted: bool,

    /// Battery-backed bookmark RAM at $DC8000 (16 KiB, mirrored across
    /// the $DC8000-$DCFFFF window).
    battram: Vec<u8>,
    /// Backing file for the bookmark RAM (`[cd] nvram` on a CDTV): a raw
    /// image of the 16 KiB. `None` keeps it session-only, like a player
    /// with a flat battery. The file belongs to the running session, not
    /// to the machine, so it is not part of a save state (see
    /// [`Self::adopt_bookmark_file_from`]).
    #[serde(skip)]
    battram_path: Option<std::path::PathBuf>,
    /// The bookmark RAM differs from what the backing file holds. Like
    /// the file itself this is the session's, not the machine's.
    #[serde(skip)]
    battram_dirty: bool,
    /// Colour clocks since the first change the file does not hold yet.
    #[serde(skip)]
    battram_dirty_cck: u32,

    /// A disc waiting in the tray: inserted (media-change STCH) once
    /// `insert_delay_cck` of emulated time has elapsed. Some discs only
    /// boot when inserted after the CDTV boot screen (verified against
    /// FS-UAE's delayed-insert option and real A570 behaviour).
    pending_disc: Option<CdImage>,
    insert_delay_cck: i64,
}

impl CdtvController {
    pub fn new() -> Self {
        let mut controller = Self {
            configured_base: None,
            shut_up: false,
            config_rom: [0xFF; 0x40],
            istr: 0,
            cntr: 0,
            wtc: 0,
            acr: 0,
            dawr: 0,
            dma_on: false,
            dma_warned: false,
            dma_done_delay_cck: -1,
            tp_a: 0,
            tp_b: 0,
            tp_c: 0,
            tp_ad: 0,
            tp_bd: 0,
            tp_cd: 0,
            tp_imask: 0,
            tp_cr: 0,
            tp_air: 0,
            tp_ilatch: 0,
            tp_ilatch2: 0,
            stch: 0,
            sten: 0,
            scor: 0,
            sbcp: 0,
            activate_stch: false,
            sub_frame: Vec::new(),
            sub_index: 0,
            sub_symbol_cck: 0,
            sub_latch: 0,
            command_in: [0; 16],
            command_cnt_in: 0,
            command_out: [0; 16],
            command_cnt_out: -1,
            command_size_out: 0,
            command_done: false,
            disc: None,
            cd_media: false,
            cd_motor: false,
            cd_playing: false,
            cd_paused: false,
            cd_error: false,
            cd_finished: false,
            cd_isready: false,
            sector_size: 2048,
            read_offset: 0,
            read_length: 0,
            play_position: 0,
            play_end: 0,
            last_play_pos: 0,
            audio_status: AUDIO_STATUS_NO_STATUS,
            flush_cd_audio: false,
            line_counter_cck: 0,
            lines_until_frame: 0,
            audio_counter_cck: 0,
            booted: false,
            battram: vec![0u8; BOOKMARK_RAM_BYTES],
            battram_path: None,
            battram_dirty: false,
            battram_dirty_cck: 0,
            pending_disc: None,
            insert_delay_cck: -1,
        };
        controller.build_config_rom();
        controller
    }

    pub fn insert_disc(&mut self, disc: CdImage) {
        self.disc = Some(disc);
        self.cd_media = true;
        self.read_offset = 0;
        self.read_length = 0;
        self.play_position = 0;
        self.play_end = 0;
        self.last_play_pos = 0;
    }

    /// Park a disc in the tray and insert it (with the media-change
    /// status interrupt) after `secs` of emulated time.
    pub fn insert_disc_after(&mut self, disc: CdImage, secs: f64) {
        self.pending_disc = Some(disc);
        self.insert_delay_cck =
            (secs.max(0.0) * f64::from(crate::chipset::paula::PAULA_CLOCK_HZ)) as i64;
    }

    /// Whether a disc is mounted or waiting in the tray.
    pub fn has_disc(&self) -> bool {
        self.disc.is_some() || self.pending_disc.is_some()
    }

    /// The disc that is mounted or waiting in the tray, for naming it.
    pub fn disc_ref(&self) -> Option<&CdImage> {
        self.disc.as_ref().or(self.pending_disc.as_ref())
    }

    /// `disk.fault.*`'s `cd` target: the actually-mounted disc only, not
    /// one still pending in the tray (not yet readable, so there is
    /// nothing for a fault policy to act on).
    pub fn disc_mut(&mut self) -> Option<&mut CdImage> {
        self.disc.as_mut()
    }

    /// Whether the drive is actively working: streaming CD audio, or a
    /// data read in flight (sectors still owed, DMA running, or the
    /// completion interrupt pending). Feeds the status-bar CD LED.
    pub fn activity_led_on(&self) -> bool {
        (self.cd_playing && !self.cd_paused)
            || self.read_length > 0
            || self.dma_on
            || self.dma_done_delay_cck >= 0
    }

    /// Track under the emulated optical head, or `None` with no mounted
    /// medium. Data reads and CD-DA playback both advance `last_play_pos`,
    /// so the front panel reports the controller position rather than host
    /// elapsed time.
    pub fn current_track(&self) -> Option<u8> {
        self.disc.as_ref()?;
        Some(self.track_for(self.last_play_pos.max(0) as u32).0)
    }

    /// Remove the disc (and any disc still waiting in the tray): stop
    /// playback, drop buffered audio, and raise the media-change status
    /// interrupt so cdtv.device notices the removal.
    pub fn eject_disc(&mut self) {
        self.pending_disc = None;
        self.insert_delay_cck = -1;
        let had_media = self.disc.take().is_some() || self.cd_media;
        if !had_media {
            return;
        }
        self.stop_audio();
        self.cd_media = false;
        self.cd_motor = false;
        self.read_length = 0;
        self.audio_status = AUDIO_STATUS_NO_STATUS;
        self.activate_stch = true;
        log::info!("cdtv: disc ejected, media-change STCH raised");
    }

    /// Reset on system reset; keeps the drive state (mounted or still
    /// pending disc) and the battery-backed bookmark RAM contents. A
    /// change the backing file does not hold yet is written first: the
    /// guest that was saving is gone, so nothing more is coming.
    pub fn reset(&mut self) {
        self.flush_bookmark_ram();
        let disc = self.disc.take();
        let pending = self.pending_disc.take();
        let delay = self.insert_delay_cck;
        let battram = std::mem::take(&mut self.battram);
        // Taken, so the value this replaces has no file left to write its
        // (now empty) RAM to when it drops.
        let battram_path = self.battram_path.take();
        let battram_dirty = self.battram_dirty;
        let battram_dirty_cck = self.battram_dirty_cck;
        *self = Self::new();
        self.battram = battram;
        self.battram_path = battram_path;
        self.battram_dirty = battram_dirty;
        self.battram_dirty_cck = battram_dirty_cck;
        if let Some(disc) = disc {
            self.insert_disc(disc);
        }
        self.pending_disc = pending;
        self.insert_delay_cck = delay;
    }

    /// Battery-backed RAM window at $DC8000-$DCFFFF.
    pub fn battram_read(&self, addr: u32, size: usize) -> u32 {
        let mut value = 0u32;
        for i in 0..size as u32 {
            let off = ((addr + i) as usize) & (self.battram.len() - 1);
            value = (value << 8) | u32::from(self.battram[off]);
        }
        value
    }

    pub fn battram_write(&mut self, addr: u32, size: usize, value: u32) {
        for i in 0..size as u32 {
            let shift = 8 * (size as u32 - 1 - i);
            let off = ((addr + i) as usize) & (self.battram.len() - 1);
            let byte = (value >> shift) as u8;
            // Only a real change dirties the file: the bookmark library
            // rewrites cells with what they already hold.
            if self.battram[off] != byte {
                self.battram[off] = byte;
                self.battram_dirty = true;
            }
        }
    }

    /// Persist the bookmark RAM to (and preload it from) `path`, a raw
    /// image of the 16 KiB. A shorter file fills the start of the RAM and
    /// leaves the rest clear; a longer one gives its first 16 KiB.
    pub fn set_bookmark_path(&mut self, path: std::path::PathBuf) {
        match std::fs::read(&path) {
            Ok(data) => {
                let n = data.len().min(self.battram.len());
                self.battram.fill(0);
                self.battram[..n].copy_from_slice(&data[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("cdtv bookmark RAM: reading {}: {e}", path.display()),
        }
        self.battram_path = Some(path);
        self.battram_dirty = false;
        self.battram_dirty_cck = 0;
    }

    /// Whether the bookmark RAM is backed by a host file. Its changes
    /// reach that file on their own schedule, which restoring a
    /// speculative machine snapshot cannot undo.
    pub fn persistent_bookmark_ram(&self) -> bool {
        self.battram_path.is_some()
    }

    /// The bookmark RAM's contents.
    pub fn bookmark_ram(&self) -> &[u8] {
        &self.battram
    }

    /// Take over the running session's backing file after a state load:
    /// `self` is the freshly restored controller, `live` the one it
    /// replaces. The file is host configuration, so the session keeps the
    /// one it was started with whatever machine the state was saved on,
    /// and the restored RAM is what it must now hold -- the next flush
    /// writes it. Anything `live` had not written yet belongs to the
    /// timeline being abandoned and is dropped with it.
    pub(crate) fn adopt_bookmark_file_from(&mut self, live: &mut CdtvController) {
        self.battram_path = live.battram_path.take();
        self.battram_dirty =
            self.battram_path.is_some() && (live.battram_dirty || self.battram != live.battram);
        self.battram_dirty_cck = 0;
    }

    /// Write the bookmark RAM to its backing file if it has changed.
    fn flush_bookmark_ram(&mut self) {
        if !self.battram_dirty {
            return;
        }
        self.battram_dirty_cck = 0;
        if let Some(path) = &self.battram_path {
            // The directory is Copperline's own and may not exist yet; it
            // is made here rather than at bind time so a player that never
            // saves leaves no empty folder behind.
            if let Err(e) =
                crate::paths::ensure_parent(path).and_then(|()| std::fs::write(path, &self.battram))
            {
                // Stay dirty so the next interval retries: a transient
                // host error must not lose a save until the guest happens
                // to write the RAM again.
                log::warn!("cdtv bookmark RAM: writing {}: {e}", path.display());
                return;
            }
        }
        self.battram_dirty = false;
    }

    /// The INT2 (PORTS) line: the 6525's interrupt output plus the
    /// DMAC's enabled end-of-process interrupt, level-fed like Gayle's.
    pub fn int2_line(&self) -> bool {
        if self.tp_ilatch & (1 << 5) != 0 {
            return true;
        }
        // Port C as plain I/O can drive the line directly.
        if self.tp_cr & 1 == 0 && self.tp_c & (1 << 5) != 0 {
            return true;
        }
        self.cntr & CNTR_INTEN != 0 && self.istr & ISTR_E_INT != 0
    }

    // ----- autoconfig ------------------------------------------------------

    /// In the configuration window until configured or shut up.
    pub fn in_config_space(&self) -> bool {
        self.configured_base.is_none() && !self.shut_up
    }

    /// The 64 KiB window this board owns once configured.
    pub fn maps_address(&self, addr: u32) -> bool {
        self.configured_base
            .is_some_and(|base| addr.wrapping_sub(base) < 0x1_0000)
    }

    fn build_config_rom(&mut self) {
        // Per the reference: type $C1 (Zorro II, 64K), product 3,
        // flags $40, manufacturer $0202 (Commodore), serial 0. The
        // special offsets ($00/$40 families) are stored uninverted.
        let mut ew = |addr: usize, value: u8, invert: bool| {
            let hi = value & 0xF0;
            let lo = (value & 0x0F) << 4;
            if invert {
                self.config_rom[addr] = !hi;
                self.config_rom[addr + 2] = !lo;
            } else {
                self.config_rom[addr] = hi;
                self.config_rom[addr + 2] = lo;
            }
        };
        ew(0x00, 0xC1, false);
        ew(0x04, 0x03, true);
        ew(0x08, 0x40, true);
        ew(0x10, 0x02, true);
        ew(0x14, 0x02, true);
        ew(0x18, 0x00, true);
        ew(0x1C, 0x00, true);
        ew(0x20, 0x00, true);
        ew(0x24, 0x00, true);
    }

    pub fn config_read(&self, addr: u32, size: usize) -> u32 {
        let mut value = 0u32;
        for i in 0..size as u32 {
            let off = ((addr + i) & 0xFFFF) as usize;
            let byte = if off < 0x40 {
                self.config_rom[off]
            } else {
                0xFF
            };
            value = (value << 8) | u32::from(byte);
        }
        value
    }

    pub fn config_write(&mut self, addr: u32, size: usize, value: u32) {
        let off = addr & 0xFFFF;
        let byte = (value >> (8 * (size as u32 - 1))) as u8;
        match off {
            0x48 => {
                let base = u32::from(byte) << 16;
                self.configured_base = Some(base);
                log::info!("cdtv: DMAC autoconfigured at {base:#010X}");
            }
            0x4C => {
                self.shut_up = true;
                log::warn!("cdtv: DMAC shut up by ROM autoconfig");
            }
            _ => {}
        }
    }

    // ----- mapped window ---------------------------------------------------

    pub fn read(&mut self, addr: u32, size: usize) -> u32 {
        let mut value = 0u32;
        for i in 0..size as u32 {
            value = (value << 8) | u32::from(self.read_byte((addr + i) & 0xFFFF));
        }
        value
    }

    pub fn write(&mut self, addr: u32, size: usize, value: u32, mem: &mut Memory) {
        for i in 0..size as u32 {
            let shift = 8 * (size as u32 - 1 - i);
            self.write_byte((addr + i) & 0xFFFF, (value >> shift) as u8, mem);
        }
    }

    fn read_byte(&mut self, addr: u32) -> u8 {
        if addr < 0x40 {
            return self.config_rom[addr as usize];
        }
        if crate::envcfg::flag("COPPERLINE_DIAG_CDTV")
            && !(0xB0..0xC0).contains(&addr)
            && addr != 0x41
        {
            log::info!("cdtv rd {addr:02X}");
        }
        if (0xB0..0xC0).contains(&addr) {
            let reg = (addr - 0xB0) / 2;
            let v = self.tp_read(reg);
            if crate::envcfg::flag("COPPERLINE_DIAG_CDTV") {
                log::info!("cdtv tp rd {reg:X}->{v:02X}");
            }
            return v;
        }
        let v = match addr {
            0x41 => {
                let mut v = self.istr;
                if v != 0 {
                    v |= ISTR_INT_P;
                }
                self.istr &= !0x0F;
                if crate::envcfg::flag("COPPERLINE_DIAG_CDTV") {
                    log::info!("cdtv rd 41 -> {v:02X}");
                }
                v
            }
            0x43 => self.cntr,
            0xA1 => {
                self.sten = 0;
                if self.command_cnt_out >= 0 {
                    let v = self.command_out[self.command_cnt_out as usize];
                    self.command_out[self.command_cnt_out as usize] = 0;
                    self.command_cnt_out += 1;
                    if self.command_cnt_out as usize >= self.command_size_out {
                        self.command_size_out = 0;
                        self.command_cnt_out = -1;
                        self.sten = 0;
                    } else {
                        self.sten = 1;
                    }
                    self.tp_check_interrupts();
                    v
                } else {
                    0
                }
            }
            0xE8 | 0xE9 => {
                self.istr |= ISTR_FE_FLG;
                0
            }
            0xA3 | 0xA5 | 0xA7 => 0xFF,
            _ => 0,
        };
        self.tp_check_interrupts();
        v
    }

    fn write_byte(&mut self, addr: u32, value: u8, mem: &mut Memory) {
        if (0xB0..0xC0).contains(&addr) {
            if crate::envcfg::flag("COPPERLINE_DIAG_CDTV") {
                log::info!("cdtv tp wr {:X}={value:02X}", (addr - 0xB0) / 2);
            }
            self.tp_write((addr - 0xB0) / 2, value);
            return;
        }
        if crate::envcfg::flag("COPPERLINE_DIAG_CDTV") {
            log::info!("cdtv wr {addr:02X}={value:02X}");
        }
        match addr {
            0x43 => {
                self.cntr = value;
                if value & CNTR_PREST != 0 {
                    self.reset_drive_state();
                }
            }
            0x80 => self.wtc = (self.wtc & 0x00FF_FFFF) | (u32::from(value) << 24),
            0x81 => self.wtc = (self.wtc & 0xFF00_FFFF) | (u32::from(value) << 16),
            0x82 => self.wtc = (self.wtc & 0xFFFF_00FF) | (u32::from(value) << 8),
            0x83 => self.wtc = (self.wtc & 0xFFFF_FF00) | u32::from(value),
            0x84 => self.acr = (self.acr & 0x00FF_FFFF) | (u32::from(value) << 24),
            0x85 => self.acr = (self.acr & 0xFF00_FFFF) | (u32::from(value) << 16),
            0x86 => self.acr = (self.acr & 0xFFFF_00FF) | (u32::from(value) << 8),
            0x87 => self.acr = (self.acr & 0xFFFF_FF01) | u32::from(value & !1),
            0x8E => self.dawr = (self.dawr & 0x00FF) | (u16::from(value) << 8),
            0x8F => self.dawr = (self.dawr & 0xFF00) | u16::from(value),
            0xA1 => self.drive_command_byte(value),
            0xE0 | 0xE1 => {
                // A word write strobes both byte lanes; only start once
                // (and a consumed WTC of zero is not a new transfer).
                if !self.dma_on && self.wtc != 0 {
                    self.dma_on = true;
                    if self.cntr & CNTR_PDMD == 0 {
                        self.run_dma(mem);
                    }
                }
            }
            0xE2 | 0xE3 => {
                self.dma_on = false;
                self.dma_done_delay_cck = -1;
            }
            0xE4 | 0xE5 => {
                self.istr = 0;
            }
            0xE8 | 0xE9 => self.istr |= ISTR_FE_FLG,
            _ => {}
        }
        self.tp_check_interrupts();
    }

    fn reset_drive_state(&mut self) {
        log::debug!("cdtv: drive reset");
        self.stop_audio();
        self.cd_playing = false;
        self.cd_paused = false;
        self.cd_motor = false;
        self.cd_error = false;
        self.cd_finished = false;
        self.stch = 1;
    }

    // ----- 6525 TPI --------------------------------------------------------

    fn get_tp_c(&self) -> u8 {
        (if self.sbcp != 0 { 0 } else { 1 << 0 })
            | (if self.scor != 0 { 0 } else { 1 << 1 })
            | (if self.stch != 0 { 0 } else { 1 << 2 })
            | (if self.sten != 0 {
                0
            } else {
                (1 << 3) | (1 << 4)
            })
    }

    /// Edge-consuming variant for the interrupt latch path.
    fn get_tp_c_level(&mut self) -> u8 {
        let v = (if self.sbcp == 1 { 0 } else { 1 << 0 })
            | (if self.scor == 1 { 0 } else { 1 << 1 })
            | (if self.stch == 1 { 0 } else { 1 << 2 })
            | (if self.sten == 1 { 0 } else { 1 << 3 })
            | (1 << 4);
        if self.sten == 1 {
            self.sten = -1;
        }
        if self.scor == 1 {
            self.scor = -1;
        }
        if self.sbcp == 1 {
            self.sbcp = -1;
        }
        v
    }

    fn tp_check_interrupts(&mut self) {
        // MC = 0: plain I/O mode, no interrupt latching.
        if self.tp_cr & 1 != 1 {
            self.get_tp_c_level();
            return;
        }
        self.tp_ilatch |= self.get_tp_c_level() ^ 0x1F;
        self.stch = 0;
        if self.tp_ilatch & (1 << 5) == 0 && self.tp_ilatch & self.tp_imask != 0 {
            self.tp_air = 0;
            let mut mask = 0x10u8;
            while (self.tp_ilatch & self.tp_imask) & mask == 0 {
                mask >>= 1;
            }
            self.tp_air |= self.tp_ilatch & mask;
            self.tp_ilatch |= 1 << 5; // interrupt out
            self.tp_ilatch2 = self.tp_ilatch & mask;
            self.tp_ilatch &= !mask;
        }
    }

    fn tp_read(&mut self, reg: u32) -> u8 {
        let v = match reg {
            // Port A: the subcode byte the last SBCP clocked in. Reading
            // it is what takes SBCP away again.
            0 => {
                self.sbcp = 0;
                self.sub_latch.reverse_bits()
            }
            1 => self.tp_b,
            2 => {
                if self.tp_cr & 1 != 0 {
                    self.tp_ilatch | self.tp_ilatch2
                } else {
                    self.get_tp_c()
                }
            }
            3 => self.tp_ad,
            4 => self.tp_bd,
            5 => {
                if self.tp_cr & 1 != 0 {
                    self.tp_imask
                } else {
                    self.tp_cd
                }
            }
            6 => self.tp_cr,
            7 => {
                let v = self.tp_air;
                if self.tp_cr & 1 != 0 {
                    self.tp_ilatch &= !(1 << 5);
                    self.tp_ilatch2 = 0;
                }
                self.tp_air = 0;
                v
            }
            _ => 0,
        };
        self.tp_check_interrupts();
        v
    }

    fn tp_write(&mut self, reg: u32, value: u8) {
        match reg {
            0 => self.tp_a = value,
            1 => self.tp_b = value,
            2 => {
                if self.tp_cr & 1 != 0 {
                    // Interrupt latch: writing 0 bits clears them.
                    self.tp_ilatch &= 0xE0 | value;
                } else {
                    self.tp_c = (self.get_tp_c() & !self.tp_cd) | (value & self.tp_cd);
                }
            }
            3 => self.tp_ad = value,
            4 => self.tp_bd = value,
            5 => {
                if self.tp_cr & 1 != 0 {
                    self.tp_imask = value & 0x1F;
                } else {
                    self.tp_cd = value;
                }
            }
            6 => self.tp_cr = value,
            7 => self.tp_air = value,
            _ => {}
        }
        // CMD/ENABLE/XAEN/DTEN come from port B; only the DAC volume
        // strobes (bits 5-7) would matter beyond this and are ignored
        // (host CD volume is fixed).
        self.tp_check_interrupts();
    }

    // ----- drive commands --------------------------------------------------

    fn drive_command_byte(&mut self, byte: u8) {
        if self.command_cnt_in < self.command_in.len() {
            self.command_in[self.command_cnt_in] = byte;
        }
        self.command_cnt_in += 1;
        let cmd = self.command_in[0];
        let needed: usize = match cmd {
            0x00 | 0x80 => 2,
            0x81 | 0x85 | 0x86 | 0x88 | 0xA2 => 1,
            _ => 7,
        };
        if self.command_cnt_in < needed {
            return;
        }
        self.execute_drive_command();
        self.command_cnt_in = 0;
    }

    fn accept(&mut self, size: i32) {
        self.command_cnt_out = 0;
        self.command_size_out = size.max(0) as usize;
        self.command_done = true;
        if size < 0 {
            self.cd_error = true;
        }
    }

    fn execute_drive_command(&mut self) {
        let cmd = self.command_in[0];
        let input = self.command_in;
        log::debug!(
            "cdtv: drive cmd {:02X?}",
            &input[..self.command_cnt_in.min(7)]
        );
        self.command_out = [0; 16];
        match cmd {
            0x00 | 0x80 => {
                self.command_out[0] = 0xAA;
                self.command_out[1] = 0x55;
                self.accept(2);
            }
            0x01 => {
                // Seek.
                self.cd_finished = true;
                self.activate_stch = true;
                self.accept(0);
            }
            0x02 => {
                // Read data sectors.
                let start =
                    (u32::from(input[1]) << 16) | (u32::from(input[2]) << 8) | u32::from(input[3]);
                let length = (u32::from(input[4]) << 8) | u32::from(input[5]);
                if self.cd_playing {
                    self.stop_audio();
                }
                log::debug!("cdtv: READ DATA {start} +{length} sectors");
                self.read_offset = u64::from(start) * u64::from(self.sector_size);
                self.read_length = u64::from(length) * u64::from(self.sector_size);
                self.last_play_pos = i64::from(start);
                self.cd_motor = true;
                self.audio_status = AUDIO_STATUS_NOT_SUPPORTED;
                self.accept(0);
            }
            0x04 => {
                self.cd_motor = true;
                self.cd_finished = true;
                self.accept(0);
            }
            0x05 => {
                self.cd_motor = false;
                self.cd_finished = true;
                self.accept(0);
            }
            0x09 | 0x0A => {
                // Play audio by LSN (09) or MSF (0A).
                let mut start =
                    (u32::from(input[1]) << 16) | (u32::from(input[2]) << 8) | u32::from(input[3]);
                let mut end =
                    (u32::from(input[4]) << 16) | (u32::from(input[5]) << 8) | u32::from(input[6]);
                if cmd == 0x09 {
                    end += start;
                } else {
                    start = msf_to_lsn(start);
                    if end < 0x00FF_FFFF {
                        end = msf_to_lsn(end);
                    }
                }
                self.play_range(start, end);
                self.accept(0);
            }
            0x0B => {
                // Play by track range.
                let track_start = input[1];
                let track_end = input[3];
                if track_start == 0 && track_end == 0 {
                    self.accept(0);
                } else {
                    let (start, end) = self.track_range(track_start, track_end);
                    match start {
                        Some(start) => {
                            self.play_range(start, end);
                        }
                        None => {
                            self.stop_audio();
                            self.cd_error = true;
                            self.activate_stch = true;
                        }
                    }
                    self.accept(0);
                }
            }
            0x81 => {
                let mut flag = 0u8;
                if !self.cd_isready {
                    flag |= 1 << 0;
                }
                if self.cd_playing {
                    flag |= 1 << 2;
                }
                if self.cd_finished {
                    flag |= 1 << 3;
                }
                if self.cd_error {
                    flag |= 1 << 4;
                }
                if self.cd_motor {
                    flag |= 1 << 5;
                }
                if self.cd_media {
                    flag |= 1 << 6;
                }
                self.command_out[0] = flag;
                self.accept(1);
                self.cd_finished = false;
            }
            0x82 => {
                if self.cd_error {
                    self.command_out[2] |= 1 << 4;
                }
                self.cd_error = false;
                self.cd_isready = false;
                self.cd_finished = true;
                self.accept(6);
            }
            0x83 => {
                self.command_out[..MODEL_NAME.len()].copy_from_slice(MODEL_NAME);
                self.cd_finished = true;
                self.accept(MODEL_NAME.len() as i32);
            }
            0x84 => {
                let size = (u32::from(input[2]) << 8) | u32::from(input[3]);
                match size {
                    512 | 1024 | 2048 | 2052 | 2336 | 2340 | 2352 => self.sector_size = size,
                    _ => {
                        log::warn!("cdtv: unsupported sector size {size}");
                        self.cd_error = true;
                    }
                }
                self.cd_finished = true;
                self.accept(0);
            }
            0x85 => {
                self.command_out[0] = (self.sector_size >> 8) as u8;
                self.command_out[1] = self.sector_size as u8;
                self.accept(2);
            }
            0x86 => {
                let last = self.disc.as_ref().map(|d| d.total_sectors()).unwrap_or(0);
                let size = last.saturating_sub(1);
                self.command_out[0] = (size >> 16) as u8;
                self.command_out[1] = (size >> 8) as u8;
                self.command_out[2] = size as u8;
                self.command_out[3] = (self.sector_size >> 8) as u8;
                self.command_out[4] = self.sector_size as u8;
                if self.cd_media {
                    self.accept(5);
                } else {
                    self.cd_error = true;
                    self.accept(-1);
                }
            }
            0x87 => {
                let len = self.subq_response(input[1] & 2 != 0);
                self.accept(len);
            }
            0x88 => {
                self.accept(14);
            }
            0x89 => {
                let len = self.info_response();
                self.accept(len);
            }
            0x8A => {
                let len = self.toc_response(input[2], input[1] & 2 != 0);
                self.accept(len);
            }
            0x8B => {
                let pause = input[1] == 0x00;
                if self.cd_playing {
                    if pause {
                        self.reset_subcode();
                    }
                    self.cd_paused = pause;
                    self.audio_status = if pause {
                        AUDIO_STATUS_PAUSED
                    } else {
                        AUDIO_STATUS_IN_PROGRESS
                    };
                } else {
                    self.cd_paused = false;
                    self.audio_status = AUDIO_STATUS_NO_STATUS;
                }
                self.cd_finished = true;
                self.accept(0);
            }
            0xA2 => {
                self.accept(4);
            }
            0xA3 => {
                self.cd_finished = true;
                self.accept(0);
            }
            other => {
                log::warn!("cdtv: unknown drive command {other:02X}");
                self.cd_error = true;
                self.accept(0);
            }
        }
    }

    fn play_range(&mut self, start: u32, mut end: u32) {
        if start == 0 && end == 0 {
            self.cd_finished = self.cd_playing;
            self.stop_audio();
            self.cd_motor = false;
            self.audio_status = AUDIO_STATUS_NO_STATUS;
            self.cd_error = true;
            self.activate_stch = true;
            return;
        }
        let last = self.disc.as_ref().map(|d| d.total_sectors()).unwrap_or(0);
        if end >= 0x00FF_FFFF || end > last {
            end = last;
        }
        log::debug!("cdtv: PLAY {start}..{end}");
        self.play_position = i64::from(start);
        self.play_end = i64::from(end);
        self.last_play_pos = i64::from(start);
        self.cd_playing = true;
        self.cd_paused = false;
        self.cd_motor = true;
        self.audio_status = AUDIO_STATUS_IN_PROGRESS;
        self.activate_stch = true;
        self.flush_cd_audio = true;
        self.reset_subcode();
    }

    fn track_range(&self, track_start: u8, track_end: u8) -> (Option<u32>, u32) {
        let Some(disc) = self.disc.as_ref() else {
            return (None, 0);
        };
        let mut start = None;
        let mut end = disc.total_sectors();
        for track in disc.tracks() {
            if track.number == track_start {
                start = Some(track.start_sector);
            }
            if track.number == track_end {
                end = track.start_sector;
            }
        }
        (start, end)
    }

    /// Explicit stop: abandon playback and drop any buffered samples.
    fn stop_audio(&mut self) {
        self.finish_audio();
        self.flush_cd_audio = true;
    }

    /// Playback ended (naturally or by command); buffered samples are
    /// left to drain through the mixer.
    fn finish_audio(&mut self) {
        if self.cd_playing {
            self.audio_status = AUDIO_STATUS_PLAY_COMPLETE;
            self.cd_finished = true;
        }
        self.cd_playing = false;
        self.cd_paused = false;
        self.reset_subcode();
    }

    fn subq_response(&mut self, msf: bool) -> i32 {
        let pos = self.last_play_pos.max(0) as u32;
        let (track, track_start) = self.track_for(pos);
        self.command_out[0] = self.audio_status;
        self.command_out[1] = 0x01; // CtlAdr (audio, ADR 1), pre-swapped
        self.command_out[2] = track;
        self.command_out[3] = 1; // index
        let diskpos = if msf { lsn_to_msf(pos) } else { pos };
        let trackrel = pos.saturating_sub(track_start);
        let trackpos = if msf { lsn_to_msf(trackrel) } else { trackrel };
        self.command_out[4] = 0;
        self.command_out[5] = (diskpos >> 16) as u8;
        self.command_out[6] = (diskpos >> 8) as u8;
        self.command_out[7] = diskpos as u8;
        self.command_out[8] = 0;
        self.command_out[9] = (trackpos >> 16) as u8;
        self.command_out[10] = (trackpos >> 8) as u8;
        self.command_out[11] = trackpos as u8;
        self.command_out[12] = 0;
        13
    }

    fn track_for(&self, pos: u32) -> (u8, u32) {
        let Some(disc) = self.disc.as_ref() else {
            return (1, 0);
        };
        let mut current = (1u8, 0u32);
        for track in disc.tracks() {
            if pos >= track.start_sector {
                current = (track.number, track.start_sector);
            }
        }
        current
    }

    fn info_response(&mut self) -> i32 {
        let Some(disc) = self.disc.as_ref() else {
            return -1;
        };
        let first = disc.tracks().first().map(|t| t.number).unwrap_or(1);
        let last = disc.tracks().last().map(|t| t.number).unwrap_or(1);
        let size = lsn_to_msf(disc.total_sectors());
        self.cd_motor = true;
        self.command_out[0] = first;
        self.command_out[1] = last;
        self.command_out[2] = (size >> 16) as u8;
        self.command_out[3] = (size >> 8) as u8;
        self.command_out[4] = size as u8;
        self.cd_finished = true;
        5
    }

    fn toc_response(&mut self, point: u8, msf: bool) -> i32 {
        let Some(disc) = self.disc.as_ref() else {
            return -1;
        };
        let track_count = disc.tracks().len() as u8;
        let total = disc.total_sectors();
        let first = disc.tracks().first().map(|t| t.number).unwrap_or(1);
        let last = disc.tracks().last().map(|t| t.number).unwrap_or(1);
        // Session points plus per-track entries, as read_toc serves them.
        let entry: Option<(u8, u8, u32)> = match point {
            0xA0 => Some((0x01, 0xA0, u32::from(first) << 16)),
            0xA1 => Some((0x01, 0xA1, u32::from(last) << 16)),
            0xA2 => Some((0x01, 0xA2, if msf { lsn_to_msf(total) } else { total })),
            p => disc.tracks().iter().find(|t| t.number == p).map(|t| {
                let addr = if msf {
                    lsn_to_msf(t.start_sector)
                } else {
                    t.start_sector
                };
                let control = if t.kind.is_data() { 0x04 } else { 0x00 };
                ((0x01 << 4) | control, to_bcd(t.number), addr)
            }),
        };
        let Some((ctladr, point_out, addr)) = entry else {
            return -1;
        };
        self.cd_motor = true;
        self.command_out[0] = 0;
        self.command_out[1] = ctladr;
        self.command_out[2] = point_out;
        self.command_out[3] = track_count;
        self.command_out[4] = 0;
        self.command_out[5] = (addr >> 16) as u8;
        self.command_out[6] = (addr >> 8) as u8;
        self.command_out[7] = addr as u8;
        self.cd_finished = true;
        8
    }

    // ----- DMA -------------------------------------------------------------

    /// The reference performs the whole transfer when DMA starts, then
    /// delays the completion interrupt by the single-speed read time.
    /// One word of sector data onto the system bus. The DMAC is a 24-bit
    /// bus master like the A2091's: it reaches chip RAM, slow RAM, and
    /// Zorro II board RAM (the CD driver allocates CD buffers in fast RAM when
    /// present). Unmapped targets warn once and drop the data.
    fn dma_write_word(&mut self, mem: &mut Memory, addr: u32, hi: u8, lo: u8) {
        let w = (u16::from(hi) << 8) | u16::from(lo);
        if !crate::zorro_device::dma_write_word(mem, addr, w) && !self.dma_warned {
            self.dma_warned = true;
            log::warn!("cdtv: DMA to unmapped address {addr:#08X}; ignoring");
        }
    }

    fn dma_read_byte(mem: &Memory, addr: u32) -> u8 {
        crate::zorro_device::dma_read_byte(mem, addr).unwrap_or(0xFF)
    }

    fn run_dma(&mut self, mem: &mut Memory) {
        let words = self.wtc;
        if self.disc.is_none() {
            self.dma_on = false;
            return;
        }
        let sector_size = u64::from(self.sector_size);
        if sector_size < 2048 {
            log::warn!("cdtv: DMA with sector size {sector_size} not supported");
            self.dma_on = false;
            return;
        }
        let mut raw = [0u8; RAW_SECTOR_BYTES];
        let mut current_sector = u32::MAX;
        let mut remaining = words;
        while remaining > 0 && self.dma_on {
            let sector = (self.read_offset / sector_size) as u32;
            self.last_play_pos = i64::from(sector);
            let in_sector = (self.read_offset % sector_size) as usize;
            if sector != current_sector {
                let disc = self.disc.as_mut().expect("checked above");
                let ok = if sector_size == 2048 {
                    let mut data = [0u8; 2048];
                    let r = disc.read_data_sector(sector, &mut data);
                    raw[..2048].copy_from_slice(&data);
                    r
                } else {
                    disc.read_raw_sector(sector, &mut raw)
                };
                if ok.is_err() {
                    log::warn!("cdtv: CD read error at sector {sector}");
                    self.cd_error = true;
                    self.activate_stch = true;
                    break;
                }
                current_sector = sector;
            }
            self.dma_write_word(mem, self.acr, raw[in_sector], raw[in_sector + 1]);
            self.acr = self.acr.wrapping_add(2);
            self.read_offset += 2;
            self.read_length = self.read_length.saturating_sub(2);
            remaining -= 1;
        }
        if crate::envcfg::flag("COPPERLINE_DIAG_CDTV") {
            let base = self.acr.wrapping_sub(words * 2);
            let peek: Vec<u8> = (0..8)
                .map(|i| Self::dma_read_byte(mem, base.wrapping_add(i)))
                .collect();
            log::info!("cdtv DMA done: {words} words to {base:#08X}, first bytes {peek:02X?}");
        }
        self.wtc = 0;
        self.dma_on = false;
        self.cd_finished = true;
        // Completion interrupt after the 1x read time for the words moved.
        let bytes = u64::from(words) * 2;
        self.dma_done_delay_cck =
            (bytes * u64::from(CCK_PER_CD_FRAME) / 2048).max(CCK_PER_LINE as u64) as i64;
    }

    // ----- periodic work ---------------------------------------------------

    pub fn tick(&mut self, cck: u32, cd_audio: &mut CdAudioRing) {
        // The bookmark RAM sits on the motherboard, not behind the DMAC's
        // autoconfig, so its backing file follows it whether or not the
        // board has been configured yet.
        if self.battram_dirty {
            self.battram_dirty_cck = self.battram_dirty_cck.saturating_add(cck);
            if self.battram_dirty_cck >= BOOKMARK_FLUSH_CCK {
                self.flush_bookmark_ram();
            }
        }
        if self.configured_base.is_none() {
            return;
        }

        // Delayed disc insert: media change once the timer runs out.
        if self.pending_disc.is_some() {
            self.insert_delay_cck -= i64::from(cck);
            if self.insert_delay_cck < 0 {
                self.disc = self.pending_disc.take();
                self.cd_media = true;
                self.activate_stch = true;
                log::info!("cdtv: disc inserted (delayed), media-change STCH raised");
            }
        }

        if self.flush_cd_audio {
            self.flush_cd_audio = false;
            cd_audio.clear();
        }

        // DMA completion interrupt after the modelled read time.
        if self.dma_done_delay_cck >= 0 {
            self.dma_done_delay_cck -= i64::from(cck);
            if self.dma_done_delay_cck < 0
                && self.cntr & (CNTR_INTEN | CNTR_TCEN) == (CNTR_INTEN | CNTR_TCEN)
            {
                self.istr |= ISTR_INT_P | ISTR_E_INT;
                if crate::envcfg::flag("COPPERLINE_DIAG_CDTV") {
                    log::info!("cdtv E_INT fired");
                }
            }
        }

        // Subcode symbols of the frame in transit. Before the audio step,
        // so a frame's last symbol is out before the next frame loads.
        if !self.sub_frame.is_empty() {
            self.sub_symbol_cck -= cck as i32;
            while self.sub_symbol_cck <= 0 && !self.sub_frame.is_empty() {
                self.sub_symbol_cck += CCK_PER_SUBCODE_SYMBOL as i32;
                self.subcode_symbol();
            }
        }

        // CD audio: stream one sector per CD frame at single speed.
        self.audio_counter_cck -= cck as i32;
        if self.audio_counter_cck <= 0 {
            self.audio_counter_cck += CCK_PER_CD_FRAME as i32;
            self.stream_audio_sector(cd_audio);
        }

        // Per-scanline housekeeping, as the reference's hsync handler.
        self.line_counter_cck -= cck as i32;
        while self.line_counter_cck <= 0 {
            self.line_counter_cck += CCK_PER_LINE as i32;
            self.line_tick();
        }
    }

    fn line_tick(&mut self) {
        if !self.booted {
            // Initial status change: media present notification path.
            self.booted = true;
            self.activate_stch = true;
        }

        if self.command_done {
            self.command_done = false;
            self.sten = 1;
            self.stch = 0;
            self.tp_check_interrupts();
        }
        if self.sten < 0 {
            self.sten -= 1;
            if self.sten < -3 {
                self.sten = 0;
            }
        }

        self.lines_until_frame -= 1;
        if self.lines_until_frame <= 0 {
            // 75 Hz frame pulse: SCOR fires continuously while not
            // playing (the reference notes these happen all the time).
            self.lines_until_frame = (CCK_PER_CD_FRAME / CCK_PER_LINE) as i32;
            if self.scor == 0 && !self.cd_playing {
                self.scor = 1;
                self.tp_check_interrupts();
                self.scor = 0;
            }
        }

        if self.activate_stch {
            self.do_stch();
        }
    }

    /// A sector just passed under the pickup: its subcode frame starts.
    /// SCOR marks the frame; the 96 symbols follow, one per symbol
    /// period. A frame still in transit is cut short, as a new frame
    /// sync cuts the old frame short on the wire.
    fn start_subcode_frame(&mut self, frame: &[u8]) {
        self.sub_frame.clear();
        self.sub_frame.extend_from_slice(frame);
        self.sub_index = 0;
        self.sub_symbol_cck = CCK_PER_SUBCODE_SYMBOL as i32;
        self.sbcp = 0;
        self.scor = 1;
        self.tp_check_interrupts();
    }

    /// One subcode symbol has been shifted in: it is on port A and SBCP
    /// says so. An earlier symbol that was never read is gone.
    fn subcode_symbol(&mut self) {
        // The frame pulse is over by the first symbol.
        if self.scor < 0 {
            self.scor = 0;
        }
        // The frame and the index come out of a save state as well as from
        // `start_subcode_frame`, so neither is trusted: an index past the
        // frame ends it, and a frame is never longer than a sector's 96
        // symbols.
        let index = usize::from(self.sub_index);
        let end = self.sub_frame.len().min(SUBCODE_FRAME_BYTES);
        if index >= end {
            self.sub_frame.clear();
            self.sub_index = 0;
            return;
        }
        self.sub_latch = self.sub_frame[index];
        if index + 1 >= end {
            self.sub_frame.clear();
            self.sub_index = 0;
        } else {
            self.sub_index = (index + 1) as u8;
        }
        self.sbcp = 1;
        self.tp_check_interrupts();
    }

    /// Colour clocks until the drive next does something on its own that
    /// the guest can be waiting for: the next subcode symbol of a frame in
    /// transit, or the next sector of a play (its SCOR). A CPU halted in
    /// STOP must not sleep past either -- it would wake to find the bytes
    /// it was waiting for already replaced.
    pub fn next_event_cck(&self) -> Option<u32> {
        // Nothing runs until the board is configured.
        self.configured_base?;
        let symbol = (!self.sub_frame.is_empty()).then_some(self.sub_symbol_cck);
        let sector = (self.cd_playing && !self.cd_paused).then_some(self.audio_counter_cck);
        symbol
            .into_iter()
            .chain(sector)
            .min()
            .map(|cck| cck.max(1) as u32)
    }

    /// The pickup stopped following a track (play started over, paused or
    /// stopped): whatever frame was in transit is abandoned and both
    /// subcode lines go quiet.
    ///
    /// UNVERIFIED for pause: a paused drive here raises no SCOR at all --
    /// no frames, because no sector plays, and none of the idle 75 Hz
    /// pulses either, because those are held off while a play is in
    /// progress and a paused play still is. That is what the reference
    /// (WinUAE's cdtv.cpp) does. A real drive holding its position in
    /// pause may well keep decoding subcode there and keep pulsing SCOR;
    /// nothing at hand says which, so the reference's behaviour stands.
    fn reset_subcode(&mut self) {
        self.sub_frame.clear();
        self.sub_index = 0;
        self.sbcp = 0;
        self.scor = 0;
    }

    fn do_stch(&mut self) {
        if self.tp_cr & 1 != 0 && self.tp_air & (1 << 2) == 0 {
            self.stch = 1;
            self.activate_stch = false;
            self.tp_check_interrupts();
        }
    }

    fn stream_audio_sector(&mut self, cd_audio: &mut CdAudioRing) {
        if !self.cd_playing || self.cd_paused {
            return;
        }
        if !cd_audio.wants_sector() {
            return;
        }
        let Some(disc) = self.disc.as_mut() else {
            return;
        };
        if self.play_position >= self.play_end || self.play_position < 0 {
            self.finish_audio();
            self.activate_stch = true;
            return;
        }
        let sector = self.play_position as u32;
        if disc.is_audio_sector(sector) {
            let mut raw = [0u8; RAW_SECTOR_BYTES];
            if disc.read_audio_sector(sector, &mut raw).is_ok() {
                cd_audio.push_sector(&raw);
            }
        }
        // The pickup reads the sector's subcode with it.
        let frame = disc.subcode_frame(sector);
        self.play_position += 1;
        self.last_play_pos = self.play_position;
        self.start_subcode_frame(&frame);
    }
}

/// A session that ends inside the flush interval still keeps its last save.
impl Drop for CdtvController {
    fn drop(&mut self) {
        self.flush_bookmark_ram();
    }
}

impl crate::zorro_device::ZorroDevice for CdtvController {
    // The CDTV self-decodes its window (base is 64K-aligned), so the dispatch
    // passes the window offset, which `read_byte`/`write_byte` mask to the same
    // low 16 bits the absolute address carried before.
    fn read(&mut self, off: u32, size: usize, _host: &mut crate::zorro_device::DeviceHost) -> u32 {
        Self::read(self, off, size)
    }

    fn write(
        &mut self,
        off: u32,
        size: usize,
        value: u32,
        host: &mut crate::zorro_device::DeviceHost,
    ) {
        Self::write(self, off, size, value, host.memory_mut())
    }

    // The DMAC tick streams CD audio rather than touching guest memory, so it
    // draws Paula's CD-audio ring from the host instead of `Memory`.
    fn tick(&mut self, cck: u32, host: &mut crate::zorro_device::DeviceHost) {
        Self::tick(self, cck, host.cd_audio())
    }

    fn int2_line(&self) -> bool {
        Self::int2_line(self)
    }

    fn reset(&mut self) {
        Self::reset(self)
    }

    fn kind(&self) -> &'static str {
        "cdtv"
    }
}

impl Default for CdtvController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdrom::DATA_SECTOR_BYTES;
    use std::path::PathBuf;

    fn test_disc() -> CdImage {
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir();
        let cue: PathBuf = dir.join(format!(
            "copperline-cdtv-{}-{unique}.cue",
            std::process::id()
        ));
        let bin: PathBuf = dir.join(format!(
            "copperline-cdtv-{}-{unique}.bin",
            std::process::id()
        ));
        let mut bytes = Vec::new();
        for s in 0u8..4 {
            bytes.extend(std::iter::repeat_n(s, DATA_SECTOR_BYTES));
        }
        for _ in 0..2 * (RAW_SECTOR_BYTES / 4) {
            bytes.extend_from_slice(&[0x00, 0x10, 0x00, 0x20]);
        }
        std::fs::write(&bin, &bytes).unwrap();
        std::fs::write(
            &cue,
            format!(
                concat!(
                    "FILE \"{}\" BINARY\n",
                    "  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n",
                    "  TRACK 02 AUDIO\n    INDEX 01 00:00:04\n",
                ),
                bin.file_name().unwrap().to_string_lossy()
            ),
        )
        .unwrap();
        let image = CdImage::load(&cue).unwrap();
        let _ = std::fs::remove_file(&cue);
        let _ = std::fs::remove_file(&bin);
        image
    }

    fn configured_controller() -> CdtvController {
        let mut cdtv = CdtvController::new();
        cdtv.insert_disc(test_disc());
        cdtv.config_write(0x48, 1, 0xE9);
        assert_eq!(cdtv.configured_base, Some(0x00E9_0000));
        cdtv
    }

    fn test_mem(chip_bytes: usize) -> Memory {
        Memory {
            chip_ram: vec![0u8; chip_bytes],
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

    /// Send a drive command and read back the response bytes via $A1.
    fn command(cdtv: &mut CdtvController, mem: &mut Memory, bytes: &[u8]) -> Vec<u8> {
        for byte in bytes {
            cdtv.write(0xA1, 1, u32::from(*byte), mem);
        }
        let size = cdtv.command_size_out;
        let mut out = Vec::new();
        for _ in 0..size {
            out.push(cdtv.read(0xA1, 1) as u8);
        }
        out
    }

    #[test]
    fn eject_disc_clears_media_and_raises_media_change_stch() {
        let mut cdtv = configured_controller();
        assert!(cdtv.has_disc());
        assert!(cdtv.cd_media);

        cdtv.eject_disc();
        assert!(!cdtv.has_disc());
        assert!(!cdtv.cd_media);
        assert!(cdtv.activate_stch, "eject must raise the media-change STCH");

        // Ejecting an empty drive is a no-op.
        cdtv.activate_stch = false;
        cdtv.eject_disc();
        assert!(!cdtv.activate_stch);

        // Eject also cancels a disc still waiting in the tray.
        cdtv.insert_disc_after(test_disc(), 5.0);
        assert!(cdtv.has_disc());
        cdtv.eject_disc();
        assert!(!cdtv.has_disc());
    }

    #[test]
    fn activity_led_tracks_audio_and_data_work() {
        let mut cdtv = configured_controller();
        assert!(!cdtv.activity_led_on());

        cdtv.cd_playing = true;
        assert!(cdtv.activity_led_on());
        cdtv.cd_paused = true;
        assert!(!cdtv.activity_led_on());
        cdtv.cd_playing = false;
        cdtv.cd_paused = false;

        cdtv.read_length = 2048;
        assert!(cdtv.activity_led_on());
        cdtv.read_length = 0;
        cdtv.dma_on = true;
        assert!(cdtv.activity_led_on());
        cdtv.dma_on = false;
        assert!(!cdtv.activity_led_on());
    }

    #[test]
    fn current_track_follows_the_optical_head_and_empty_tray() {
        let mut cdtv = configured_controller();
        assert_eq!(cdtv.current_track(), Some(1));

        cdtv.play_range(4, 6);
        assert_eq!(cdtv.current_track(), Some(2));

        cdtv.eject_disc();
        assert_eq!(cdtv.current_track(), None);
        cdtv.insert_disc_after(test_disc(), 5.0);
        assert_eq!(cdtv.current_track(), None, "a pending disc is not mounted");
    }

    #[test]
    fn autoconfig_rom_identifies_commodore_dmac() {
        let cdtv = CdtvController::new();
        // er_Type $C1: Zorro II, 64K board.
        assert_eq!(cdtv.config_read(0x00, 1), 0xC0);
        assert_eq!(cdtv.config_read(0x02, 1), 0x10);
        // Product 3 (inverted nibbles).
        assert_eq!(cdtv.config_read(0x04, 1) & 0xF0, 0xF0);
        assert_eq!(cdtv.config_read(0x06, 1) & 0xF0, 0xC0);
    }

    #[test]
    fn handshake_command_returns_aa55_and_pulses_sten() {
        let mut mem = test_mem(4096);
        let mut cdtv = configured_controller();
        // Put the 6525 into interrupt mode with STEN unmasked.
        cdtv.write(0xBC, 1, 0x01, &mut mem); // CR: MC=1
        cdtv.write(0xBA, 1, 0x08, &mut mem); // imask: STEN

        let out = command(&mut cdtv, &mut mem, &[0x00, 0x00]);
        assert_eq!(out, vec![0xAA, 0x55]);
    }

    #[test]
    fn status_command_reports_media_and_motor() {
        let mut mem = test_mem(4096);
        let mut cdtv = configured_controller();
        let out = command(&mut cdtv, &mut mem, &[0x81]);
        assert_eq!(out.len(), 1);
        assert_ne!(out[0] & 0x40, 0, "media bit should be set");

        // Motor on, then status again.
        let _ = command(&mut cdtv, &mut mem, &[0x04, 0, 0, 0, 0, 0, 0]);
        let out = command(&mut cdtv, &mut mem, &[0x81]);
        assert_ne!(out[0] & 0x20, 0, "motor bit should be set");
    }

    #[test]
    fn read_and_dma_transfers_sector_data_into_chip_ram() {
        let mut mem = test_mem(64 * 1024);
        let mut cdtv = configured_controller();

        // READ sector 1, 1 sector; then DMA 1024 words to $2000.
        let _ = command(&mut cdtv, &mut mem, &[0x02, 0, 0, 1, 0, 1, 0]);
        cdtv.write(0x80, 4, 1024, &mut mem); // WTC
        cdtv.write(0x84, 4, 0x2000, &mut mem); // ACR
        cdtv.write(0x43, 1, u32::from(CNTR_INTEN | CNTR_TCEN), &mut mem);
        cdtv.write(0xE0, 1, 0, &mut mem); // start DMA

        assert!(mem.chip_ram[0x2000..0x2000 + 2048].iter().all(|&b| b == 1));
        // Completion interrupt fires after the modelled read time.
        let mut ring = CdAudioRing::default();
        for _ in 0..80 {
            cdtv.tick(2048, &mut ring);
        }
        assert_ne!(cdtv.istr & ISTR_E_INT, 0);
        assert!(cdtv.int2_line());
    }

    #[test]
    fn dma_reaches_zorro_fast_ram_without_touching_chip_ram() {
        // The DMAC is a 24-bit bus master: when the CD driver allocates the CD
        // buffer in autoconfigured fast RAM, sector data must land on the
        // board, not wrap into chip RAM (which corrupted the display and
        // made the CDTV ROM fall back to the audio player).
        let mut mem = test_mem(64 * 1024);
        mem.zorro
            .add_board_configured_at(crate::zorro::BoardSpec::fast_ram(512 * 1024), 0x0020_0000)
            .unwrap();
        let mut cdtv = configured_controller();

        // READ sector 1, 1 sector; then DMA 1024 words to $200800.
        let _ = command(&mut cdtv, &mut mem, &[0x02, 0, 0, 1, 0, 1, 0]);
        cdtv.write(0x80, 4, 1024, &mut mem); // WTC
        cdtv.write(0x84, 4, 0x0020_0800, &mut mem); // ACR
        cdtv.write(0x43, 1, u32::from(CNTR_INTEN | CNTR_TCEN), &mut mem);
        cdtv.write(0xE0, 1, 0, &mut mem); // start DMA

        let (board, off) = mem.zorro.region_at(0x0020_0800, 2).unwrap();
        let ram = mem.zorro.board_ram(board);
        assert!(ram[off..off + 2048].iter().all(|&b| b == 1));
        assert!(mem.chip_ram.iter().all(|&b| b == 0), "chip RAM untouched");
    }

    #[test]
    fn toc_and_info_commands_describe_the_disc() {
        let mut mem = test_mem(4096);
        let mut cdtv = configured_controller();

        let info = command(&mut cdtv, &mut mem, &[0x89, 0, 0, 0, 0, 0, 0]);
        assert_eq!(info[0], 1, "first track");
        assert_eq!(info[1], 2, "last track");

        // Lead-out point A2 in LSN mode: 6 sectors total.
        let toc = command(&mut cdtv, &mut mem, &[0x8A, 0, 0xA2, 0, 0, 0, 0]);
        let addr = (u32::from(toc[5]) << 16) | (u32::from(toc[6]) << 8) | u32::from(toc[7]);
        assert_eq!(addr, 6);

        // Track 2 is audio at sector 4.
        let toc = command(&mut cdtv, &mut mem, &[0x8A, 0, 0x02, 0, 0, 0, 0]);
        assert_eq!(toc[1] & 0x04, 0, "audio control nibble");
        let addr = (u32::from(toc[5]) << 16) | (u32::from(toc[6]) << 8) | u32::from(toc[7]);
        assert_eq!(addr, 4);
    }

    #[test]
    fn delayed_insert_raises_media_change_after_timer() {
        let mut mem = test_mem(4096);
        let mut ring = CdAudioRing::default();
        let mut cdtv = CdtvController::new();
        cdtv.insert_disc_after(test_disc(), 0.05);
        cdtv.config_write(0x48, 1, 0xE9);
        // Interrupt mode with STCH unmasked, so the insert is observable.
        cdtv.write(0xBC, 1, 0x01, &mut mem);
        cdtv.write(0xBA, 1, 0x04, &mut mem);

        // Before the timer: no media.
        let out = command(&mut cdtv, &mut mem, &[0x81]);
        assert_eq!(out[0] & 0x40, 0, "no media before the delayed insert");

        // ~0.06s of emulated time.
        for _ in 0..110 {
            cdtv.tick(2048, &mut ring);
        }
        assert!(cdtv.cd_media, "media should be inserted");
        assert_ne!(cdtv.tp_ilatch & (1 << 5), 0, "STCH interrupt raised");
        let out = command(&mut cdtv, &mut mem, &[0x81]);
        assert_ne!(out[0] & 0x40, 0, "media bit set after insert");
    }

    #[test]
    fn play_streams_audio_and_completes() {
        let mut mem = test_mem(4096);
        let mut cdtv = configured_controller();
        let mut ring = CdAudioRing::default();

        // PLAY LSN 4, length 2.
        let _ = command(&mut cdtv, &mut mem, &[0x09, 0, 0, 4, 0, 0, 2]);
        assert!(cdtv.cd_playing);
        for _ in 0..16 {
            cdtv.tick(CCK_PER_CD_FRAME / 2, &mut ring);
        }
        assert!(!cdtv.cd_playing, "play should have completed");
        assert_eq!(cdtv.audio_status, AUDIO_STATUS_PLAY_COMPLETE);
        let (left, right) = ring.next_sample();
        assert!((left - 0x1000 as f32 / 32768.0).abs() < 1e-4);
        assert!((right - 0x2000 as f32 / 32768.0).abs() < 1e-4);
    }

    // ----- subcode ---------------------------------------------------------

    /// The Q channel out of 96 symbols read from port A (bit-reversed: P
    /// in bit 0, Q in bit 1).
    fn q_from_port_a(symbols: &[u8]) -> [u8; 12] {
        let mut q = [0u8; 12];
        for (i, symbol) in symbols.iter().enumerate() {
            if symbol & 0x02 != 0 {
                q[i / 8] |= 0x80 >> (i % 8);
            }
        }
        q
    }

    /// Step in single colour clocks until SBCP is asserted; returns how
    /// many it took, or `None` if it never came within `limit`.
    fn wait_for_sbcp(cdtv: &mut CdtvController, ring: &mut CdAudioRing, limit: u32) -> Option<u32> {
        for waited in 1..=limit {
            cdtv.tick(1, ring);
            if cdtv.sbcp != 0 {
                return Some(waited);
            }
        }
        None
    }

    #[test]
    fn a_played_sector_clocks_its_subcode_frame_out_behind_scor() {
        let mut mem = test_mem(4096);
        let mut cdtv = configured_controller();
        let mut ring = CdAudioRing::default();
        // 6525 in interrupt mode (CR, register 6) with SCOR and SBCP
        // unmasked (register 5); the registers sit two bytes apart.
        cdtv.write(0xB0 + 2 * 6, 1, 0x01, &mut mem);
        cdtv.write(0xB0 + 2 * 5, 1, 0x03, &mut mem);

        // PLAY LSN 4, length 2 (the test disc's audio track).
        let _ = command(&mut cdtv, &mut mem, &[0x09, 0, 0, 4, 0, 0, 2]);
        assert_eq!(cdtv.sbcp, 0);
        // Run to the frame: the first sector plays and SCOR is latched.
        let mut waited = 0;
        while cdtv.sub_frame.is_empty() {
            cdtv.tick(1, &mut ring);
            waited += 1;
            assert!(waited <= 2 * CCK_PER_CD_FRAME, "no sector played");
        }
        assert_ne!(cdtv.scor, 0, "SCOR marks the frame");
        assert_ne!(
            (cdtv.tp_ilatch | cdtv.tp_ilatch2) & 0x02,
            0,
            "SCOR reached the 6525's interrupt latch"
        );
        assert!(cdtv.int2_line());
        assert_eq!(cdtv.sbcp, 0, "no byte yet");

        // 96 bytes follow, one per symbol period, each raising SBCP until
        // port A is read.
        let mut symbols = Vec::new();
        for n in 0..96 {
            let waited = wait_for_sbcp(&mut cdtv, &mut ring, 2 * CCK_PER_SUBCODE_SYMBOL)
                .unwrap_or_else(|| panic!("byte {n} never arrived"));
            assert_eq!(waited, CCK_PER_SUBCODE_SYMBOL, "byte {n} spacing");
            assert_eq!(
                cdtv.get_tp_c() & 0x01,
                0,
                "SBCP is low (asserted) on port C"
            );
            symbols.push(cdtv.read(0xB0, 1) as u8);
            assert_eq!(cdtv.sbcp, 0, "reading port A takes SBCP away");
            assert_eq!(cdtv.get_tp_c() & 0x01, 0x01);
        }
        assert!(cdtv.sub_frame.is_empty(), "the frame is 96 bytes long");
        // The frame is sector 4's: track 2, index 1, at its first sector.
        let q = q_from_port_a(&symbols);
        assert_eq!(
            &q[..10],
            &[0x01, 0x02, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x04]
        );
        // No stored subcode on this disc: nothing but Q.
        assert!(symbols.iter().all(|&s| s & !0x02 == 0));

        // The next sector's frame follows at the next CD frame.
        let mut waited = 0;
        while cdtv.sub_frame.is_empty() {
            cdtv.tick(1, &mut ring);
            waited += 1;
            assert!(waited <= CCK_PER_CD_FRAME, "second frame missing");
        }
        wait_for_sbcp(&mut cdtv, &mut ring, 2 * CCK_PER_SUBCODE_SYMBOL).unwrap();
        // Its first symbol is Q bit 0 of control/ADR $01: clear.
        assert_eq!(cdtv.read(0xB0, 1) & 0x02, 0);
    }

    #[test]
    fn an_unread_subcode_byte_is_replaced_by_the_next() {
        let mut mem = test_mem(4096);
        let mut cdtv = configured_controller();
        let mut ring = CdAudioRing::default();
        let _ = command(&mut cdtv, &mut mem, &[0x09, 0, 0, 4, 0, 0, 2]);
        while cdtv.sub_frame.is_empty() {
            cdtv.tick(1, &mut ring);
        }
        // Sector 4's Q packet starts 0000 0001 (control/ADR $01): the
        // eighth symbol is the first with its Q bit set. Let seven go by
        // unread; port A then holds the eighth, not the first.
        while cdtv.sub_index < 8 {
            cdtv.tick(1, &mut ring);
        }
        assert_ne!(cdtv.sbcp, 0, "SBCP stays up while nobody reads port A");
        assert_eq!(cdtv.read(0xB0, 1) as u8, 0x02);
        assert_eq!(cdtv.sbcp, 0);
    }

    #[test]
    fn a_malformed_restored_subcode_frame_ends_instead_of_panicking() {
        let mut cdtv = configured_controller();
        let mut ring = CdAudioRing::default();
        // An index past the frame: the frame just ends.
        cdtv.sub_frame = vec![0x40; 10];
        cdtv.sub_index = 200;
        cdtv.sub_symbol_cck = 1;
        cdtv.tick(2, &mut ring);
        assert!(cdtv.sub_frame.is_empty());
        assert_eq!((cdtv.sub_index, cdtv.sbcp), (0, 0));

        // A frame longer than a sector's 96 symbols is clocked out for 96
        // and no further, however far behind the clock is.
        cdtv.sub_frame = vec![0x40; 1000];
        cdtv.sub_index = 0;
        cdtv.sub_symbol_cck = i32::MIN / 2;
        cdtv.tick(1, &mut ring);
        assert!(cdtv.sub_frame.is_empty());
        assert_eq!(cdtv.sub_index, 0);
        assert_eq!(cdtv.read(0xB0, 1) as u8, 0x02);
    }

    #[test]
    fn the_drive_reports_its_next_subcode_symbol_and_sector_as_events() {
        let mut mem = test_mem(4096);
        let mut cdtv = configured_controller();
        let mut ring = CdAudioRing::default();
        // Idle: nothing a sleeping CPU needs waking for.
        assert_eq!(cdtv.next_event_cck(), None);
        let _ = command(&mut cdtv, &mut mem, &[0x09, 0, 0, 4, 0, 0, 2]);
        while cdtv.sub_frame.is_empty() {
            cdtv.tick(1, &mut ring);
        }
        // A frame in transit: the next symbol is one period away, sooner
        // than the next sector, and each tick brings it closer.
        assert_eq!(cdtv.next_event_cck(), Some(CCK_PER_SUBCODE_SYMBOL));
        cdtv.tick(100, &mut ring);
        assert_eq!(cdtv.next_event_cck(), Some(CCK_PER_SUBCODE_SYMBOL - 100));
        // Sleeping exactly that long lands on the symbol.
        cdtv.tick(CCK_PER_SUBCODE_SYMBOL - 100, &mut ring);
        assert_ne!(cdtv.sbcp, 0);
        // Paused: no frames and no sectors are coming.
        let _ = command(&mut cdtv, &mut mem, &[0x8B, 0x00, 0, 0, 0, 0, 0]);
        assert_eq!(cdtv.next_event_cck(), None);
    }

    #[test]
    fn pausing_or_stopping_quiets_the_subcode_lines() {
        let mut mem = test_mem(4096);
        let mut cdtv = configured_controller();
        let mut ring = CdAudioRing::default();
        let _ = command(&mut cdtv, &mut mem, &[0x09, 0, 0, 4, 0, 0, 2]);
        while cdtv.sub_frame.is_empty() {
            cdtv.tick(1, &mut ring);
        }
        wait_for_sbcp(&mut cdtv, &mut ring, 2 * CCK_PER_SUBCODE_SYMBOL).unwrap();
        // Pause: the frame in transit is abandoned.
        let _ = command(&mut cdtv, &mut mem, &[0x8B, 0x00, 0, 0, 0, 0, 0]);
        assert!(cdtv.cd_paused);
        assert_eq!(cdtv.sbcp, 0);
        assert!(wait_for_sbcp(&mut cdtv, &mut ring, 2 * CCK_PER_CD_FRAME).is_none());
        // Resume: frames come again.
        let _ = command(&mut cdtv, &mut mem, &[0x8B, 0x80, 0, 0, 0, 0, 0]);
        assert!(wait_for_sbcp(&mut cdtv, &mut ring, 2 * CCK_PER_CD_FRAME).is_some());
        cdtv.stop_audio();
        assert_eq!(cdtv.sbcp, 0);
        assert!(wait_for_sbcp(&mut cdtv, &mut ring, 2 * CCK_PER_CD_FRAME).is_none());
    }

    #[test]
    fn an_idle_drive_pulses_scor_without_any_subcode_bytes() {
        let mut mem = test_mem(4096);
        let mut cdtv = configured_controller();
        let mut ring = CdAudioRing::default();
        cdtv.write(0xB0 + 2 * 6, 1, 0x01, &mut mem);
        cdtv.write(0xB0 + 2 * 5, 1, 0x03, &mut mem);
        let mut scor_seen = false;
        for _ in 0..3 * CCK_PER_CD_FRAME / CCK_PER_LINE {
            cdtv.tick(CCK_PER_LINE, &mut ring);
            assert_eq!(cdtv.sbcp, 0);
            scor_seen |= (cdtv.tp_ilatch | cdtv.tp_ilatch2) & 0x02 != 0;
        }
        assert!(scor_seen);
        assert_eq!(cdtv.read(0xB0, 1), 0, "port A holds nothing");
    }

    #[test]
    fn stored_r_to_w_subcode_reaches_port_a() {
        // The test disc again, with a .sub file beside its image: every
        // sector's R-W symbols are 0x2A (R, T and V set).
        let dir = std::env::temp_dir();
        let stem = format!("copperline-cdtv-sub-{}", std::process::id());
        let cue = dir.join(format!("{stem}.cue"));
        let bin = dir.join(format!("{stem}.bin"));
        let sub = dir.join(format!("{stem}.sub"));
        let sectors = 6;
        std::fs::write(&bin, vec![0u8; sectors * RAW_SECTOR_BYTES]).unwrap();
        let mut stored = Vec::new();
        for _ in 0..sectors {
            stored.extend_from_slice(&[0u8; 24]);
            for channel in 2..8 {
                stored.extend_from_slice(&[if channel % 2 == 0 { 0xFF } else { 0x00 }; 12]);
            }
        }
        std::fs::write(&sub, &stored).unwrap();
        std::fs::write(
            &cue,
            format!("FILE \"{stem}.bin\" BINARY\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n"),
        )
        .unwrap();
        let mut mem = test_mem(4096);
        let mut cdtv = CdtvController::new();
        cdtv.insert_disc(CdImage::load(&cue).unwrap());
        cdtv.config_write(0x48, 1, 0xE9);
        let mut ring = CdAudioRing::default();
        let _ = command(&mut cdtv, &mut mem, &[0x09, 0, 0, 1, 0, 0, 2]);
        while cdtv.sub_frame.is_empty() {
            cdtv.tick(1, &mut ring);
        }
        let mut symbols = Vec::new();
        for _ in 0..96 {
            wait_for_sbcp(&mut cdtv, &mut ring, 2 * CCK_PER_SUBCODE_SYMBOL).unwrap();
            symbols.push(cdtv.read(0xB0, 1) as u8);
        }
        // R, T, V are bits 5, 3, 1 on the disc and 2, 4, 6 on port A.
        assert!(symbols.iter().all(|&s| s & 0xFC == 0x54), "{symbols:02X?}");
        let q = q_from_port_a(&symbols);
        assert_eq!(&q[..3], &[0x01, 0x01, 0x01]);
        drop(cdtv);
        let _ = std::fs::remove_file(&cue);
        let _ = std::fs::remove_file(&bin);
        let _ = std::fs::remove_file(&sub);
    }

    // ----- bookmark RAM backing file ---------------------------------------

    /// A per-test backing path in the host temp directory, removed first so
    /// a rerun never sees a previous run's file.
    fn bookmark_file(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "copperline-cdtv-bookmark-{}-{name}.bin",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// Advance the controller by `cck` colour clocks in line-sized ticks.
    fn run_cck(cdtv: &mut CdtvController, cck: u32) {
        let mut ring = CdAudioRing::default();
        let mut left = cck;
        while left > 0 {
            let step = left.min(CCK_PER_LINE);
            cdtv.tick(step, &mut ring);
            left -= step;
        }
    }

    #[test]
    fn bookmark_ram_reaches_its_file_a_quarter_second_after_a_change() {
        let path = bookmark_file("flush");
        let mut cdtv = CdtvController::new();
        cdtv.set_bookmark_path(path.clone());
        assert!(cdtv.persistent_bookmark_ram());

        cdtv.battram_write(0x0010, 2, 0xC0DE);
        // Mirrored across the window: $DCC000 is $DC8000 again.
        cdtv.battram_write(0x4020, 1, 0x5A);
        run_cck(&mut cdtv, BOOKMARK_FLUSH_CCK - CCK_PER_LINE);
        assert!(!path.exists(), "written before the flush interval ran out");
        run_cck(&mut cdtv, 2 * CCK_PER_LINE);
        let file = std::fs::read(&path).unwrap();
        assert_eq!(file.len(), BOOKMARK_RAM_BYTES);
        assert_eq!(&file[0x10..0x12], &[0xC0, 0xDE]);
        assert_eq!(file[0x20], 0x5A);
        assert!(!cdtv.battram_dirty);

        // A second machine started on the same file sees the save.
        let mut resumed = CdtvController::new();
        resumed.set_bookmark_path(path.clone());
        assert_eq!(resumed.battram_read(0x0010, 2), 0xC0DE);
        assert_eq!(resumed.battram_read(0x0020, 1), 0x5A);
        drop(cdtv);
        drop(resumed);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bookmark_ram_flushes_before_the_board_is_configured() {
        // The RAM is on the motherboard; the DMAC's autoconfig state has
        // no say in whether a save is kept.
        let path = bookmark_file("unconfigured");
        let mut cdtv = CdtvController::new();
        assert!(cdtv.configured_base.is_none());
        cdtv.set_bookmark_path(path.clone());
        cdtv.battram_write(0, 1, 0x42);
        run_cck(&mut cdtv, BOOKMARK_FLUSH_CCK + CCK_PER_LINE);
        assert_eq!(std::fs::read(&path).unwrap()[0], 0x42);
        drop(cdtv);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rewriting_a_cell_with_its_own_value_writes_no_file() {
        let path = bookmark_file("clean");
        let mut cdtv = CdtvController::new();
        cdtv.set_bookmark_path(path.clone());
        cdtv.battram_write(0x100, 4, 0);
        run_cck(&mut cdtv, 2 * BOOKMARK_FLUSH_CCK);
        cdtv.reset();
        drop(cdtv);
        assert!(!path.exists(), "an unchanged RAM created its backing file");
    }

    #[test]
    fn a_session_only_bookmark_ram_writes_nothing() {
        let mut cdtv = CdtvController::new();
        assert!(!cdtv.persistent_bookmark_ram());
        cdtv.battram_write(0, 1, 0x99);
        run_cck(&mut cdtv, 2 * BOOKMARK_FLUSH_CCK);
        assert_eq!(cdtv.battram_read(0, 1), 0x99);
        assert!(!cdtv.battram_dirty, "nothing to retry without a file");
    }

    #[test]
    fn reset_writes_a_pending_change_and_keeps_the_file_bound() {
        let path = bookmark_file("reset");
        let mut cdtv = CdtvController::new();
        cdtv.set_bookmark_path(path.clone());
        cdtv.battram_write(0x200, 1, 0x77);
        cdtv.reset();
        // The whole RAM, not the empty buffer the reset swaps through.
        let file = std::fs::read(&path).unwrap();
        assert_eq!(file.len(), BOOKMARK_RAM_BYTES);
        assert_eq!(file[0x200], 0x77);
        assert_eq!(cdtv.battram_read(0x200, 1), 0x77);

        // Still bound after the reset: the next change reaches the file.
        cdtv.battram_write(0x201, 1, 0x78);
        run_cck(&mut cdtv, BOOKMARK_FLUSH_CCK + CCK_PER_LINE);
        assert_eq!(std::fs::read(&path).unwrap()[0x201], 0x78);
        drop(cdtv);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dropping_the_controller_writes_a_change_inside_the_flush_interval() {
        let path = bookmark_file("drop");
        let mut cdtv = CdtvController::new();
        cdtv.set_bookmark_path(path.clone());
        cdtv.battram_write(0x3FFF, 1, 0xAB);
        run_cck(&mut cdtv, CCK_PER_LINE);
        assert!(!path.exists());
        drop(cdtv);
        assert_eq!(std::fs::read(&path).unwrap()[0x3FFF], 0xAB);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bookmark_file_of_another_size_loads_what_fits() {
        let path = bookmark_file("sizes");
        std::fs::write(&path, [1u8, 2, 3]).unwrap();
        let mut cdtv = CdtvController::new();
        cdtv.set_bookmark_path(path.clone());
        assert_eq!(cdtv.battram_read(0, 4), 0x0102_0300);
        drop(cdtv);

        let mut long = vec![0x11u8; 2 * BOOKMARK_RAM_BYTES];
        long[BOOKMARK_RAM_BYTES - 1] = 0x22;
        long[BOOKMARK_RAM_BYTES] = 0x33;
        std::fs::write(&path, &long).unwrap();
        let mut cdtv = CdtvController::new();
        cdtv.set_bookmark_path(path.clone());
        assert_eq!(cdtv.battram_read(BOOKMARK_RAM_BYTES as u32 - 1, 1), 0x22);
        // Past the RAM is the mirror of its start, not the file's tail.
        assert_eq!(cdtv.battram_read(BOOKMARK_RAM_BYTES as u32, 1), 0x11);
        // Loading does not rewrite the file.
        run_cck(&mut cdtv, 2 * BOOKMARK_FLUSH_CCK);
        drop(cdtv);
        assert_eq!(std::fs::read(&path).unwrap().len(), 2 * BOOKMARK_RAM_BYTES);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_failed_bookmark_write_is_retried_at_the_next_interval() {
        // A path whose parent is a regular file cannot be created.
        let blocker = bookmark_file("retry-blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let path = blocker.join("cdtv-bookmark.bin");
        let mut cdtv = CdtvController::new();
        cdtv.set_bookmark_path(path.clone());
        cdtv.battram_write(0, 1, 0x61);
        run_cck(&mut cdtv, BOOKMARK_FLUSH_CCK + CCK_PER_LINE);
        assert!(cdtv.battram_dirty, "a failed write must stay pending");

        // The obstacle goes away; the next interval writes the save.
        std::fs::remove_file(&blocker).unwrap();
        run_cck(&mut cdtv, BOOKMARK_FLUSH_CCK + CCK_PER_LINE);
        assert!(!cdtv.battram_dirty);
        assert_eq!(std::fs::read(&path).unwrap()[0], 0x61);
        drop(cdtv);
        let _ = std::fs::remove_dir_all(&blocker);
    }

    #[test]
    fn a_save_state_carries_the_bookmark_ram_but_not_its_file() {
        let path = bookmark_file("state");
        let mut saved = CdtvController::new();
        saved.set_bookmark_path(path.clone());
        saved.battram_write(0x40, 1, 0xA1);
        let mut bytes = Vec::new();
        crate::savestate::chunk::encode(&saved, &mut bytes).unwrap();
        // Without the adoption step a restored controller has no file: a
        // state loaded on another machine cannot write to this one's path.
        let mut restored: CdtvController = crate::savestate::chunk::decode(&bytes).unwrap();
        assert_eq!(restored.battram_read(0x40, 1), 0xA1);
        assert!(!restored.persistent_bookmark_ram());

        // The running session moved on after the state was taken...
        saved.battram_write(0x40, 1, 0xB2);
        run_cck(&mut saved, BOOKMARK_FLUSH_CCK + CCK_PER_LINE);
        assert_eq!(std::fs::read(&path).unwrap()[0x40], 0xB2);
        // ...and loading the state hands its file to the restored
        // controller, which then holds the state's RAM again.
        restored.adopt_bookmark_file_from(&mut saved);
        assert!(restored.persistent_bookmark_ram());
        assert!(!saved.persistent_bookmark_ram());
        run_cck(&mut restored, BOOKMARK_FLUSH_CCK + CCK_PER_LINE);
        assert_eq!(std::fs::read(&path).unwrap()[0x40], 0xA1);
        drop(saved);
        drop(restored);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn loading_a_state_that_matches_the_file_writes_nothing() {
        let path = bookmark_file("state-same");
        let mut live = CdtvController::new();
        live.set_bookmark_path(path.clone());
        let mut bytes = Vec::new();
        crate::savestate::chunk::encode(&live, &mut bytes).unwrap();
        let mut restored: CdtvController = crate::savestate::chunk::decode(&bytes).unwrap();
        restored.adopt_bookmark_file_from(&mut live);
        run_cck(&mut restored, 2 * BOOKMARK_FLUSH_CCK);
        drop(live);
        drop(restored);
        assert!(!path.exists());
    }
}
