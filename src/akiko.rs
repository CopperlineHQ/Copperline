// SPDX-License-Identifier: GPL-3.0-or-later

//! Akiko (CD32 gate array) at $B80000: identification, the
//! chunky-to-planar port, NVRAM lines, and the CD-ROM controller.
//!
//! Register layout, command protocol, and DMA behaviour follow WinUAE's
//! akiko.cpp, the de-facto reference for this undocumented chip:
//!
//! - $B80000.L  ID $C0CACAFE (the CD32 ROM C2P probe checks $CAFE at +2).
//! - $B80004.L  INTREQ (read-only; per-source clear rules below).
//! - $B80008.L  INTENA (only the top byte is writable).
//! - $B80010.L  data DMA base (16 sector slots of 4 KiB each).
//! - $B80014.L  misc DMA base: RX ring at +0, subcode at +$100, TX ring
//!   at +$200 (256-byte rings indexed by the inx registers).
//! - $B80018/19/1A  subcode offset / TX index / RX index (read).
//! - $B8001D/1F  TX/RX ring stop offsets (write; also clear the
//!   matching DMA-done interrupt and restart the DMA).
//! - $B80020.W  PBX: sector-slot enable mask (writes OR in; the
//!   transferred slot's bit reads back clear).
//! - $B80024.L  FLAGS (enable, PBX, TXD/RXD DMA, subcode, raw, ...).
//! - $B80028.B  PIO command/response port (unused by the ROM).
//! - $B80030/32 NVRAM I2C lines (SCL bit 7, SDA bit 6, direction
//!   register at $32) with a 24C08 EEPROM behind them.
//! - $B80038.L  C2P port (8 longwords in, 8 planar longwords out).
//!
//! The drive itself is the CD32's Chinon: commands arrive as
//! checksummed byte strings through the TX ring, responses return
//! through the RX ring. Implemented commands: noop, stop, pause,
//! unpause, multi (seek/play/read), LED, SubQ, and status/firmware.
//! Data sectors DMA into chip RAM as 2352-byte raw frames at 75 (x2
//! speed: 150) sectors per second. CD audio playback streams decoded
//! CD-DA sectors into the host mixer ring (44.1 kHz, the mixer's native
//! rate) and sends the drive's start/end notification packets.

use crate::cdrom::{to_bcd, CdImage, CdTrack, LEADIN_SECTORS, RAW_SECTOR_BYTES};
use crate::chipset::paula::CdAudioRing;

pub const AKIKO_BASE: u32 = 0x00B8_0000;
pub const AKIKO_SIZE: u32 = 0x0001_0000;

const ID: [u8; 4] = [0xC0, 0xCA, 0xCA, 0xFE];

// INTREQ/INTENA bits.
const CDINT_SUBCODE: u32 = 0x8000_0000;
const CDINT_DRIVEXMIT: u32 = 0x4000_0000;
const CDINT_DRIVERECV: u32 = 0x2000_0000;
const CDINT_RXDMADONE: u32 = 0x1000_0000;
const CDINT_TXDMADONE: u32 = 0x0800_0000;
const CDINT_PBX: u32 = 0x0400_0000;
const CDINT_OVERFLOW: u32 = 0x0200_0000;

// FLAGS bits.
const CDFLAG_SUBCODE: u32 = 0x8000_0000;
const CDFLAG_TXD: u32 = 0x4000_0000;
const CDFLAG_RXD: u32 = 0x2000_0000;
#[allow(dead_code)]
const CDFLAG_CAS: u32 = 0x1000_0000;
const CDFLAG_PBX: u32 = 0x0800_0000;
const CDFLAG_ENABLE: u32 = 0x0400_0000;
#[allow(dead_code)]
const CDFLAG_RAW: u32 = 0x0200_0000;

const CDS_PLAYING: u8 = 0x08;
const CDS_ERROR: u8 = 0x80;
const CH_ERR_CHECKSUM: u8 = 0x88;
const CH_ERR_BADCOMMAND: u8 = 0x80;
const CH_ERR_NODISK: u8 = 0xF8;

const FIRMWARE_VERSION: &[u8; 18] = b"CHINON  O-658-2 24";

/// Lengths of the drive commands (payload bytes after the command byte,
/// excluding the checksum), indexed by the low command nibble. -1 =
/// unknown command.
const COMMAND_LENGTHS: [i8; 16] = [1, 2, 1, 1, 12, 2, 1, 1, 4, 1, 2, -1, -1, -1, -1, -1];

/// Each TOC entry is returned three times in a row, like the real drive.
const TOC_REPEAT: u32 = 3;

/// Colour clocks per 1/75th second (one single-speed CD frame).
const CCK_PER_CD_FRAME: u32 = crate::chipset::paula::PAULA_CLOCK_HZ / 75;
/// One subcode frame per sector: 98 bytes of P-W symbols less the two
/// sync patterns, as the drive hands them to Akiko.
const SUBCODE_FRAME_BYTES: usize = 96;

/// The drive firmware returns its cached TOC over the command channel,
/// independently of the 75-sector/s optical data path. A single-track TOC
/// is 12 packets after the mandated three copies of each point; 600 packets/s
/// lets the controller deliver that table inside the boot ROM's first media
/// probe. Sector data and CD-DA remain at their physical 75/150 Hz cadence.
const CCK_PER_TOC_PACKET: u32 = crate::chipset::paula::PAULA_CLOCK_HZ / 600;
/// Cold spin-up: emulated seconds from power-on mount until the lead-in
/// TOC is readable. Back-solved from a real-CD32 boot video (Kickstart
/// grey until ~11.5 s, boot screen, startup-sequence entry at 14.31 s
/// per the cd32-probe ENTRYTIM row) against the calibrated CPU and
/// locate models.
const COLD_SPIN_UP_SECS: f64 = 8.8;

/// Warm spin-up after a runtime disc change: the drive is already
/// powered and settles faster than a cold start.
const WARM_SPIN_UP_SECS: f64 = 3.5;

/// TX/RX DMA restart delay: ~3 scanlines, expressed in colour clocks.
const DMA_RESTART_DELAY_CCK: u32 = 3 * 227;

/// Bound on the drive's unparsed command bytes before the TX DMA stalls.
/// The real buffer's size is unmeasured; this only has to clear the
/// deepest legitimate backlog, Kickstart's one LED packet per TOC entry
/// on a 99-track audio CD (102 points x 3 copies x 3 bytes), with room.
const TX_FIFO_BYTES: usize = 4096;

/// Drive-microcontroller command turnaround: a received command executes
/// this much emulated time after its last byte arrives, roughly 1 ms.
/// The real Chinon takes at least that long, and drivers depend on the
/// asynchrony: they finish arming DMA windows, interrupt enables, and
/// flag bits after kicking the command, then sleep until the completion
/// interrupt. Executing synchronously inside the guest's register write
/// delivers the response before the driver has finished arming, which
/// inverts its interrupt handshake (observed with AROS cd.device).
/// TODO: measure the real command latency per opcode on CD32 hardware.
const CMD_EXEC_DELAY_CCK: u32 = crate::chipset::paula::PAULA_CLOCK_HZ / 1000;

fn get_long_byte(value: u32, offset: u32) -> u8 {
    (value >> (8 * (3 - offset))) as u8
}

fn put_long_byte(value: &mut u32, offset: u32, byte: u8) {
    let shift = 8 * (3 - offset);
    *value = (*value & !(0xFF << shift)) | (u32::from(byte) << shift);
}

fn from_bcd(v: u8) -> u32 {
    u32::from(v >> 4) * 10 + u32::from(v & 0x0F)
}

/// BCD MM:SS:FF (as found in drive commands) to a file-relative sector
/// number; negative when inside the lead-in.
fn bcd_msf_to_lsn(msf: &[u8]) -> i64 {
    let m = from_bcd(msf[0]) as i64;
    let s = from_bcd(msf[1]) as i64;
    let f = from_bcd(msf[2]) as i64;
    (m * 60 + s) * 75 + f - i64::from(LEADIN_SECTORS)
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
struct TocEntry {
    /// Track number 1-99, or 0xA0/0xA1/0xA2 for the session entries.
    point: u8,
    /// Q-channel control nibble (0x04 = data track).
    control: u8,
    /// File-relative start sector (meaningless for A0/A1).
    address: u32,
}

/// CD32 NVRAM: a 24C08 I2C EEPROM (1024 bytes) on Akiko's $B80030
/// lines (bit 7 = SCL, bit 6 = SDA, $B80032 = direction register).
/// Implements the I2C slave protocol: START/STOP detection, device
/// address with the block bits, word address, sequential reads and
/// page writes with ACKs. Contents persist to `path` when given.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Nvram {
    memory: Vec<u8>,
    path: Option<std::path::PathBuf>,
    dirty: bool,

    // Line state as last seen (true = high).
    scl: bool,
    sda: bool,
    // Slave drive on SDA (true = pulling low).
    sda_drive_low: bool,

    state: I2cState,
    /// Current byte being shifted, MSB first.
    shift: u8,
    bit_count: u8,
    /// Memory address counter: block bits from the device address plus
    /// the word address byte.
    address: u16,
    /// Transaction phase after the device address byte.
    phase: I2cPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum I2cState {
    Idle,
    /// Receiving a byte from the master (device addr, word addr, data).
    Receive,
    /// Slave ACK clock for a received byte.
    AckOut,
    /// Sending a byte to the master (read transaction).
    Send,
    /// Master ACK/NAK clock after a sent byte.
    AckIn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum I2cPhase {
    DeviceAddress,
    WordAddress,
    Write,
    Read,
}

impl Nvram {
    const SIZE: usize = 1024;

    fn new(path: Option<std::path::PathBuf>) -> Self {
        let mut memory = vec![0u8; Self::SIZE];
        if let Some(path) = &path {
            match std::fs::read(path) {
                Ok(data) => {
                    let n = data.len().min(Self::SIZE);
                    memory[..n].copy_from_slice(&data[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => log::warn!("cd32 nvram: reading {}: {e}", path.display()),
            }
        }
        Self {
            memory,
            path,
            dirty: false,
            scl: true,
            sda: true,
            sda_drive_low: false,
            state: I2cState::Idle,
            shift: 0,
            bit_count: 0,
            address: 0,
            phase: I2cPhase::DeviceAddress,
        }
    }

    /// SDA as the CPU reads it: low when either side drives it low.
    fn sda_read(&self, cpu_drives: bool, cpu_level: bool) -> bool {
        let cpu = !cpu_drives || cpu_level;
        cpu && !self.sda_drive_low
    }

    /// Feed the line levels after a register write. `scl`/`sda` are the
    /// resolved bus levels from the CPU side (input direction = high).
    fn set_lines(&mut self, scl: bool, sda: bool) {
        let scl_was = self.scl;
        let sda_was = self.sda;
        self.scl = scl;
        self.sda = sda;

        // START: SDA falls while SCL high. STOP: SDA rises while SCL high.
        if scl_was && scl {
            if sda_was && !sda {
                self.state = I2cState::Receive;
                self.phase = I2cPhase::DeviceAddress;
                self.shift = 0;
                self.bit_count = 0;
                self.sda_drive_low = false;
                return;
            }
            if !sda_was && sda {
                self.stop();
                return;
            }
        }

        if !scl_was && scl {
            // Rising clock: sample.
            match self.state {
                I2cState::Receive => {
                    self.shift = (self.shift << 1) | u8::from(sda);
                    self.bit_count += 1;
                }
                // Master NAKs (high) to end a read.
                I2cState::AckIn if sda => {
                    self.state = I2cState::Idle;
                }
                _ => {}
            }
        } else if scl_was && !scl {
            // Falling clock: change outputs.
            match self.state {
                I2cState::Receive if self.bit_count == 8 => {
                    self.byte_received();
                }
                I2cState::AckOut => {
                    self.sda_drive_low = false;
                    if self.phase == I2cPhase::Read {
                        self.load_send_byte();
                    } else {
                        self.state = I2cState::Receive;
                        self.shift = 0;
                        self.bit_count = 0;
                    }
                }
                I2cState::Send => {
                    if self.bit_count == 0 {
                        // Byte fully clocked out: release for master ACK.
                        self.sda_drive_low = false;
                        self.state = I2cState::AckIn;
                    } else {
                        self.output_next_bit();
                    }
                }
                I2cState::AckIn => {
                    // Master ACKed: continue sequential read.
                    self.address = (self.address + 1) % Self::SIZE as u16;
                    self.load_send_byte();
                }
                _ => {}
            }
        }
    }

    fn byte_received(&mut self) {
        let byte = self.shift;
        match self.phase {
            I2cPhase::DeviceAddress => {
                // 1010 xBB R/W: a 24C08 answers device code 1010 with the
                // 256-byte block index in bits 2-1.
                if byte & 0xF0 != 0xA0 {
                    self.state = I2cState::Idle;
                    return;
                }
                let block = u16::from((byte >> 1) & 0x03);
                self.address = (self.address & 0x00FF) | (block << 8);
                if byte & 1 != 0 {
                    self.phase = I2cPhase::Read;
                } else {
                    self.phase = I2cPhase::WordAddress;
                }
            }
            I2cPhase::WordAddress => {
                self.address = (self.address & 0x0300) | u16::from(byte);
                self.phase = I2cPhase::Write;
            }
            I2cPhase::Write => {
                let addr = usize::from(self.address) % Self::SIZE;
                if self.memory[addr] != byte {
                    self.memory[addr] = byte;
                    self.dirty = true;
                }
                // Page writes wrap inside the 16-byte page.
                let page = self.address & !0x000F;
                self.address = page | ((self.address + 1) & 0x000F);
            }
            I2cPhase::Read => {}
        }
        // ACK the byte: drive SDA low for the 9th clock.
        self.sda_drive_low = true;
        self.state = I2cState::AckOut;
    }

    fn load_send_byte(&mut self) {
        self.shift = self.memory[usize::from(self.address) % Self::SIZE];
        self.bit_count = 8;
        self.state = I2cState::Send;
        self.output_next_bit();
    }

    fn output_next_bit(&mut self) {
        self.bit_count -= 1;
        let bit = self.shift & (1 << self.bit_count) != 0;
        self.sda_drive_low = !bit;
    }

    fn stop(&mut self) {
        self.state = I2cState::Idle;
        self.sda_drive_low = false;
        self.phase = I2cPhase::DeviceAddress;
        if self.dirty {
            if let Some(path) = &self.path {
                if let Err(e) = crate::paths::ensure_parent(path)
                    .and_then(|()| std::fs::write(path, &self.memory))
                {
                    // Stay dirty so the next STOP retries: a transient
                    // host error must not lose the EEPROM contents (save
                    // games) until the guest happens to write them again.
                    log::warn!("cd32 nvram: writing {}: {e}", path.display());
                    return;
                }
            }
            self.dirty = false;
        }
    }
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct C2p {
    buffer: [u32; 8],
    write_offset: usize,
    read_offset: Option<usize>,
    result: [u32; 8],
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Akiko {
    c2p: C2p,
    nvram_lines: u8,
    nvram_direction: u8,
    nvram: Nvram,

    disc: Option<CdImage>,
    toc: Vec<TocEntry>,
    /// A disc waiting in the tray after a runtime insert: mounted (and a
    /// media-status packet volunteered) once `insert_delay_cck` of emulated
    /// time elapses, so the drive reports the absent->present transition a
    /// real tray produces instead of an instantaneous swap.
    pending_disc: Option<CdImage>,
    insert_delay_cck: i64,

    intreq: u32,
    intena: u32,
    subcodeoffset: u8,
    addressdata: u32,
    addressmisc: u32,
    subcode_address: u32,
    cdrx_address: u32,
    cdtx_address: u32,
    flags: u32,
    pbx: u32,
    cdcomtxinx: u8,
    cdcomrxinx: u8,
    cdcomtxcmp: u8,
    cdcomrxcmp: u8,
    /// TX byte offset within its 256-byte page. Like rx_dma_offset it is
    /// kept separate from the visible comparator index for save-state
    /// stability and masked to the page on every DMA access: the index
    /// registers are eight-bit and Kickstart's command producer wraps a
    /// packet's bytes at index $FF.
    tx_dma_offset: u16,
    /// RX byte offset within its 256-byte page. Keep this separate from the
    /// visible comparator index so an in-flight response survives save-state
    /// round trips, but mask it to the RX page on every DMA access.
    rx_dma_offset: u16,
    tx_dma_delay_cck: u32,
    rx_dma_delay_cck: u32,

    command_buffer: [u8; 32],
    command_length: usize,
    command: u8,
    command_active: u32,
    checksum_error: bool,
    unknown_command: bool,

    result_buffer: [u8; 32],
    receive_length: usize,
    receive_offset: usize,
    last_rx: u8,

    /// 0 = cold, 1 = initial media status pushed, 2 = host ran INFO.
    cd_initialized: u8,
    /// A runtime insert/eject happened: volunteer a media-status packet
    /// (as the real drive does on a disc change) once the channel is idle.
    media_notify: bool,
    door: u8,
    /// Colour clocks until the mechanism has spun the freshly loaded disc
    /// up and read its lead-in: until this expires the drive acks a TOC
    /// request but delivers no entries, which is what holds the KS
    /// driver's first TOC transaction (and every io queued behind it,
    /// the CDUI's one-shot CD_CHANGESTATE included) open until the disc
    /// is really readable. Real-CD32 video of a cold boot with a disc in
    /// the drive shows ~10 s of Kickstart grey before the boot screen;
    /// the CD32's mechanism is famously slow to spin up. A warm guest
    /// reset keeps the drive spinning and the lead-in cached, so reset()
    /// clears the gate.
    toc_spin_up_cck: i64,
    /// Command bytes the TX DMA has delivered to the drive but its
    /// microcontroller has not parsed yet. Akiko's DMA engine drains the
    /// guest's 256-byte command ring whenever the comparator indices
    /// differ; the drive parses one command at a time and, while a
    /// lead-in dump streams, none at all (see `parse_commands`). The
    /// buffer is what keeps the ring index moving meanwhile: Kickstart's
    /// driver queues a 3-byte LED packet for every TOC entry it receives,
    /// which on a many-track disc (39 tracks is 126 packets, 378 bytes)
    /// laps the ring if nothing consumes it, and the lapped bytes then
    /// parse as garbage that fails its checksum.
    tx_fifo: std::collections::VecDeque<u8>,
    toc_counter: i32,
    data_offset: i64,
    /// Exclusive end sector supplied by the drive's READ DATA command.
    data_end: i64,
    sector_counter: u32,
    current_sector: i64,
    seek_delay: u32,
    speed: u32,

    playing: bool,
    paused: bool,
    /// Pending audio notification: >0 counts CD frames down to a
    /// play-start packet, <0 schedules end (-1), error (-3) packets.
    audio_notify: i32,
    /// Current and one-past-end disc sectors of CD audio playback.
    play_position: i64,
    play_end: i64,
    /// Drop any buffered host CD audio on the next tick (stop command).
    flush_cd_audio: bool,
    /// Colour-clock countdown pacing audio sector production at 75 Hz.
    audio_counter_cck: i32,

    /// Colour-clock countdowns driving sector DMA and TOC pacing.
    read_counter_cck: i32,
    frame_counter_cck: i32,
    frame_sync: bool,
}

impl Default for Akiko {
    fn default() -> Self {
        Self {
            c2p: C2p::default(),
            nvram_lines: 0,
            nvram_direction: 0,
            nvram: Nvram::new(None),
            disc: None,
            toc: Vec::new(),
            pending_disc: None,
            insert_delay_cck: -1,
            toc_spin_up_cck: 0,
            tx_fifo: std::collections::VecDeque::new(),
            intreq: 0,
            intena: 0,
            subcodeoffset: 0,
            addressdata: 0,
            addressmisc: 0,
            subcode_address: 0,
            cdrx_address: 0,
            cdtx_address: 0,
            flags: 0,
            pbx: 0,
            cdcomtxinx: 0,
            cdcomrxinx: 0,
            cdcomtxcmp: 0,
            cdcomrxcmp: 0,
            tx_dma_offset: 0,
            rx_dma_offset: 0,
            tx_dma_delay_cck: 0,
            rx_dma_delay_cck: 0,
            command_buffer: [0; 32],
            command_length: 0,
            command: 0,
            command_active: 0,
            checksum_error: false,
            unknown_command: false,
            result_buffer: [0; 32],
            receive_length: 0,
            receive_offset: 0,
            last_rx: 0,
            cd_initialized: 0,
            media_notify: false,
            door: 1,
            toc_counter: -1,
            data_offset: -1,
            data_end: -1,
            sector_counter: 0,
            current_sector: -1,
            seek_delay: 0,
            speed: 1,
            playing: false,
            paused: false,
            audio_notify: 0,
            play_position: 0,
            play_end: 0,
            flush_cd_audio: false,
            audio_counter_cck: 0,
            read_counter_cck: 0,
            frame_counter_cck: 0,
            frame_sync: false,
        }
    }
}

impl Akiko {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mount a CD image. The TOC is built once here. When the controller
    /// is already running (runtime disc swap, not boot-time mount), the
    /// drive volunteers a media-status packet so the OS sees the change.
    pub fn insert_disc(&mut self, disc: CdImage) {
        self.toc = build_toc(&disc);
        self.disc = Some(disc);
        self.current_sector = -1;
        self.media_notify = self.cd_initialized != 0;
        self.toc_spin_up_cck =
            (COLD_SPIN_UP_SECS * f64::from(crate::chipset::paula::PAULA_CLOCK_HZ)) as i64;
    }

    /// Park a disc in the tray and mount it after `secs` of emulated tray
    /// time. Any disc currently in the drive is ejected first, so the
    /// controller volunteers a media-absent packet now and a media-present
    /// packet once the tray settles -- the absent->present transition a real
    /// drive produces, which the CD32 firmware needs to spot a disc change.
    pub fn insert_disc_after(&mut self, disc: CdImage, secs: f64) {
        self.eject_disc();
        self.pending_disc = Some(disc);
        self.insert_delay_cck =
            (secs.max(0.0) * f64::from(crate::chipset::paula::PAULA_CLOCK_HZ)) as i64;
    }

    /// Whether a disc is mounted or still waiting in the tray.
    pub fn has_disc(&self) -> bool {
        self.disc.is_some() || self.pending_disc.is_some()
    }

    /// Whether the drive is actively working: streaming CD audio, a TOC
    /// dump in progress, or a data read with the host still feeding PBX
    /// buffer slots (the same gate as run_sector_read). Feeds the
    /// status-bar CD LED.
    pub fn activity_led_on(&self) -> bool {
        (self.playing && !self.paused)
            || self.toc_counter >= 0
            || (self.data_offset >= 0
                && self.pbx != 0
                && self.flags & CDFLAG_ENABLE != 0
                && self.flags & CDFLAG_PBX != 0)
    }

    /// Track under the emulated optical head, or `None` with no mounted
    /// medium. The first program track is reported before the first seek.
    pub fn current_track(&self) -> Option<u8> {
        self.disc.as_ref()?;
        let sector = self.current_sector.max(0) as u32;
        self.toc
            .iter()
            .skip(3)
            .rev()
            .find(|entry| sector >= entry.address)
            .or_else(|| self.toc.get(3))
            .map(|entry| entry.point)
    }

    /// Remove the disc: stop playback, drop buffered audio, and volunteer
    /// a media-status packet so the OS notices the removal.
    pub fn eject_disc(&mut self) {
        // Cancel any disc still waiting in the tray from a delayed insert.
        self.pending_disc = None;
        self.insert_delay_cck = -1;
        if self.disc.take().is_none() {
            // Nothing was mounted (the OS never saw a present disc), so no
            // removal notification is owed.
            return;
        }
        self.toc.clear();
        self.playing = false;
        self.paused = false;
        self.flush_cd_audio = true;
        self.audio_notify = 0;
        self.toc_counter = -1;
        self.data_offset = -1;
        self.data_end = -1;
        self.media_notify = self.cd_initialized != 0;
        log::info!("akiko: disc ejected");
    }

    /// System reset: clear controller state but keep the mounted disc
    /// and the NVRAM contents.
    pub fn reset(&mut self) {
        // A guest reset does not stop the mechanism: the disc keeps
        // spinning and the lead-in stays cached, which is why a warm
        // CD32 reboot reaches the boot screen far faster than a cold
        // power-on (toc_spin_up_cck stays cleared below).
        let disc = self.disc.take();
        let toc = std::mem::take(&mut self.toc);
        let path = self.nvram.path.take();
        let memory = std::mem::take(&mut self.nvram.memory);
        *self = Self::default();
        self.disc = disc;
        self.toc = toc;
        self.nvram.path = path;
        self.nvram.memory = memory;
    }

    /// A power cycle stops the mechanism: the next lead-in dump pays the
    /// full cold spin-up again. A guest reset alone keeps the disc
    /// spinning (see `reset`), so the Bus calls this only on the
    /// power-on path.
    pub fn rearm_cold_spin_up(&mut self) {
        if self.disc.is_some() {
            self.toc_spin_up_cck =
                (COLD_SPIN_UP_SECS * f64::from(crate::chipset::paula::PAULA_CLOCK_HZ)) as i64;
        }
    }

    /// Persist NVRAM to (and preload it from) `path`.
    pub fn set_nvram_path(&mut self, path: std::path::PathBuf) {
        self.nvram = Nvram::new(Some(path));
    }

    /// Whether the CD32 EEPROM is backed by a host file. Its I2C STOP
    /// condition flushes dirty bytes immediately, which cannot be undone by
    /// restoring a speculative machine snapshot.
    pub fn persistent_nvram(&self) -> bool {
        self.nvram.path.is_some()
    }

    /// Resolved I2C bus levels from the CPU-side latches: an input
    /// direction floats high through the pull-ups.
    fn nvram_bus_levels(&self) -> (bool, bool) {
        let scl = self.nvram_direction & 0x80 == 0 || self.nvram_lines & 0x80 != 0;
        let sda = self.nvram_direction & 0x40 == 0 || self.nvram_lines & 0x40 != 0;
        (scl, sda)
    }

    /// The INT2 (PORTS) line into Paula, level-fed like Gayle's.
    pub fn int2_line(&self) -> bool {
        self.intreq & self.intena != 0
    }

    // ----- CPU access ------------------------------------------------------

    pub fn read(&mut self, addr: u32, size: usize, mem: &mut (impl DmaSpace + ?Sized)) -> u32 {
        let offset = addr & 0xFFFF;
        if offset >= 0x8000 {
            return 0;
        }
        let mut value = 0u32;
        for i in 0..size as u32 {
            value = (value << 8) | u32::from(self.read_byte((offset + i) & 0x3F));
        }
        self.c2p_read_step(offset);
        self.run_internal(mem);
        value
    }

    pub fn write(
        &mut self,
        addr: u32,
        size: usize,
        value: u32,
        mem: &mut (impl DmaSpace + ?Sized),
    ) {
        let offset = addr & 0xFFFF;
        if offset >= 0x8000 {
            return;
        }
        // Low byte lane first, like the hardware: the write landing on
        // byte 0 (a longword's MSB) completes a C2P entry.
        for i in (0..size as u32).rev() {
            let shift = 8 * (size as u32 - 1 - i);
            self.write_byte((offset + i) & 0x3F, (value >> shift) as u8);
        }
        self.run_internal(mem);
    }

    fn read_byte(&mut self, offset: u32) -> u8 {
        match offset {
            0x00..=0x03 => ID[offset as usize],
            // INTREQ / INTENA (and the read-only INTENA mirror).
            // The status read returns the raw request latches; INTENA
            // gates only the INT2 line (see int2_line()). Games driving
            // the DRIVE port directly poll CDINTREQ for DRIVEXMIT/DRIVERECV
            // with those sources never enabled in INTENA -- Jim Power's
            // CD32 loader spins on bit 30 before each PIO command byte --
            // so a masked read hides the transmitter-ready latch and
            // hangs them. WinUAE and MAME both return the raw latches.
            // (An earlier masked reading here papered over the AROS
            // cd.device INT2 server reacting to stale completion latches
            // it had never armed; that server now masks CDINTREQ with
            // its own enable state, and the bundled ROM carries the
            // fix.)
            0x04..=0x07 => get_long_byte(self.intreq, offset - 0x04),
            0x08..=0x0B => get_long_byte(self.intena, offset - 0x08),
            0x0C..=0x0F => get_long_byte(self.intena, offset - 0x0C),
            // 0x18-0x1B mirror 0x10/0x14/0x1C.
            0x10 | 0x14 | 0x18 | 0x1C => self.subcodeoffset,
            0x11 | 0x15 | 0x19 | 0x1D => self.cdcomtxinx,
            0x12 | 0x16 | 0x1A | 0x1E => self.cdcomrxinx,
            0x13 | 0x17 | 0x1B | 0x1F => 0,
            0x20 | 0x21 => get_long_byte(self.pbx, offset - 0x20 + 2),
            0x24..=0x27 => get_long_byte(self.flags, offset - 0x24),
            0x28 => {
                // PIO response port (the ROM uses RX DMA instead).
                if self.flags & CDFLAG_RXD == 0 && self.receive_offset < self.receive_length {
                    self.last_rx = self.result_buffer[self.receive_offset];
                    self.receive_offset += 1;
                    if self.receive_offset == self.receive_length {
                        self.intreq &= !CDINT_DRIVERECV;
                        self.receive_length = 0;
                        self.intreq |= CDINT_DRIVEXMIT;
                    }
                } else {
                    self.intreq &= !CDINT_DRIVERECV;
                }
                self.last_rx
            }
            0x30 => {
                let (scl, sda) = self.nvram_bus_levels();
                let sda = self.nvram.sda_read(self.nvram_direction & 0x40 != 0, sda);
                (u8::from(scl) << 7) | (u8::from(sda) << 6)
            }
            0x32 => self.nvram_direction,
            0x38..=0x3B => self.c2p_read_byte(offset),
            _ => 0,
        }
    }

    fn write_byte(&mut self, offset: u32, value: u8) {
        match offset {
            0x08..=0x0B => {
                put_long_byte(&mut self.intena, offset - 0x08, value);
                self.intena &= 0xFF00_0000;
                log::trace!(
                    "akiko: INTENA={:08X} (intreq={:08X})",
                    self.intena,
                    self.intreq
                );
            }
            0x10..=0x13 => {
                put_long_byte(&mut self.addressdata, offset - 0x10, value);
                self.addressdata &= 0x00FF_F000;
            }
            0x14..=0x17 => {
                put_long_byte(&mut self.addressmisc, offset - 0x14, value);
                self.addressmisc &= 0x00FF_FC00;
                self.subcode_address = self.addressmisc | 0x100;
                self.cdrx_address = self.addressmisc;
                self.cdtx_address = self.addressmisc | 0x200;
            }
            0x18 => self.intreq &= !CDINT_SUBCODE,
            0x1D => {
                self.intreq &= !CDINT_TXDMADONE;
                if self.cdcomtxinx == self.cdcomtxcmp
                    && (self.command_active != 0 || self.command_length == 0)
                {
                    // A fresh DMA window starts at base + the visible index.
                    // Mid-command comparator extensions leave the offset in
                    // place; both fold to the same page position on access.
                    self.tx_dma_offset = u16::from(self.cdcomtxinx);
                }
                self.cdcomtxcmp = value;
                self.tx_dma_delay_cck = DMA_RESTART_DELAY_CCK;
                log::trace!(
                    "akiko: TXCMP={:02X} (txinx={:02X} intreq={:08X})",
                    value,
                    self.cdcomtxinx,
                    self.intreq
                );
            }
            0x1F => {
                self.intreq &= !CDINT_RXDMADONE;
                if self.cdcomrxinx == self.cdcomrxcmp && self.receive_offset == 0 {
                    // New response packet/window: restart at base + index.
                    // An extension after a partial response instead preserves
                    // the current in-page position in rx_dma_offset.
                    self.rx_dma_offset = u16::from(self.cdcomrxinx);
                }
                self.cdcomrxcmp = value;
                self.rx_dma_delay_cck = DMA_RESTART_DELAY_CCK;
                log::trace!(
                    "akiko: RXCMP={:02X} (rxinx={:02X} intreq={:08X})",
                    value,
                    self.cdcomrxinx,
                    self.intreq
                );
            }
            0x20 | 0x21 => {
                // PBX writes OR slots in; the flag gate can hold it at 0.
                let previous = self.pbx;
                put_long_byte(&mut self.pbx, offset - 0x20 + 2, value);
                self.pbx |= previous;
                self.pbx &= 0xFFFF;
                if self.flags & CDFLAG_PBX == 0 {
                    self.pbx = 0;
                }
                self.intreq &= !CDINT_PBX;
            }
            0x24..=0x27 => {
                let previous = self.flags;
                put_long_byte(&mut self.flags, offset - 0x24, value);
                if self.flags & CDFLAG_ENABLE != 0 && previous & CDFLAG_ENABLE == 0 {
                    self.sector_counter = 0;
                    self.intreq &= !CDINT_OVERFLOW;
                }
                if self.flags & CDFLAG_PBX == 0 {
                    self.pbx = 0;
                }
                self.flags &= 0xFF80_0000;
            }
            0x28 => {
                // PIO command port (the ROM uses TX DMA instead).
                if self.flags & CDFLAG_TXD == 0 {
                    self.intreq &= !CDINT_DRIVEXMIT;
                    if self.tx_fifo.len() < TX_FIFO_BYTES {
                        self.tx_fifo.push_back(value);
                    }
                    self.parse_commands();
                    if self.can_send_command() {
                        self.intreq |= CDINT_DRIVEXMIT;
                    }
                }
            }
            0x30 => {
                self.nvram_lines = value;
                let (scl, sda) = self.nvram_bus_levels();
                self.nvram.set_lines(scl, sda);
            }
            0x32 => {
                self.nvram_direction = value;
                let (scl, sda) = self.nvram_bus_levels();
                self.nvram.set_lines(scl, sda);
            }
            0x38..=0x3B => self.c2p_write_byte(offset, value),
            _ => {}
        }
    }

    // ----- periodic work ---------------------------------------------------

    /// Advance the controller by `cck` colour clocks: sector DMA pacing,
    /// DMA restart delays, audio playback, and status push-backs.
    pub fn tick(
        &mut self,
        cck: u32,
        mem: &mut (impl DmaSpace + ?Sized),
        cd_audio: &mut CdAudioRing,
    ) {
        self.tx_dma_delay_cck = self.tx_dma_delay_cck.saturating_sub(cck);
        self.rx_dma_delay_cck = self.rx_dma_delay_cck.saturating_sub(cck);

        // Delayed disc insert: once the tray settles, mount the disc and
        // volunteer a media-status packet (present), like the real drive's
        // change notification at the end of tray motion.
        if self.pending_disc.is_some() {
            self.insert_delay_cck -= i64::from(cck);
            if self.insert_delay_cck < 0 {
                let disc = self.pending_disc.take().unwrap();
                self.toc = build_toc(&disc);
                self.disc = Some(disc);
                self.current_sector = -1;
                self.media_notify = self.cd_initialized != 0;
                // A tray insert before the host driver has spoken to the
                // drive is a disc sitting in the drive at power-on: the
                // mechanism does its full cold spin-up. A change on a
                // warmed-up drive settles faster.
                let spin_up = if self.cd_initialized < 2 {
                    COLD_SPIN_UP_SECS
                } else {
                    WARM_SPIN_UP_SECS
                };
                self.toc_spin_up_cck =
                    (spin_up * f64::from(crate::chipset::paula::PAULA_CLOCK_HZ)) as i64;
                log::info!("akiko: disc inserted (delayed), media-status notification queued");
            }
        }

        if self.command_active > 0 {
            self.command_active = self.command_active.saturating_sub(cck);
            if self.command_active == 0 {
                if self.receive_length > 0 {
                    // The response channel still holds an undelivered
                    // packet (an unsolicited notification can land while
                    // the turnaround runs): executing now would clobber
                    // it in result_buffer. Hold the command until the
                    // ring drains; the host cannot send another one
                    // meanwhile (can_send_command gates on both).
                    self.command_active = 1;
                } else if self.intreq & self.intena & CDINT_TXDMADONE != 0 {
                    // The three-wire drive link is half duplex. Akiko may
                    // already have the drive's reply buffered, but it does
                    // not expose RX completion while an enabled TX-DMA
                    // completion is still awaiting service. Keeping those
                    // edges distinct matters to the ROM driver: it uses one
                    // shared INT2 server for both channels and advances the
                    // response ring according to the completion it observed.
                    self.command_active = 1;
                } else {
                    self.execute_command();
                }
            }
        }

        self.read_counter_cck -= cck as i32;
        if self.read_counter_cck <= 0 {
            self.read_counter_cck += (CCK_PER_CD_FRAME / self.speed.max(1)) as i32;
            if self.seek_delay > 0 {
                self.seek_delay -= 1;
            } else {
                self.run_sector_read(mem);
            }
        }

        if self.toc_spin_up_cck > 0 {
            self.toc_spin_up_cck -= i64::from(cck);
        }
        self.frame_counter_cck -= cck as i32;
        if self.frame_counter_cck <= 0 {
            self.frame_counter_cck += CCK_PER_TOC_PACKET as i32;
            // No lead-in data until the mechanism has spun up: the TOC
            // request stays acknowledged but unanswered, like the real
            // drive's long first read of a cold disc.
            self.frame_sync = self.toc_spin_up_cck <= 0;
        }

        if self.flush_cd_audio {
            self.flush_cd_audio = false;
            cd_audio.clear();
        }
        // CD-DA always plays at single speed: stream one decoded sector
        // into the host mixer ring per CD frame.
        self.audio_counter_cck -= cck as i32;
        if self.audio_counter_cck <= 0 {
            self.audio_counter_cck += CCK_PER_CD_FRAME as i32;
            self.stream_audio_sector(mem, cd_audio);
        }

        self.handler();
        self.run_internal(mem);
    }

    /// Produce the next CD-DA sector of the running play command, with
    /// the subcode frame the drive reads alongside it.
    fn stream_audio_sector(
        &mut self,
        mem: &mut (impl DmaSpace + ?Sized),
        cd_audio: &mut CdAudioRing,
    ) {
        if !self.playing || self.paused {
            return;
        }
        if !cd_audio.wants_sector() {
            return; // mixer is behind; retry next CD frame
        }
        let Some(disc) = self.disc.as_mut() else {
            return;
        };
        if self.play_position >= self.play_end || self.play_position < 0 {
            self.playing = false;
            self.audio_notify = -1; // play end notification
            return;
        }
        let sector = self.play_position as u32;
        self.current_sector = self.play_position;
        if disc.is_audio_sector(sector) {
            let mut raw = [0u8; crate::cdrom::RAW_SECTOR_BYTES];
            if disc.read_audio_sector(sector, &mut raw).is_ok() {
                cd_audio.push_sector(&raw);
            }
        }
        self.play_position += 1;
        // The pickup decodes P-W subcode for every frame it plays, not
        // only for data reads: this stream is what the KS cd.device's
        // subcode interrupt server turns into CD_ADDFRAMEINT calls and
        // CD_QCODE positions while audio plays.
        self.deliver_subcode(mem, sector);
    }

    /// Status push-backs the drive volunteers between commands.
    fn handler(&mut self) {
        if self.receive_length != 0 {
            return;
        }
        if self.cd_initialized == 0 {
            // First status is 0x0a when booted with a CD inserted.
            if self.disc.is_some() {
                let len = self.command_media_status();
                self.start_return_data(len);
            }
            self.cd_initialized = 1;
            return;
        }
        // Runtime disc insert/eject: push the new media status as soon as
        // the receive channel is free, like the real drive's change
        // notification. WinUAE volunteers this even while a command is
        // active (cdrom_can_return_data only checks the receive length), so
        // we do not gate on command_active here -- gating on it lets the
        // boot ROM's status polling starve the notification.
        if self.media_notify {
            self.media_notify = false;
            let len = self.command_media_status();
            self.start_return_data(len);
            return;
        }
        if self.cd_initialized < 2 {
            return;
        }
        match self.audio_notify {
            n if n > 1 => self.audio_notify -= 1,
            1 => {
                // Play started.
                let len = self.playend_notify(0);
                self.start_return_data(len);
                self.audio_notify = 0;
            }
            -1 => {
                // Play finished.
                let len = self.playend_notify(1);
                self.start_return_data(len);
                self.audio_notify = 0;
            }
            -3 => {
                // Play failed (illegal address).
                let len = self.playend_notify(-1);
                self.start_return_data(len);
                self.audio_notify = 0;
            }
            _ => {}
        }
        // One cached-TOC packet per firmware transport interval.
        if self.toc_counter >= 0 && self.command_active == 0 && self.frame_sync {
            self.frame_sync = false;
            let len = self.return_toc_entry();
            self.start_return_data(len);
        }
    }

    /// The WinUAE `akiko_internal` equivalent, run after register
    /// accesses and ticks: pump RX data out, TX commands in, and run a
    /// completed command.
    fn run_internal(&mut self, mem: &mut (impl DmaSpace + ?Sized)) {
        self.return_data(mem);
        self.run_command_dma(mem);
        self.parse_commands();
        // Command execution is paced by emulated time (`command_active`
        // counts down in tick()), never by register accesses: the drive's
        // microcontroller answers after CMD_EXEC_DELAY_CCK, while the
        // guest is off arming its completion interrupt.
    }

    // ----- command path ----------------------------------------------------

    /// The drive's microcontroller is ready to parse its next command:
    /// it has been probed, holds no command in its turnaround, and has
    /// no reply undelivered.
    fn can_send_command(&self) -> bool {
        self.cd_initialized != 0 && self.command_active == 0 && self.receive_length == 0
    }

    /// TX DMA: fetch command bytes from the TX ring, wherever in the
    /// 24-bit space the guest placed it, into the drive's receive buffer.
    ///
    /// The engine is not gated on the drive's command state: whether the
    /// drive is mid-turnaround, has a reply pending, or is streaming a
    /// lead-in dump, Akiko keeps shifting queued bytes across the link
    /// and the guest's comparator index keeps advancing. Only the
    /// (generous) receive buffer bound stalls it.
    fn run_command_dma(&mut self, mem: &mut (impl DmaSpace + ?Sized)) {
        if self.flags & CDFLAG_TXD == 0 {
            return;
        }
        if self.flags & CDFLAG_ENABLE != 0 {
            return;
        }
        if self.cdcomtxinx == self.cdcomtxcmp {
            return;
        }
        if self.tx_dma_delay_cck > 0 {
            return;
        }
        if self.tx_fifo.len() >= TX_FIFO_BYTES {
            return;
        }
        // The TX address folds to the 256-byte command page, like RX: the
        // comparator index registers are eight-bit, and Kickstart's command
        // producer wraps a packet's bytes at index $FF (register trace: an
        // idle-screen LED packet at txinx $FE..$00 lands its checksum at
        // page offset 0, and carrying the address into the following page
        // reads garbage there and fails the packet's checksum).
        let byte = mem.dma_get(self.cdtx_address + u32::from(self.tx_dma_offset & 0xFF));
        self.tx_fifo.push_back(byte);
        self.tx_dma_offset = self.tx_dma_offset.wrapping_add(1);
        self.cdcomtxinx = self.cdcomtxinx.wrapping_add(1);
        if self.cdcomtxinx == self.cdcomtxcmp {
            self.intreq |= CDINT_TXDMADONE;
        }
    }

    /// Drive side of the command link: parse delivered bytes into the
    /// command buffer, one command at a time. Parsing pauses at a
    /// complete command (its turnaround then runs in `tick`), while a
    /// reply is undelivered, and for the whole of a lead-in dump.
    ///
    /// The dump hold is calibrated against a real CD32: the drive's
    /// microcontroller finishes streaming the TOC (spin-up included)
    /// before it acts on the next queued command. Acting on it mid-dump
    /// delivers its reply early, and the KS driver's dump wait -- which
    /// waits on reply-or-TOC-complete -- aborts and reports "no disc" to
    /// the CDUI's one-shot CD_CHANGESTATE, starting the boot-screen show
    /// a real cold boot with a disc inserted never shows. The bytes
    /// behind the held command keep arriving from the TX DMA, so the
    /// queued commands come out intact and in order once the dump ends.
    fn parse_commands(&mut self) {
        while self.can_send_command() && self.toc_counter < 0 {
            let Some(byte) = self.tx_fifo.pop_front() else {
                break;
            };
            self.add_command_byte(byte);
        }
    }

    fn add_command_byte(&mut self, byte: u8) {
        if self.command_length < self.command_buffer.len() {
            self.command_buffer[self.command_length] = byte;
        }
        self.command_length += 1;
        self.command = self.command_buffer[0];
        let cmd_len = COMMAND_LENGTHS[usize::from(self.command & 0x0F)];

        self.checksum_error = false;
        self.unknown_command = false;

        if cmd_len < 0 {
            self.unknown_command = true;
            self.command_active = CMD_EXEC_DELAY_CCK;
            return;
        }
        let cmd_len = cmd_len as usize;
        if cmd_len + 1 > self.command_length {
            return;
        }
        let mut checksum: u8 = 0;
        for i in 0..=cmd_len {
            checksum = checksum.wrapping_add(self.command_buffer[i]);
        }
        if checksum != 0xFF {
            self.checksum_error = true;
        }
        self.command_active = CMD_EXEC_DELAY_CCK;
        self.command_length = cmd_len;
    }

    fn execute_command(&mut self) {
        log::trace!(
            "akiko: exec cmd {:02x?} csum_err={} unknown={}",
            &self.command_buffer[..self.command_length.min(13)],
            self.checksum_error,
            self.unknown_command
        );
        self.command_length = 0;
        // The next packet starts at base + the visible index, exactly where
        // the ROM's producer copied it (the producer's index arithmetic is
        // eight-bit, so this equals the masked DMA offset).
        self.tx_dma_offset = u16::from(self.cdcomtxinx);
        self.result_buffer = [0; 32];

        if self.checksum_error || self.unknown_command {
            self.result_buffer[0] = (self.command & 0xF0) | 5;
            self.result_buffer[1] = if self.checksum_error {
                CH_ERR_CHECKSUM | self.door
            } else {
                CH_ERR_BADCOMMAND | self.door
            };
            self.start_return_data(2);
            return;
        }

        let len = match self.command & 0x0F {
            0 => {
                self.result_buffer[0] = self.command;
                1
            }
            1 => self.command_stop(),
            2 => self.command_pause(),
            3 => self.command_unpause(),
            4 => self.command_multi(),
            5 => self.command_led(),
            6 => self.command_subq(),
            7 => self.command_status(),
            _ => 0,
        };
        if len == 0 {
            self.intreq |= CDINT_DRIVEXMIT;
            return;
        }
        self.start_return_data(len);
    }

    fn check_no_disk(&mut self) -> bool {
        if self.disc.is_none() {
            self.result_buffer[1] = CH_ERR_NODISK | self.door;
            return true;
        }
        false
    }

    fn command_stop(&mut self) -> usize {
        self.audio_notify = 0;
        self.result_buffer[0] = self.command;
        if self.check_no_disk() {
            return 2;
        }
        self.result_buffer[1] = 0;
        self.stop_audio();
        self.data_offset = -1;
        self.data_end = -1;
        2
    }

    fn command_pause(&mut self) -> usize {
        self.audio_notify = 0;
        self.toc_counter = -1;
        self.result_buffer[0] = self.command;
        if self.check_no_disk() {
            return 2;
        }
        self.result_buffer[1] = (if self.playing { CDS_PLAYING } else { 0 }) | self.door;
        if !self.paused {
            self.paused = true;
        }
        2
    }

    fn command_unpause(&mut self) -> usize {
        self.result_buffer[0] = self.command;
        if self.check_no_disk() {
            return 2;
        }
        self.result_buffer[1] = (if self.playing { CDS_PLAYING } else { 0 }) | self.door;
        self.paused = false;
        2
    }

    /// Seek / play audio / read data sectors.
    fn command_multi(&mut self) -> usize {
        let seekpos = bcd_msf_to_lsn(&self.command_buffer[1..4]);
        let endpos = bcd_msf_to_lsn(&self.command_buffer[4..7]);

        if self.playing {
            self.stop_audio();
        }
        self.paused = false;
        self.speed = if self.command_buffer[8] & 0x40 != 0 {
            2
        } else {
            1
        };
        self.result_buffer[0] = self.command;
        self.result_buffer[1] = 0;
        if self.disc.is_none() {
            self.result_buffer[1] = 1; // no disk
            return 2;
        }

        if self.command_buffer[7] & 0x80 != 0 {
            // Data read from seekpos to endpos.
            self.data_offset = seekpos;
            self.data_end = endpos;
            let distance = (self.current_sector - seekpos).unsigned_abs();
            // Real-CD32 locate curve, measured with tools/cd32-probe on
            // the Chinon O-658 mechanism (1-sector reads at fixed
            // distances, min of 3, both burned discs): 254 ms at +500,
            // ~300-430 ms at +1000, 579 ms at +10000, 1.02 s at
            // +100000, 1.37 s at +200000. Piecewise-linear through
            // those anchors in 1x sector frames (13.3 ms), scaled by
            // the configured speed because the delay counter decrements
            // once per output sector slot while the mechanism's time is
            // physical. Only a seamless continuation of the running
            // stream (the head is already there) skips the relocate.
            // WinUAE bills near seeks at a single slot, which the real
            // drive cannot do.
            const LOCATE_ANCHORS: [(u64, u32); 5] = [
                (0, 19),
                (1_000, 26),
                (10_000, 44),
                (100_000, 77),
                (200_000, 102),
            ];
            self.seek_delay = if distance <= 2 {
                1
            } else {
                let mut frames = LOCATE_ANCHORS[LOCATE_ANCHORS.len() - 1].1;
                for pair in LOCATE_ANCHORS.windows(2) {
                    let ((d0, f0), (d1, f1)) = (pair[0], pair[1]);
                    if distance <= d1 {
                        let span = (d1 - d0).max(1);
                        frames = f0 + ((f1 - f0) as u64 * (distance - d0) / span) as u32;
                        break;
                    }
                }
                frames * self.speed.max(1)
            };
            log::debug!("akiko: READ DATA {seekpos}..{endpos} speed {}x", self.speed);
            self.result_buffer[1] |= 0x02;
        } else if seekpos < 0 {
            // Play command with a lead-in address: a TOC dump.
            self.toc_counter = 0;
        } else if !self
            .disc
            .as_ref()
            .is_some_and(|d| d.is_audio_sector(seekpos as u32))
        {
            // A play aimed at a data track: the real firmware refuses
            // (cd32-probe on a burned data-only disc reads io_Error 36
            // for every play row) and volunteers the play-failed
            // notification instead of starting the stream.
            self.toc_counter = -1;
            self.result_buffer[1] = 0x42;
            self.audio_notify = -3;
            log::debug!("akiko: PLAY {seekpos}..{endpos} refused (data track)");
        } else {
            // Audio play: stream CD-DA into the host mixer from here.
            self.toc_counter = -1;
            self.result_buffer[1] = 0x42; // play starting
            self.playing = true;
            self.play_position = seekpos;
            self.play_end = endpos;
            self.current_sector = seekpos;
            self.audio_notify = 10; // play-start packet shortly
            log::debug!("akiko: PLAY {seekpos}..{endpos}");
        }
        2
    }

    fn command_led(&mut self) -> usize {
        let v = self.command_buffer[1];
        if v & 0x80 != 0 {
            self.result_buffer[0] = self.command;
            self.result_buffer[1] = v & 1;
            return 2;
        }
        0
    }

    fn command_subq(&mut self) -> usize {
        self.result_buffer[0] = self.command;
        self.result_buffer[1] = 0;
        self.result_buffer[2..13].fill(0);
        // The drive reports its Q-channel position only while an audio
        // play is in progress or paused (WinUAE's cd_qcode gates the same
        // way); the all-zero packet otherwise reads as "no valid position
        // yet". Layout after the two header bytes: zero, control/ADR,
        // track, index, track-relative MSF, zero, absolute MSF.
        if self.playing {
            if let Some(disc) = self.disc.as_ref() {
                let q = q_channel_position(disc.tracks(), self.play_position.max(0) as u32);
                let d = &mut self.result_buffer[2..13];
                d[1] = q[0];
                d[2] = q[1];
                d[3] = q[2];
                d[4..7].copy_from_slice(&q[3..6]);
                d[8..11].copy_from_slice(&q[7..10]);
            }
        }
        15
    }

    fn command_status(&mut self) -> usize {
        self.result_buffer[0] = self.command;
        self.result_buffer[1] = self.door;
        self.result_buffer[2..2 + FIRMWARE_VERSION.len()].copy_from_slice(FIRMWARE_VERSION);
        self.cd_initialized = 2;
        20
    }

    fn command_media_status(&mut self) -> usize {
        self.result_buffer[0] = 0x0A;
        // Disc present is 0x83, cross-checked against both real-hardware
        // ROM drivers: the CD32 Kickstart media handler masks this byte
        // with 0x03 and treats nonzero as present (extended ROM $E59712,
        // "andib #3"), while AROS cd.device compares the whole byte
        // against 0x83 - only 0x83 satisfies both. WinUAE returns a bare
        // 0x01 here, which only works because of Kickstart's mask.
        self.result_buffer[1] = if self.disc.is_some() { 0x83 } else { 0x00 };
        2
    }

    fn playend_notify(&mut self, status: i32) -> usize {
        self.result_buffer[0] = 4;
        self.result_buffer[1] = match status {
            s if s < 0 => CDS_ERROR, // error
            0 => CDS_PLAYING | 2,    // play started
            _ => 0,                  // play ended
        } | self.door;
        2
    }

    fn return_toc_entry(&mut self) -> usize {
        self.result_buffer[0] = 6;
        if self.toc.is_empty() {
            self.result_buffer[1] = CDS_ERROR | self.door;
            self.toc_counter = -1;
            return 15;
        }
        self.result_buffer[1] = 0x0A; // matches real CD32 captures

        // The firmware's TOC dump streams the track entries first and the
        // A0/A1/A2 session entries last. Drivers may treat the lead-out
        // entry (A2) as "TOC complete" and stop parsing, so every track
        // entry must precede it; session-entries-first starves such a
        // parser of the track list (KS cd.device is order-insensitive,
        // AROS cd.device latches at A2).
        let pos = (self.toc_counter as u32 / TOC_REPEAT) as usize;
        let tracks = self.toc.len() - 3;
        let index = if pos < tracks { pos + 3 } else { pos - tracks };
        let entry = toc_entry_bytes(&self.toc[index]);
        self.result_buffer[2..15].copy_from_slice(&entry);
        // Fake the head's running position, as the real firmware does.
        let counter = self.toc_counter as u32;
        self.result_buffer[6] = to_bcd(99);
        self.result_buffer[7] = to_bcd((24 + counter / 75) as u8);
        self.result_buffer[8] = to_bcd((counter % 75) as u8);
        self.toc_counter += 1;
        if (self.toc_counter as u32 / TOC_REPEAT) as usize >= self.toc.len() {
            self.toc_counter = -1;
        }
        15
    }

    fn stop_audio(&mut self) {
        self.playing = false;
        self.paused = false;
        self.play_position = 0;
        self.play_end = 0;
        self.flush_cd_audio = true;
    }

    // ----- response path ---------------------------------------------------

    fn start_return_data(&mut self, len: usize) -> bool {
        if self.receive_length > 0 || len == 0 {
            return false;
        }
        self.receive_length = len;
        let mut checksum: u8 = 0xFF;
        for i in 0..len {
            checksum = checksum.wrapping_sub(self.result_buffer[i]);
        }
        self.result_buffer[self.receive_length] = checksum;
        self.receive_length += 1;
        self.receive_offset = 0;
        self.intreq |= CDINT_DRIVERECV;
        log::trace!(
            "akiko: return {:02x?} (rxinx={:02X} rxcmp={:02X} intreq={:08X})",
            &self.result_buffer[..self.receive_length],
            self.cdcomrxinx,
            self.cdcomrxcmp,
            self.intreq
        );
        true
    }

    /// RX DMA: write pending response bytes into the RX ring.
    fn return_data(&mut self, mem: &mut (impl DmaSpace + ?Sized)) {
        if self.receive_length == 0 {
            return;
        }
        if self.flags & CDFLAG_RXD == 0 {
            return;
        }
        if self.cdcomrxinx == self.cdcomrxcmp {
            return;
        }
        if self.rx_dma_delay_cck > 0 {
            return;
        }
        while self.receive_offset < self.receive_length {
            self.last_rx = self.result_buffer[self.receive_offset];
            mem.dma_put(
                self.cdrx_address + u32::from(self.rx_dma_offset & 0xFF),
                self.last_rx,
            );
            self.rx_dma_offset = self.rx_dma_offset.wrapping_add(1) & 0xFF;
            self.cdcomrxinx = self.cdcomrxinx.wrapping_add(1);
            self.receive_offset += 1;
            if self.cdcomrxinx == self.cdcomrxcmp {
                self.intreq |= CDINT_RXDMADONE;
                break;
            }
        }
        if self.receive_offset == self.receive_length {
            self.receive_length = 0;
            self.receive_offset = 0;
            // The next packet begins at base + the visible index.
            self.rx_dma_offset = u16::from(self.cdcomrxinx);
            self.intreq &= !CDINT_DRIVERECV;
            self.intreq |= CDINT_DRIVEXMIT;
        }
    }

    // ----- sector DMA ------------------------------------------------------

    fn run_sector_read(&mut self, mem: &mut (impl DmaSpace + ?Sized)) {
        if self.flags & CDFLAG_ENABLE == 0 {
            return;
        }
        if self.pbx == 0 || self.flags & CDFLAG_PBX == 0 {
            return;
        }
        if self.data_offset < 0 {
            return;
        }
        let Some(disc) = self.disc.as_mut() else {
            return;
        };
        // Akiko has no per-sector destination register: the driver arms a
        // 16-bit mask (PBX) of available 4 KB buffers and the engine
        // consumes one buffer per incoming sector, clearing its bit. Fill
        // order is the highest armed buffer first (priority-encode from
        // bit 15). This ordering is empirical, not from a datasheet:
        // WinUAE observes it on real CD32 silicon (its own comment cites
        // the same regression title as this file's tests), but the only
        // independent reverse-engineering (MAME) instead maps the buffer
        // to (lba - lba_start) & 0x0F, i.e. sector-ordinal order gated by
        // the arm mask -- not highest-bit-first. No HDL/datasheet ground
        // truth exists to settle it (see docs/internals for chip-model
        // cross-checking method). Keep highest-bit-first since WinUAE is
        // the more hardware-validated CD32 reference, but treat this as a
        // model, not confirmed hardware.
        // TODO: verify buffer-fill order against real Akiko silicon.
        let slot = (15 - self.pbx.leading_zeros().saturating_sub(16)) & 15;
        let slot = if self.pbx & (1 << slot) != 0 {
            slot
        } else {
            return;
        };
        let sector = self.data_offset + i64::from(self.sector_counter);
        self.current_sector = sector;
        if self.data_end >= 0 && sector > self.data_end {
            self.data_offset = -1;
            self.data_end = -1;
            return;
        }

        // The READ DATA end MSF is exclusive, but the CD32 driver keeps a
        // continuous hardware read open across filesystem requests. When a
        // request seeks backwards after that stream reaches lead-out, the
        // driver needs one final position-bearing PBX frame to notice that
        // the buffered stream is nowhere near the requested sector; it then
        // sends STOP and starts a new read at the requested LSN. Re-present
        // the final valid raw sector as that boundary probe. Its payload is
        // never returned to the caller because its on-disc MSF mismatches the
        // requested LSN, while retaining the real frame also preserves valid
        // Mode 1/2 EDC and ECC bytes for the ROM's sector validator.
        let at_end = self.data_end >= 0 && sector == self.data_end;
        let read_sector = if at_end { sector - 1 } else { sector };
        if read_sector < 0 || !sector_in_data_track(&self.toc, read_sector as u32) {
            if at_end {
                self.data_offset = -1;
                self.data_end = -1;
                self.intreq |= CDINT_PBX;
            }
            return;
        }
        // Akiko's PBX path carries the complete raw Mode 1 frame. Preserve
        // a raw BIN/CUE sector verbatim, or let the shared image backend
        // promote a cooked ISO sector with its EDC and P/Q parity. CDXL
        // players use this raw-sector path for sustained video streaming.
        let mut raw = [0u8; RAW_SECTOR_BYTES];
        if disc.read_raw_sector(read_sector as u32, &mut raw).is_err() {
            return;
        }

        // The first four sync bytes carry Akiko's transfer tag instead.
        raw[0] = 0;
        raw[1] = 0;
        raw[2] = 0;
        raw[3] = (self.sector_counter & 31) as u8;

        log::trace!(
            "akiko: sector {sector} -> slot {slot} at {:#010x} (tag {}, pbx {:04x})",
            self.addressdata + slot * 4096,
            self.sector_counter & 31,
            self.pbx
        );
        let base = self.addressdata + slot * 4096;
        mem.dma_put_bytes(base, &raw);
        // Clear the slot's subcode area.
        mem.dma_put_bytes(base + 0xC00, &[0u8; 73 * 2]);
        self.pbx &= !(1 << slot);
        self.intreq |= CDINT_PBX;

        // Sector-synchronous subcode delivery alongside the payload.
        self.deliver_subcode(mem, read_sector as u32);

        self.sector_counter += 1;
        if at_end {
            log::debug!(
                "akiko: READ DATA reached exclusive end {sector}; delivered final-sector boundary probe"
            );
            self.data_offset = -1;
            self.data_end = -1;
        }
    }

    /// Hand the subcode frame of `sector` to the host. Akiko DMAs each
    /// 96-byte frame into the misc page's subcode area (+$100),
    /// alternating between its two 128-byte halves, appends the $FFFF
    /// $0000 end marker, advances the offset register by 100, and raises
    /// the subcode interrupt -- WinUAE's akiko.cpp delivery sequence.
    /// Only the Q channel carries anything: image formats store no
    /// subchannel data, so it is regenerated from the TOC.
    fn deliver_subcode(&mut self, mem: &mut (impl DmaSpace + ?Sized), sector: u32) {
        if self.flags & CDFLAG_SUBCODE == 0 {
            return;
        }
        let frame = match self.disc.as_ref() {
            Some(disc) => subcode_frame(disc.tracks(), sector),
            None => [0u8; SUBCODE_FRAME_BYTES],
        };
        self.subcodeoffset = if self.subcodeoffset >= 128 { 0 } else { 128 };
        let base = self.subcode_address + u32::from(self.subcodeoffset);
        mem.dma_put_bytes(base, &frame);
        mem.dma_put_bytes(base + SUBCODE_FRAME_BYTES as u32, &[0xFF, 0xFF, 0x00, 0x00]);
        self.subcodeoffset = self.subcodeoffset.wrapping_add(100);
        self.intreq |= CDINT_SUBCODE;
    }

    // ----- C2P -------------------------------------------------------------

    fn c2p_read_byte(&mut self, offset: u32) -> u8 {
        let read_offset = match self.c2p.read_offset {
            Some(off) => off,
            None => {
                self.c2p_convert();
                self.c2p.write_offset = 0;
                self.c2p.read_offset = Some(0);
                0
            }
        };
        let long = self.c2p.result[read_offset];
        (long >> (8 * (3 - (offset - 0x38)))) as u8
    }

    fn c2p_write_byte(&mut self, offset: u32, value: u8) {
        let byte = (offset - 0x38) as usize;
        if byte == 3 {
            self.c2p.buffer[self.c2p.write_offset] = 0;
        }
        self.c2p.buffer[self.c2p.write_offset] |= u32::from(value) << (8 * (3 - byte));
        if byte == 0 {
            self.c2p.write_offset = (self.c2p.write_offset + 1) & 7;
        }
        self.c2p.read_offset = None;
    }

    fn c2p_read_step(&mut self, offset: u32) {
        if !(0x38..0x3C).contains(&(offset & 0x3F)) {
            return;
        }
        if let Some(read_offset) = self.c2p.read_offset.as_mut() {
            *read_offset = (*read_offset + 1) & 7;
        }
    }

    /// The C2P transpose: 8 longwords in (32 chunky 8-bit pixels, low
    /// byte of the most recent longword first), 8 planar longwords out.
    /// Bit mapping per WinUAE's reference implementation.
    fn c2p_convert(&mut self) {
        self.c2p.result = [0; 8];
        for i in 0..(8 * 32) {
            if self.c2p.buffer[7 - (i >> 5)] & (1u32 << (i & 31)) != 0 {
                self.c2p.result[i & 7] |= 1 << (i >> 3);
            }
        }
    }
}

/// The 24-bit address space Akiko's DMA engines read and write. The
/// address registers mask to $00FFF000, i.e. Akiko drives a full 24-bit
/// address bus, so the command/response rings and sector buffers can live
/// in any RAM in the low 16 MB - Zorro II fast RAM included, which is
/// where AROS puts its MEMF_24BITDMA allocations when fast RAM exists.
/// WinUAE routes these accesses through its general bus accessors for the
/// same reason. Reads outside RAM float to zero and writes are dropped,
/// both logged. Per-byte resolution costs a few bank range checks at most
/// 2352 bytes per CD frame, which is noise.
pub trait DmaSpace {
    fn dma_byte_mut(&mut self, addr: u32) -> Option<&mut u8>;

    fn dma_get(&mut self, addr: u32) -> u8 {
        match self.dma_byte_mut(addr & 0x00FF_FFFF) {
            Some(b) => *b,
            None => {
                log::warn!("akiko: DMA read outside RAM at {addr:#010X}");
                0
            }
        }
    }

    fn dma_put(&mut self, addr: u32, value: u8) {
        match self.dma_byte_mut(addr & 0x00FF_FFFF) {
            Some(b) => *b = value,
            None => log::warn!("akiko: DMA write outside RAM at {addr:#010X}"),
        }
    }

    fn dma_put_bytes(&mut self, addr: u32, bytes: &[u8]) {
        // Aggregate unmapped bytes into one warning per transfer: a
        // sector is 2352 bytes at CD-frame cadence, and a bad
        // destination must not flood the log.
        let mut unmapped = 0usize;
        let mut first = 0u32;
        for (i, &v) in bytes.iter().enumerate() {
            let a = addr.wrapping_add(i as u32) & 0x00FF_FFFF;
            match self.dma_byte_mut(a) {
                Some(b) => *b = v,
                None => {
                    if unmapped == 0 {
                        first = a;
                    }
                    unmapped += 1;
                }
            }
        }
        if unmapped > 0 {
            log::warn!(
                "akiko: DMA write outside RAM: {unmapped} of {} bytes from {first:#010X}",
                bytes.len()
            );
        }
    }
}

impl DmaSpace for crate::memory::Memory {
    fn dma_byte_mut(&mut self, addr: u32) -> Option<&mut u8> {
        let a = addr as usize;
        if a < self.chip_ram.len() {
            return Some(&mut self.chip_ram[a]);
        }
        let slow = crate::memory::SLOW_RAM_BASE as usize;
        if a >= slow && a < slow + self.slow_ram.len() {
            return Some(&mut self.slow_ram[a - slow]);
        }
        let mb = self.mb_ram_base() as usize;
        if a >= mb && a < mb + self.mb_ram.len() {
            return Some(&mut self.mb_ram[a - mb]);
        }
        let accel = crate::memory::ACCEL_RAM_BASE as usize;
        if a >= accel && a < accel + self.accel_ram.len() {
            return Some(&mut self.accel_ram[a - accel]);
        }
        if let Some((board, off)) = self.zorro.region_at(addr, 1) {
            return Some(&mut self.zorro.board_ram_mut(board)[off]);
        }
        None
    }
}

/// Identity-mapped space for unit tests: a bare RAM image from address 0.
impl DmaSpace for [u8] {
    fn dma_byte_mut(&mut self, addr: u32) -> Option<&mut u8> {
        let a = addr as usize;
        if a < self.len() {
            Some(&mut self[a])
        } else {
            None
        }
    }
}

impl DmaSpace for Vec<u8> {
    fn dma_byte_mut(&mut self, addr: u32) -> Option<&mut u8> {
        self.as_mut_slice().dma_byte_mut(addr)
    }
}

fn build_toc(disc: &CdImage) -> Vec<TocEntry> {
    let tracks = disc.tracks();
    let mut toc = Vec::with_capacity(tracks.len() + 3);
    let first = tracks.first().map(|t| t.number).unwrap_or(1);
    let last = tracks.last().map(|t| t.number).unwrap_or(1);
    // Session entries: first track, last track, lead-out address.
    toc.push(TocEntry {
        point: 0xA0,
        control: 0,
        address: u32::from(first),
    });
    toc.push(TocEntry {
        point: 0xA1,
        control: 0,
        address: u32::from(last),
    });
    toc.push(TocEntry {
        point: 0xA2,
        control: 0,
        address: disc.total_sectors(),
    });
    for track in tracks {
        toc.push(TocEntry {
            point: track.number,
            control: if track.kind.is_data() { 0x04 } else { 0x00 },
            address: track.start_sector,
        });
    }
    toc
}

/// One 13-byte TOC packet body, as the Chinon firmware formats it.
fn toc_entry_bytes(entry: &TocEntry) -> [u8; 13] {
    let mut d = [0u8; 13];
    d[1] = 0x01 | (entry.control << 4); // ADR 1 | control
    d[3] = if entry.point < 100 {
        to_bcd(entry.point)
    } else {
        entry.point
    };
    if entry.point == 0xA0 || entry.point == 0xA1 {
        d[8] = to_bcd(entry.address as u8);
    } else {
        let msf = entry.address + LEADIN_SECTORS;
        d[8] = to_bcd((msf / (60 * 75)) as u8);
        d[9] = to_bcd(((msf / 75) % 60) as u8);
        d[10] = to_bcd((msf % 75) as u8);
    }
    d
}

fn sector_in_data_track(toc: &[TocEntry], sector: u32) -> bool {
    // Track entries follow the three session entries; a data sector must
    // fall inside a control-0x04 track, bounded by the next track start
    // (or the lead-out for the last track).
    for i in 3..toc.len() {
        let entry = &toc[i];
        if entry.control & 0x04 == 0 {
            continue;
        }
        let end = toc
            .get(i + 1)
            .map(|next| next.address)
            .unwrap_or_else(|| toc[2].address);
        if sector >= entry.address && sector < end {
            return true;
        }
    }
    false
}

/// The Q-channel CRC: CRC-16-CCITT (x^16 + x^12 + x^5 + 1, zero seed)
/// over the ten payload bytes, transmitted inverted (Red Book).
fn q_channel_crc(payload: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in payload {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    !crc
}

fn write_bcd_msf(out: &mut [u8], frames: u32) {
    out[0] = to_bcd((frames / (60 * 75)) as u8);
    out[1] = to_bcd(((frames / 75) % 60) as u8);
    out[2] = to_bcd((frames % 75) as u8);
}

/// The twelve Q-channel bytes of the subcode frame at disc sector
/// `sector`: an ADR 1 position packet (control nibble, track, index,
/// track-relative MSF, zero, absolute MSF, CRC), all BCD. A sector in
/// the gap before a track's INDEX 01 belongs to that track as index 0
/// with the relative time counting down, as the pickup sees it.
fn q_channel_position(tracks: &[CdTrack], sector: u32) -> [u8; 12] {
    let mut q = [0u8; 12];
    let (control, number, index, relative) =
        match tracks.iter().rposition(|t| sector >= t.start_sector) {
            Some(i) => {
                let track = &tracks[i];
                let next = tracks.get(i + 1);
                match next {
                    Some(next) if sector >= track.start_sector + track.sector_count => {
                        (next.kind, next.number, 0, next.start_sector - sector)
                    }
                    _ => (track.kind, track.number, 1, sector - track.start_sector),
                }
            }
            // Lead-in side of the first track: its pregap, counting down.
            None => match tracks.first() {
                Some(first) => (first.kind, first.number, 0, first.start_sector - sector),
                None => return q,
            },
        };
    q[0] = if control.is_data() { 0x41 } else { 0x01 };
    q[1] = to_bcd(number);
    q[2] = to_bcd(index);
    write_bcd_msf(&mut q[3..6], relative);
    write_bcd_msf(&mut q[7..10], sector + LEADIN_SECTORS);
    let crc = q_channel_crc(&q[..10]);
    q[10] = (crc >> 8) as u8;
    q[11] = crc as u8;
    q
}

/// One subcode frame as the drive delivers it: bit-interleaved, byte i
/// carrying bit i of every channel with P in bit 7 and Q in bit 6. P
/// and R-W stay blank; Q is the position packet.
fn subcode_frame(tracks: &[CdTrack], sector: u32) -> [u8; SUBCODE_FRAME_BYTES] {
    let q = q_channel_position(tracks, sector);
    let mut frame = [0u8; SUBCODE_FRAME_BYTES];
    for (i, byte) in frame.iter_mut().enumerate() {
        if q[i / 8] & (0x80 >> (i % 8)) != 0 {
            *byte |= 0x40;
        }
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdrom::DATA_SECTOR_BYTES;
    use std::io::Write;
    use std::path::PathBuf;

    fn no_chip() -> Vec<u8> {
        vec![0u8; 1024]
    }

    #[test]
    fn activity_led_tracks_audio_toc_and_data_work() {
        let mut akiko = Akiko::new();
        akiko.insert_disc(test_disc());
        assert!(!akiko.activity_led_on());

        akiko.playing = true;
        assert!(akiko.activity_led_on());
        akiko.paused = true;
        assert!(!akiko.activity_led_on());
        akiko.playing = false;
        akiko.paused = false;

        akiko.toc_counter = 0;
        assert!(akiko.activity_led_on());
        akiko.toc_counter = -1;

        // A data read only lights the LED while the host keeps feeding
        // PBX buffer slots.
        akiko.data_offset = 100;
        assert!(!akiko.activity_led_on());
        akiko.pbx = 1;
        akiko.flags = CDFLAG_ENABLE | CDFLAG_PBX;
        assert!(akiko.activity_led_on());
        akiko.pbx = 0;
        assert!(!akiko.activity_led_on());
    }

    #[test]
    fn current_track_follows_data_and_audio_positions() {
        let mut akiko = Akiko::new();
        akiko.insert_disc(test_two_track_disc());
        assert_eq!(akiko.current_track(), Some(1));

        akiko.current_sector = 4;
        assert_eq!(akiko.current_track(), Some(2));

        akiko.current_sector = 0;
        akiko.playing = true;
        akiko.play_position = 4;
        akiko.play_end = 6;
        let mut ring = CdAudioRing::default();
        let mut chip = no_chip();
        akiko.stream_audio_sector(&mut chip, &mut ring);
        assert_eq!(akiko.current_track(), Some(2));

        akiko.eject_disc();
        assert_eq!(akiko.current_track(), None);
    }

    #[test]
    fn guest_reset_keeps_the_lead_in_but_a_power_cycle_respins() {
        let mut akiko = Akiko::new();
        akiko.insert_disc(test_disc());
        assert!(akiko.toc_spin_up_cck > 0, "cold mount pays spin-up");
        akiko.toc_spin_up_cck = 0;

        // A guest reset keeps the disc spinning and the lead-in cached.
        akiko.reset();
        assert!(akiko.disc.is_some());
        assert_eq!(akiko.toc_spin_up_cck, 0);

        // A power cycle stops the mechanism: the next lead-in dump pays
        // the full cold spin-up again.
        akiko.rearm_cold_spin_up();
        assert!(akiko.toc_spin_up_cck > 0);

        // Without a disc there is nothing to spin up.
        akiko.eject_disc();
        akiko.toc_spin_up_cck = 0;
        akiko.rearm_cold_spin_up();
        assert_eq!(akiko.toc_spin_up_cck, 0);
    }

    #[test]
    fn runtime_disc_change_volunteers_media_status() {
        let mut akiko = Akiko::new();
        // Boot-time mount: the cold-start status push covers it, no
        // extra notification.
        akiko.insert_disc(test_disc());
        assert!(!akiko.media_notify);
        assert!(akiko.has_disc());

        // Runtime eject once the host is up: the drive volunteers a
        // media-status packet showing no disc.
        akiko.cd_initialized = 2;
        akiko.eject_disc();
        assert!(!akiko.has_disc());
        assert!(akiko.media_notify);
        akiko.handler();
        assert_eq!(akiko.result_buffer[0], 0x0A);
        assert_eq!(akiko.result_buffer[1], 0);
        assert!(!akiko.media_notify);
        assert!(akiko.receive_length > 0);

        // Runtime insert: another packet, now showing media present.
        // 0x83 satisfies both ROM drivers (KS masks with 3, AROS matches
        // the whole byte).
        akiko.receive_length = 0;
        akiko.insert_disc(test_disc());
        assert!(akiko.media_notify);
        akiko.handler();
        assert_eq!(akiko.result_buffer[0], 0x0A);
        assert_eq!(akiko.result_buffer[1], 0x83);
    }

    fn test_disc() -> CdImage {
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir();
        let cue: PathBuf = dir.join(format!("copperline-akiko-{pid}-{unique}.cue"));
        let bin: PathBuf = dir.join(format!("copperline-akiko-{pid}-{unique}.bin"));
        let mut bytes = Vec::new();
        for s in 0u8..8 {
            bytes.extend(std::iter::repeat_n(s, DATA_SECTOR_BYTES));
        }
        let mut f = std::fs::File::create(&bin).unwrap();
        f.write_all(&bytes).unwrap();
        std::fs::write(
            &cue,
            format!(
                "FILE \"{}\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n",
                bin.file_name().unwrap().to_string_lossy()
            ),
        )
        .unwrap();
        let image = CdImage::load(&cue).unwrap();
        let _ = std::fs::remove_file(&cue);
        image
    }

    fn test_two_track_disc() -> CdImage {
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir();
        let cue: PathBuf = dir.join(format!("copperline-akiko-tracks-{pid}-{unique}.cue"));
        let bin: PathBuf = dir.join(format!("copperline-akiko-tracks-{pid}-{unique}.bin"));
        let mut bytes = Vec::new();
        for s in 0u8..8 {
            bytes.extend(std::iter::repeat_n(s, DATA_SECTOR_BYTES));
        }
        let mut f = std::fs::File::create(&bin).unwrap();
        f.write_all(&bytes).unwrap();
        std::fs::write(
            &cue,
            format!(
                concat!(
                    "FILE \"{}\" BINARY\n",
                    "  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n",
                    "  TRACK 02 MODE1/2048\n    INDEX 01 00:00:04\n",
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

    #[test]
    fn kickstart_probe_reads_cafe_at_b80002() {
        let mut chip = no_chip();
        let mut akiko = Akiko::new();
        assert_eq!(akiko.read(AKIKO_BASE + 2, 2, &mut chip), 0xCAFE);
        assert_eq!(akiko.read(AKIKO_BASE, 4, &mut chip), 0xC0CA_CAFE);
    }

    #[test]
    fn c2p_converts_last_longword_low_byte_into_plane_bit_zero() {
        let mut chip = no_chip();
        let mut akiko = Akiko::new();
        for _ in 0..7 {
            akiko.write(AKIKO_BASE + 0x38, 4, 0, &mut chip);
        }
        akiko.write(AKIKO_BASE + 0x38, 4, 0x0000_00FF, &mut chip);
        for plane in 0..8 {
            let v = akiko.read(AKIKO_BASE + 0x38, 4, &mut chip);
            assert_eq!(v, 1, "plane {plane}");
        }
    }

    #[test]
    fn c2p_distributes_pixel_colour_bits_across_planes() {
        let mut chip = no_chip();
        let mut akiko = Akiko::new();
        for _ in 0..7 {
            akiko.write(AKIKO_BASE + 0x38, 4, 0, &mut chip);
        }
        akiko.write(AKIKO_BASE + 0x38, 4, 0x0000_0005, &mut chip);
        let plane0 = akiko.read(AKIKO_BASE + 0x38, 4, &mut chip);
        let plane1 = akiko.read(AKIKO_BASE + 0x38, 4, &mut chip);
        let plane2 = akiko.read(AKIKO_BASE + 0x38, 4, &mut chip);
        let plane3 = akiko.read(AKIKO_BASE + 0x38, 4, &mut chip);
        assert_eq!((plane0, plane1, plane2, plane3), (1, 0, 1, 0));
    }

    /// Bit-bang I2C master helpers driving the NVRAM lines through the
    /// Akiko registers, as the CD32 ROM does.
    mod i2c {
        use super::*;

        const SCL: u32 = 0x80;
        const SDA: u32 = 0x40;

        fn set(akiko: &mut Akiko, chip: &mut [u8], dir: u32, lines: u32) {
            akiko.write(AKIKO_BASE + 0x32, 1, dir, chip);
            akiko.write(AKIKO_BASE + 0x30, 1, lines, chip);
        }

        pub fn start(akiko: &mut Akiko, chip: &mut [u8]) {
            set(akiko, chip, SCL | SDA, SCL | SDA);
            set(akiko, chip, SCL | SDA, SCL); // SDA falls, SCL high
            set(akiko, chip, SCL | SDA, 0); // SCL low
        }

        pub fn stop(akiko: &mut Akiko, chip: &mut [u8]) {
            set(akiko, chip, SCL | SDA, 0);
            set(akiko, chip, SCL | SDA, SCL); // SCL high, SDA low
            set(akiko, chip, SCL | SDA, SCL | SDA); // SDA rises
        }

        /// Write one byte MSB-first and return the ACK level (false =
        /// ACKed).
        pub fn write_byte(akiko: &mut Akiko, chip: &mut [u8], byte: u8) -> bool {
            for bit in (0..8).rev() {
                let sda = if byte & (1 << bit) != 0 { SDA } else { 0 };
                set(akiko, chip, SCL | SDA, sda);
                set(akiko, chip, SCL | SDA, SCL | sda);
                set(akiko, chip, SCL | SDA, sda);
            }
            // ACK clock with SDA as input.
            set(akiko, chip, SCL, 0);
            set(akiko, chip, SCL, SCL);
            let ack = akiko.read(AKIKO_BASE + 0x30, 1, chip) & SDA != 0;
            set(akiko, chip, SCL, 0);
            ack
        }

        /// Read one byte MSB-first; `ack` = master ACKs (continue).
        pub fn read_byte(akiko: &mut Akiko, chip: &mut [u8], ack: bool) -> u8 {
            let mut byte = 0u8;
            for _ in 0..8 {
                set(akiko, chip, SCL, SCL);
                byte = (byte << 1) | u8::from(akiko.read(AKIKO_BASE + 0x30, 1, chip) & SDA != 0);
                set(akiko, chip, SCL, 0);
            }
            let sda = if ack { 0 } else { SDA };
            set(akiko, chip, SCL | SDA, sda);
            set(akiko, chip, SCL | SDA, SCL | sda);
            set(akiko, chip, SCL | SDA, sda);
            byte
        }
    }

    #[test]
    fn nvram_eeprom_round_trips_a_page_write_and_random_read() {
        let mut chip = no_chip();
        let mut akiko = Akiko::new();

        // Page write: device 1010 block1 W, word address 0x42, two bytes.
        i2c::start(&mut akiko, &mut chip);
        assert!(!i2c::write_byte(&mut akiko, &mut chip, 0xA2), "addr ACK");
        assert!(!i2c::write_byte(&mut akiko, &mut chip, 0x42), "word ACK");
        assert!(!i2c::write_byte(&mut akiko, &mut chip, 0xDE), "data ACK");
        assert!(!i2c::write_byte(&mut akiko, &mut chip, 0xAD), "data ACK");
        i2c::stop(&mut akiko, &mut chip);
        assert_eq!(akiko.nvram.memory[0x142], 0xDE);
        assert_eq!(akiko.nvram.memory[0x143], 0xAD);

        // Random read: set the address with a write header, repeated
        // START, then read two bytes sequentially.
        i2c::start(&mut akiko, &mut chip);
        assert!(!i2c::write_byte(&mut akiko, &mut chip, 0xA2));
        assert!(!i2c::write_byte(&mut akiko, &mut chip, 0x42));
        i2c::start(&mut akiko, &mut chip); // repeated start
        assert!(
            !i2c::write_byte(&mut akiko, &mut chip, 0xA3),
            "read addr ACK"
        );
        assert_eq!(i2c::read_byte(&mut akiko, &mut chip, true), 0xDE);
        assert_eq!(i2c::read_byte(&mut akiko, &mut chip, false), 0xAD);
        i2c::stop(&mut akiko, &mut chip);

        // A non-EEPROM device address is ignored (no ACK).
        i2c::start(&mut akiko, &mut chip);
        assert!(i2c::write_byte(&mut akiko, &mut chip, 0x55), "no ACK");
        i2c::stop(&mut akiko, &mut chip);
    }

    /// A failed backing-file write must leave the EEPROM dirty so a
    /// later STOP retries; clearing it up front would silently drop the
    /// NVRAM contents (save games) on a transient host error.
    #[test]
    fn nvram_retries_the_backing_file_write_after_a_failure() {
        let mut chip = no_chip();
        let mut akiko = Akiko::new();
        let unique = {
            static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        };
        let dir = std::env::temp_dir().join(format!(
            "copperline-akiko-nvram-retry-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("cd32-nvram.bin");
        // A directory where the file goes: the write fails, and unlike a
        // missing parent it stays failing, since the flush makes its own
        // directories now.
        std::fs::create_dir_all(&path).unwrap();
        akiko.set_nvram_path(path.clone());

        i2c::start(&mut akiko, &mut chip);
        assert!(!i2c::write_byte(&mut akiko, &mut chip, 0xA2));
        assert!(!i2c::write_byte(&mut akiko, &mut chip, 0x42));
        assert!(!i2c::write_byte(&mut akiko, &mut chip, 0xDE));
        i2c::stop(&mut akiko, &mut chip); // flush fails: a directory is in the way
        assert!(path.is_dir(), "nothing should have been written over it");

        std::fs::remove_dir(&path).unwrap();
        i2c::start(&mut akiko, &mut chip);
        i2c::stop(&mut akiko, &mut chip); // still dirty: retried and lands
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes[0x142], 0xDE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn audio_play_streams_sectors_into_mixer_ring_and_notifies_end() {
        // Disc: 2 data sectors then 4 audio sectors of a known sample.
        let nanos_unique = {
            static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        };
        let dir = std::env::temp_dir();
        let cue = dir.join(format!(
            "copperline-akiko-audio-{}-{nanos_unique}.cue",
            std::process::id()
        ));
        let bin = dir.join(format!(
            "copperline-akiko-audio-{}-{nanos_unique}.bin",
            std::process::id()
        ));
        let mut bytes = vec![0u8; 2 * DATA_SECTOR_BYTES];
        // Audio frames: left = 0x1000, right = 0x2000 (little endian).
        for _ in 0..4 * (crate::cdrom::RAW_SECTOR_BYTES / 4) {
            bytes.extend_from_slice(&[0x00, 0x10, 0x00, 0x20]);
        }
        std::fs::write(
            &cue,
            format!(
                concat!(
                    "FILE \"{}\" BINARY\n",
                    "  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n",
                    "  TRACK 02 AUDIO\n    INDEX 01 00:00:02\n",
                ),
                bin.file_name().unwrap().to_string_lossy()
            ),
        )
        .unwrap();
        std::fs::write(&bin, &bytes).unwrap();
        let image = CdImage::load(&cue).unwrap();
        let _ = std::fs::remove_file(&cue);
        let _ = std::fs::remove_file(&bin);

        let mut chip = vec![0u8; 64 * 1024];
        let mut ring = CdAudioRing::default();
        let mut akiko = Akiko::new();
        akiko.insert_disc(image);
        akiko.tick(2048, &mut chip, &mut ring);
        akiko.cd_initialized = 2;
        akiko.receive_length = 0;

        // PLAY track 2: MSF 00:02:02 (disc sector 2) to 00:02:06.
        let response = dma_command(
            &mut akiko,
            &mut chip,
            &[
                0x04, 0x00, 0x02, 0x02, 0x00, 0x02, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00,
            ],
        );
        assert_eq!(response[0], 0x04);
        assert_eq!(response[1] & 0x42, 0x42, "play-start status");
        assert!(akiko.playing);

        // Stream the 4 sectors (75 Hz pacing) plus the end notification.
        akiko.write(
            AKIKO_BASE + 0x1F,
            1,
            u32::from(akiko.cdcomrxinx.wrapping_sub(1)),
            &mut chip,
        );
        for _ in 0..16 {
            akiko.tick(CCK_PER_CD_FRAME / 2, &mut chip, &mut ring);
        }
        assert!(!akiko.playing, "play should have reached the end");
        let (left, right) = ring.next_sample();
        assert!((left - 0x1000 as f32 / 32768.0).abs() < 1e-4);
        assert!((right - 0x2000 as f32 / 32768.0).abs() < 1e-4);

        // The play-end packet (type 4, CDS_PLAYEND) lands in the RX ring.
        let rx_base = 0x1000usize;
        let ring_bytes = &chip[rx_base..rx_base + 0x100];
        let found = (0..0x100).any(|i| ring_bytes[i] == 4 && ring_bytes[(i + 1) & 0xFF] == 0x01);
        assert!(found, "play-end notification not found in RX ring");
    }

    /// Two 2048-byte data sectors then four CD-DA sectors in one file.
    /// Track 2 starts at file sector 2; with `pregap` > 0 its INDEX 00
    /// sits there and INDEX 01 that many sectors later.
    fn test_audio_disc(pregap: u32) -> CdImage {
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir();
        let cue = dir.join(format!("copperline-akiko-subq-{pid}-{unique}.cue"));
        let bin = dir.join(format!("copperline-akiko-subq-{pid}-{unique}.bin"));
        let mut bytes = vec![0u8; 2 * DATA_SECTOR_BYTES];
        for _ in 0..4 * (RAW_SECTOR_BYTES / 4) {
            bytes.extend_from_slice(&[0x00, 0x10, 0x00, 0x20]);
        }
        let track2 = if pregap > 0 {
            format!(
                "  TRACK 02 AUDIO\n    INDEX 00 00:00:02\n    INDEX 01 00:00:{:02}\n",
                2 + pregap
            )
        } else {
            "  TRACK 02 AUDIO\n    INDEX 01 00:00:02\n".to_string()
        };
        std::fs::write(
            &cue,
            format!(
                "FILE \"{}\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n{track2}",
                bin.file_name().unwrap().to_string_lossy()
            ),
        )
        .unwrap();
        std::fs::write(&bin, &bytes).unwrap();
        let image = CdImage::load(&cue).unwrap();
        let _ = std::fs::remove_file(&cue);
        let _ = std::fs::remove_file(&bin);
        image
    }

    /// Decode the Q channel back out of an interleaved subcode frame.
    fn q_channel_of(frame: &[u8]) -> [u8; 12] {
        let mut q = [0u8; 12];
        for (i, byte) in frame.iter().enumerate().take(SUBCODE_FRAME_BYTES) {
            if byte & 0x40 != 0 {
                q[i / 8] |= 0x80 >> (i % 8);
            }
        }
        q
    }

    #[test]
    fn q_channel_crc_is_inverted_crc16_ccitt() {
        // CRC-16/XMODEM's check value for "123456789" is $31C3; the Q
        // channel transmits the complement.
        assert_eq!(q_channel_crc(b"123456789"), !0x31C3);
    }

    #[test]
    fn q_channel_position_places_sectors_by_track_index_and_msf() {
        let disc = test_audio_disc(0);
        let tracks = disc.tracks();
        // Data track 1, one sector in: control 4 / ADR 1, index 1,
        // 00:00:01 into the track, absolute 00:02:01 (lead-in offset).
        let q = q_channel_position(tracks, 1);
        assert_eq!(
            &q[..10],
            &[0x41, 0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x02, 0x01]
        );
        assert_eq!([q[10], q[11]], q_channel_crc(&q[..10]).to_be_bytes());
        // Audio track 2 from sector 2: the relative time restarts and the
        // data control bit clears.
        let q = q_channel_position(tracks, 5);
        assert_eq!(
            &q[..10],
            &[0x01, 0x02, 0x01, 0x00, 0x00, 0x03, 0x00, 0x00, 0x02, 0x05]
        );
    }

    #[test]
    fn q_channel_counts_a_pregap_down_as_index_zero_of_the_next_track() {
        // Track 2's INDEX 00 at sector 2, INDEX 01 at sector 3.
        let disc = test_audio_disc(1);
        let tracks = disc.tracks();
        assert_eq!(tracks[1].start_sector, 3);
        let q = q_channel_position(tracks, 2);
        assert_eq!(
            &q[..10],
            &[0x01, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02],
            "pregap sector: track 2 index 0, one frame before INDEX 01"
        );
        let q = q_channel_position(tracks, 3);
        assert_eq!(
            &q[..10],
            &[0x01, 0x02, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03]
        );
    }

    #[test]
    fn audio_play_delivers_a_subcode_frame_per_sector() {
        let mut chip = vec![0u8; 64 * 1024];
        let mut ring = CdAudioRing::default();
        let mut akiko = Akiko::new();
        akiko.insert_disc(test_audio_disc(0));
        akiko.tick(2048, &mut chip, &mut ring);
        akiko.cd_initialized = 2;
        akiko.receive_length = 0;
        const MISC: u32 = 0x0000_1000;
        akiko.write(AKIKO_BASE + 0x14, 4, MISC, &mut chip);
        let sub = (MISC | 0x100) as usize;

        // With subcode DMA off a play streams audio but touches no
        // subcode memory and raises no subcode interrupt.
        akiko.playing = true;
        akiko.play_position = 2;
        akiko.play_end = 6;
        akiko.intreq = 0;
        akiko.tick(CCK_PER_CD_FRAME, &mut chip, &mut ring);
        assert_eq!(akiko.play_position, 3);
        assert_eq!(akiko.intreq & CDINT_SUBCODE, 0);
        assert!(chip[sub..sub + 0x100].iter().all(|&b| b == 0));

        // With CDFLAG_SUBCODE set (the KS cd.device's frame-interrupt
        // feed) every streamed sector lands one frame: upper half first,
        // end marker after the 96 bytes, offset register advanced by 100.
        akiko.write(AKIKO_BASE + 0x24, 4, CDFLAG_SUBCODE, &mut chip);
        akiko.tick(CCK_PER_CD_FRAME, &mut chip, &mut ring);
        assert_eq!(akiko.play_position, 4);
        assert_ne!(akiko.intreq & CDINT_SUBCODE, 0);
        let q = q_channel_of(&chip[sub + 128..sub + 128 + 96]);
        assert_eq!(
            &q[..10],
            &[0x01, 0x02, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x02, 0x03],
            "sector 3: track 2 index 1 at 00:00:01, absolute 00:02:03"
        );
        assert_eq!(
            &chip[sub + 128 + 96..sub + 128 + 100],
            &[0xFF, 0xFF, 0x00, 0x00]
        );
        assert_eq!(akiko.read(AKIKO_BASE + 0x18, 1, &mut chip), 228);
        // P and R-W stay blank: only the Q bit of any byte is ever set.
        assert!(chip[sub + 128..sub + 128 + 96]
            .iter()
            .all(|&b| b & !0x40 == 0));

        // The next sector takes the lower half.
        akiko.tick(CCK_PER_CD_FRAME, &mut chip, &mut ring);
        let q = q_channel_of(&chip[sub..sub + 96]);
        assert_eq!(
            &q[..10],
            &[0x01, 0x02, 0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x02, 0x04]
        );
        assert_eq!(akiko.read(AKIKO_BASE + 0x18, 1, &mut chip), 100);

        // Paused, the pickup holds: no sector, no frame.
        akiko.paused = true;
        akiko.intreq = 0;
        akiko.tick(CCK_PER_CD_FRAME, &mut chip, &mut ring);
        assert_eq!(akiko.play_position, 5);
        assert_eq!(akiko.intreq & CDINT_SUBCODE, 0);
    }

    #[test]
    fn data_read_delivers_the_q_position_with_each_sector() {
        let mut chip = vec![0u8; 256 * 1024];
        let mut akiko = Akiko::new();
        akiko.insert_disc(test_disc());
        const MISC: u32 = 0x0000_1000;
        akiko.write(AKIKO_BASE + 0x14, 4, MISC, &mut chip);
        akiko.addressdata = 0x0001_0000;
        akiko.flags = CDFLAG_ENABLE | CDFLAG_PBX | CDFLAG_CAS | CDFLAG_SUBCODE;
        akiko.data_offset = 3;
        akiko.data_end = 8;
        akiko.pbx = 0x0001;
        akiko.run_sector_read(&mut chip);
        assert_ne!(akiko.intreq & CDINT_SUBCODE, 0);
        let sub = (MISC | 0x100) as usize;
        let q = q_channel_of(&chip[sub + 128..sub + 128 + 96]);
        assert_eq!(
            &q[..10],
            &[0x41, 0x01, 0x01, 0x00, 0x00, 0x03, 0x00, 0x00, 0x02, 0x03],
            "sector 3 of data track 1"
        );
    }

    #[test]
    fn subq_command_reports_the_position_of_a_running_or_paused_play() {
        let mut chip = vec![0u8; 64 * 1024];
        let mut ring = CdAudioRing::default();
        let mut akiko = Akiko::new();
        akiko.insert_disc(test_audio_disc(0));
        akiko.tick(2048, &mut chip, &mut ring);
        akiko.cd_initialized = 2;
        akiko.receive_length = 0;

        // Idle: no valid position, the packet stays zero.
        let response = dma_command(&mut akiko, &mut chip, &[0x06]);
        assert_eq!(response[0], 0x06);
        assert!(
            response[1..13].iter().all(|&b| b == 0),
            "idle SubQ packet: {response:02X?}"
        );

        // Paused two sectors into track 2 (the pickup holds at sector 4):
        // zero, control/ADR, track, index, relative MSF, zero, absolute.
        akiko.playing = true;
        akiko.paused = true;
        akiko.play_position = 4;
        akiko.play_end = 6;
        let response = dma_command(&mut akiko, &mut chip, &[0x06]);
        assert_eq!(
            &response[..13],
            &[0x06, 0x00, 0x00, 0x01, 0x02, 0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x02, 0x04],
            "{response:02X?}"
        );
    }

    /// Run the full DMA command/response round trip the CD32 ROM uses:
    /// rings at $1000, command written to the TX ring, response read
    /// back from the RX ring.
    fn dma_command(akiko: &mut Akiko, chip: &mut [u8], cmd: &[u8]) -> Vec<u8> {
        let mut ring = CdAudioRing::default();
        const MISC: u32 = 0x0000_1000; // addressmisc base
        akiko.write(AKIKO_BASE + 0x14, 4, MISC, chip);
        // Enable TX/RX DMA in flags.
        akiko.write(AKIKO_BASE + 0x24, 4, CDFLAG_TXD | CDFLAG_RXD, chip);

        // Write the command (with checksum) into the TX ring.
        let tx_base = (MISC | 0x200) as usize;
        let start = akiko.cdcomtxinx;
        let mut checksum = 0xFFu8;
        for (i, b) in cmd.iter().enumerate() {
            chip[tx_base + ((start as usize + i) & 0xFF)] = *b;
            checksum = checksum.wrapping_sub(*b);
        }
        chip[tx_base + ((start as usize + cmd.len()) & 0xFF)] = checksum;
        let end = start.wrapping_add(cmd.len() as u8 + 1);
        // Arm RX for a full ring, then kick TX.
        akiko.write(
            AKIKO_BASE + 0x1F,
            1,
            u32::from(akiko.cdcomrxinx.wrapping_sub(1)),
            chip,
        );
        akiko.write(AKIKO_BASE + 0x1D, 1, u32::from(end), chip);

        // Let the DMA delays elapse and the command run.
        let rx_start = akiko.cdcomrxinx;
        for _ in 0..64 {
            akiko.tick(2048, chip, &mut ring);
        }
        let rx_end = akiko.cdcomrxinx;
        let rx_base = MISC as usize;
        let mut out = Vec::new();
        let mut i = rx_start;
        while i != rx_end {
            out.push(chip[rx_base + i as usize]);
            i = i.wrapping_add(1);
        }
        out
    }

    #[test]
    fn status_command_returns_firmware_string_with_checksum() {
        let mut ring = CdAudioRing::default();
        let mut chip = vec![0u8; 64 * 1024];
        let mut akiko = Akiko::new();
        akiko.insert_disc(test_disc());
        // Boot push: media status packet goes out first.
        akiko.tick(2048, &mut chip, &mut ring);
        akiko.cd_initialized = 2;
        akiko.receive_length = 0;

        let response = dma_command(&mut akiko, &mut chip, &[0x17]);
        assert!(response.len() >= 21, "short response: {response:02X?}");
        assert_eq!(response[0], 0x17);
        assert_eq!(&response[2..20], FIRMWARE_VERSION);
        let sum: u8 = response.iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
        assert_eq!(sum, 0xFF, "response checksum invalid: {response:02X?}");
    }

    #[test]
    fn checksum_error_returns_error_packet() {
        let mut ring = CdAudioRing::default();
        let mut chip = vec![0u8; 64 * 1024];
        let mut akiko = Akiko::new();
        akiko.insert_disc(test_disc());
        akiko.tick(2048, &mut chip, &mut ring);
        akiko.cd_initialized = 2;
        akiko.receive_length = 0;

        // Corrupt the checksum by sending command 0x17 with a wrong
        // trailing byte: build manually.
        const MISC: u32 = 0x0000_1000;
        akiko.write(AKIKO_BASE + 0x14, 4, MISC, &mut chip);
        akiko.write(AKIKO_BASE + 0x24, 4, CDFLAG_TXD | CDFLAG_RXD, &mut chip);
        let tx_base = (MISC | 0x200) as usize;
        let start = akiko.cdcomtxinx as usize;
        chip[tx_base + start] = 0x17;
        chip[tx_base + start + 1] = 0x00;
        chip[tx_base + start + 2] = 0x12; // bad checksum
        akiko.write(
            AKIKO_BASE + 0x1F,
            1,
            u32::from(akiko.cdcomrxinx.wrapping_sub(1)),
            &mut chip,
        );
        akiko.write(AKIKO_BASE + 0x1D, 1, (start as u32 + 3) & 0xFF, &mut chip);
        let rx_start = akiko.cdcomrxinx;
        for _ in 0..64 {
            akiko.tick(2048, &mut chip, &mut ring);
        }
        let rx_base = MISC as usize;
        assert_eq!(chip[rx_base + rx_start as usize], 0x15); // cmd|5 error tag
        assert_eq!(
            chip[rx_base + rx_start as usize + 1] & 0xF8,
            CH_ERR_CHECKSUM
        );
    }

    #[test]
    fn data_read_command_dmas_sectors_into_pbx_slots() {
        // Regression example: Lotus Trilogy's CD32 loader arms multiple
        // PBX slots ahead of the data and depends on them filling highest
        // slot first (see the comment on `run_sector_read`).
        let mut ring = CdAudioRing::default();
        let mut chip = vec![0u8; 256 * 1024];
        let mut akiko = Akiko::new();
        akiko.insert_disc(test_disc());
        akiko.tick(2048, &mut chip, &mut ring);
        akiko.cd_initialized = 2;
        akiko.receive_length = 0;

        // READ DATA sectors 0..4 (MSF 00:02:00 - 00:02:04), double speed.
        let response = dma_command(
            &mut akiko,
            &mut chip,
            &[
                0x04, 0x00, 0x02, 0x00, 0x00, 0x02, 0x04, 0x80, 0x40, 0x00, 0x00, 0x00,
            ],
        );
        assert!(!response.is_empty());
        assert_eq!(response[0], 0x04);
        assert_eq!(response[1] & 0x02, 0x02, "data-read ack flag");

        // Point the data DMA at $10000, enable transfers, open 2 slots.
        akiko.write(AKIKO_BASE + 0x10, 4, 0x0001_0000, &mut chip);
        akiko.write(
            AKIKO_BASE + 0x24,
            4,
            CDFLAG_TXD | CDFLAG_RXD | CDFLAG_ENABLE | CDFLAG_PBX | CDFLAG_CAS,
            &mut chip,
        );
        akiko.write(AKIKO_BASE + 0x20, 2, 0x0003, &mut chip);

        // Two CD frames at 2x speed.
        for _ in 0..40 {
            akiko.tick(CCK_PER_CD_FRAME / 8, &mut chip, &mut ring);
        }

        // Highest slot first: sector 0 lands in slot 1, sector 1 in 0.
        let slot1 = 0x0001_0000 + 4096;
        assert_eq!(chip[slot1 + 3], 0); // transfer tag: first sector
        assert_eq!(chip[slot1 + 16], 0x00); // sector 0 payload byte
        let slot0 = 0x0001_0000;
        assert_eq!(chip[slot0 + 3], 1); // second sector tag
        assert_eq!(chip[slot0 + 16], 0x01); // sector 1 payload
                                            // Both slots consumed, PBX interrupt raised.
        assert_eq!(akiko.pbx, 0);
        assert_ne!(akiko.intreq & CDINT_PBX, 0);
    }

    #[test]
    fn data_read_end_delivers_a_position_probe_for_the_next_seek() {
        let mut chip = vec![0u8; 256 * 1024];
        let mut akiko = Akiko::new();
        akiko.insert_disc(test_disc());
        akiko.addressdata = 0x0001_0000;
        akiko.flags = CDFLAG_ENABLE | CDFLAG_PBX | CDFLAG_CAS;
        akiko.data_offset = 0;
        akiko.data_end = 2;
        akiko.pbx = 0x0007;

        // Sectors 0 and 1 consume the two highest armed buffers.
        akiko.run_sector_read(&mut chip);
        akiko.run_sector_read(&mut chip);
        assert_eq!(akiko.sector_counter, 2);
        assert_eq!(akiko.pbx, 0x0001);

        // Reaching the exclusive end re-presents the final valid sector. The
        // ROM uses its MSF to detect that a later backwards request needs a
        // STOP/reseek, rather than sleeping forever on an empty PBX ring.
        akiko.intreq = 0;
        akiko.run_sector_read(&mut chip);
        assert_eq!(akiko.data_offset, -1);
        assert_eq!(akiko.data_end, -1);
        assert_eq!(akiko.pbx, 0);
        assert_eq!(akiko.sector_counter, 3);
        let slot0 = 0x0001_0000;
        assert_eq!(chip[slot0 + 3], 2);
        assert_eq!(chip[slot0 + 16], 1);
        assert_ne!(akiko.intreq & CDINT_PBX, 0);
        assert!(!akiko.activity_led_on());
    }

    #[test]
    fn intreq_read_returns_raw_latches() {
        // The CDINTREQ status read returns the raw request latches
        // regardless of INTENA, which gates only the INT2 line. Pollers
        // of the direct DRIVE port depend on this: they spin on
        // DRIVEXMIT/DRIVERECV without ever enabling those sources
        // (regression example: Jim Power CD32 polls bit 30 before each
        // PIO command byte).
        let mut chip = no_chip();
        let mut akiko = Akiko::new();
        akiko.intreq = CDINT_RXDMADONE | CDINT_DRIVEXMIT;
        akiko.intena = 0;
        assert_eq!(
            akiko.read(AKIKO_BASE + 0x04, 4, &mut chip),
            CDINT_RXDMADONE | CDINT_DRIVEXMIT
        );
        // The latch itself survives the read.
        assert_eq!(akiko.intreq, CDINT_RXDMADONE | CDINT_DRIVEXMIT);
    }

    #[test]
    fn pio_command_roundtrip_polls_raw_latches_with_interrupts_disabled() {
        // A guest driving the DRIVE port directly leaves INTENA clear and
        // spins on the CDINTREQ latches instead: DRIVEXMIT before each
        // command byte written to $28, DRIVERECV before each response
        // byte read back from $28 (regression example: Jim Power CD32's
        // loader, which hangs at its intro if the poll cannot see the
        // latches).
        let mut ring = CdAudioRing::default();
        let mut chip = vec![0u8; 64 * 1024];
        let mut akiko = Akiko::new();
        akiko.insert_disc(test_disc());
        akiko.tick(2048, &mut chip, &mut ring);
        akiko.cd_initialized = 2;
        akiko.receive_length = 0;
        akiko.intena = 0;
        // The ROM's last response drain leaves the transmitter-ready latch.
        akiko.intreq = CDINT_DRIVEXMIT;
        // The guest switches both directions to PIO.
        akiko.write(AKIKO_BASE + 0x24, 4, 0, &mut chip);

        // LED command with the status-request bit: poll, then write, per byte.
        for &byte in &[0x15u8, 0x81, 0x69] {
            assert_ne!(
                akiko.read(AKIKO_BASE + 0x04, 4, &mut chip) & CDINT_DRIVEXMIT,
                0,
                "transmit-ready latch visible to the poll"
            );
            akiko.write(AKIKO_BASE + 0x28, 1, u32::from(byte), &mut chip);
        }
        assert_eq!(
            akiko.read(AKIKO_BASE + 0x04, 4, &mut chip) & CDINT_DRIVEXMIT,
            0,
            "transmitter busy while the command executes"
        );
        akiko.tick(CMD_EXEC_DELAY_CCK, &mut chip, &mut ring);

        // Response: poll DRIVERECV, then read each byte back from $28.
        let mut response = Vec::new();
        while akiko.read(AKIKO_BASE + 0x04, 4, &mut chip) & CDINT_DRIVERECV != 0 {
            response.push(akiko.read(AKIKO_BASE + 0x28, 1, &mut chip) as u8);
        }
        assert_eq!(response, vec![0x15, 0x01, 0xE9]);
        // The drained response re-arms the transmitter-ready latch.
        assert_ne!(akiko.intreq & CDINT_DRIVEXMIT, 0);
    }

    /// Two-bank space: chip at 0 and "fast" at $200000, like a CD32 with
    /// a Zorro II RAM expansion.
    struct SplitSpace {
        chip: Vec<u8>,
        fast: Vec<u8>,
        fast_base: u32,
    }

    impl DmaSpace for SplitSpace {
        fn dma_byte_mut(&mut self, addr: u32) -> Option<&mut u8> {
            let a = addr as usize;
            if a < self.chip.len() {
                return Some(&mut self.chip[a]);
            }
            let f = self.fast_base as usize;
            if a >= f && a < f + self.fast.len() {
                return Some(&mut self.fast[a - f]);
            }
            None
        }
    }

    #[test]
    fn dma_reaches_fast_ram_buffers() {
        // Akiko drives a 24-bit address bus: a guest may point the
        // command/response rings at fast RAM (AROS allocates them
        // MEMF_24BITDMA, which prefers fast when it exists). The rings
        // must not alias into chip RAM.
        let mut ring = CdAudioRing::default();
        let mut space = SplitSpace {
            chip: vec![0u8; 64 * 1024],
            fast: vec![0u8; 64 * 1024],
            fast_base: 0x0020_0000,
        };
        let mut akiko = Akiko::new();
        akiko.insert_disc(test_disc());
        akiko.tick(2048, &mut space, &mut ring);
        akiko.cd_initialized = 2;
        akiko.receive_length = 0;

        // Misc buffer (rings) in fast RAM at $201000.
        const MISC: u32 = 0x0020_1000;
        akiko.write(AKIKO_BASE + 0x14, 4, MISC, &mut space);
        akiko.write(AKIKO_BASE + 0x24, 4, CDFLAG_TXD | CDFLAG_RXD, &mut space);
        let tx_off = (MISC - 0x0020_0000 + 0x200) as usize;
        let start = akiko.cdcomtxinx;
        space.fast[tx_off + (start as usize & 0xFF)] = 0x17; // STATUS, seq 1
        space.fast[tx_off + ((start as usize + 1) & 0xFF)] = 0xFFu8.wrapping_sub(0x17);
        akiko.write(
            AKIKO_BASE + 0x1F,
            1,
            u32::from(akiko.cdcomrxinx.wrapping_sub(1)),
            &mut space,
        );
        let rx0 = akiko.cdcomrxinx as usize;
        akiko.write(
            AKIKO_BASE + 0x1D,
            1,
            u32::from(start.wrapping_add(2)),
            &mut space,
        );
        for _ in 0..64 {
            akiko.tick(2048, &mut space, &mut ring);
        }

        // The response landed in the fast-RAM ring, and chip RAM at the
        // aliased offset stayed untouched.
        let rx_off = (MISC - 0x0020_0000) as usize;
        assert_eq!(
            space.fast[rx_off + (rx0 & 0xFF)] & 0x0F,
            0x07,
            "response in fast"
        );
        assert_eq!(
            space.chip[(MISC as usize & 0xFFFF) + (rx0 & 0xFF)],
            0,
            "chip clean"
        );
    }

    #[test]
    fn command_execution_waits_for_the_response_ring_to_drain() {
        // An unsolicited notification can land in the response channel
        // while a command's turnaround runs. Executing the command then
        // would clobber the undelivered packet in result_buffer, so the
        // command holds until the ring drains.
        let mut ring = CdAudioRing::default();
        let mut chip = vec![0u8; 64 * 1024];
        let mut akiko = Akiko::new();
        akiko.insert_disc(test_disc());
        akiko.tick(2048, &mut chip, &mut ring);
        akiko.cd_initialized = 2;
        akiko.receive_length = 0;

        // Kick a STATUS command; the RX window stays closed for now.
        const MISC: u32 = 0x0000_1000;
        akiko.write(AKIKO_BASE + 0x14, 4, MISC, &mut chip);
        akiko.write(AKIKO_BASE + 0x24, 4, CDFLAG_TXD | CDFLAG_RXD, &mut chip);
        let tx_base = (MISC | 0x200) as usize;
        let start = akiko.cdcomtxinx;
        chip[tx_base + (start as usize & 0xFF)] = 0x17; // STATUS, seq 1
        chip[tx_base + ((start as usize + 1) & 0xFF)] = 0xFFu8.wrapping_sub(0x17);
        akiko.write(
            AKIKO_BASE + 0x1D,
            1,
            u32::from(start.wrapping_add(2)),
            &mut chip,
        );
        // TX restart delay, then one ring byte per pump.
        for _ in 0..4 {
            akiko.tick(400, &mut chip, &mut ring);
        }
        assert!(akiko.command_active > 0, "turnaround armed");
        assert_eq!(akiko.receive_length, 0);

        // Mid-turnaround, the drive volunteers a media packet.
        akiko.eject_disc();
        akiko.tick(500, &mut chip, &mut ring);
        assert!(akiko.receive_length > 0, "notification queued");
        assert_eq!(akiko.result_buffer[0], 0x0A);

        // The turnaround expires, but the packet is still undelivered:
        // the command must hold and the packet must survive.
        for _ in 0..4 {
            akiko.tick(CMD_EXEC_DELAY_CCK, &mut chip, &mut ring);
        }
        assert!(akiko.command_active > 0, "command deferred");
        assert_eq!(akiko.result_buffer[0], 0x0A, "packet survives");

        // Open the RX window: the notification drains, then the command
        // executes and its response follows it into the ring.
        let rx0 = akiko.cdcomrxinx as usize;
        akiko.write(
            AKIKO_BASE + 0x1F,
            1,
            u32::from(akiko.cdcomrxinx.wrapping_sub(1)),
            &mut chip,
        );
        for _ in 0..8 {
            akiko.tick(CMD_EXEC_DELAY_CCK, &mut chip, &mut ring);
        }
        assert_eq!(akiko.command_active, 0, "command ran after the drain");
        let rx = &chip[MISC as usize..MISC as usize + 0x100];
        assert_eq!(rx[rx0 & 0xFF], 0x0A, "notification delivered first");
        assert_eq!(rx[(rx0 + 3) & 0xFF] & 0x0F, 0x07, "then the response");
    }

    #[test]
    fn command_execution_is_paced_by_emulated_time_not_accesses() {
        // The drive's microcontroller answers a command CMD_EXEC_DELAY_CCK
        // after its last byte, never synchronously inside the guest's
        // register write. Drivers finish arming their completion interrupt
        // after kicking the command; a response delivered mid-arming
        // inverts the handshake (observed with AROS cd.device).
        let mut ring = CdAudioRing::default();
        let mut chip = vec![0u8; 64 * 1024];
        let mut akiko = Akiko::new();
        akiko.insert_disc(test_disc());
        akiko.tick(2048, &mut chip, &mut ring);
        akiko.cd_initialized = 2;
        akiko.receive_length = 0;

        const MISC: u32 = 0x0000_1000;
        akiko.write(AKIKO_BASE + 0x14, 4, MISC, &mut chip);
        akiko.write(AKIKO_BASE + 0x24, 4, CDFLAG_TXD | CDFLAG_RXD, &mut chip);
        let tx_base = (MISC | 0x200) as usize;
        let start = akiko.cdcomtxinx;
        chip[tx_base + (start as usize & 0xFF)] = 0x17; // STATUS, seq 1
        chip[tx_base + ((start as usize + 1) & 0xFF)] = 0xFFu8.wrapping_sub(0x17);
        akiko.write(
            AKIKO_BASE + 0x1F,
            1,
            u32::from(akiko.cdcomrxinx.wrapping_sub(1)),
            &mut chip,
        );
        let rx_before = akiko.cdcomrxinx;
        akiko.write(
            AKIKO_BASE + 0x1D,
            1,
            u32::from(start.wrapping_add(2)),
            &mut chip,
        );

        // Hammer the register file without advancing time: no response.
        for _ in 0..64 {
            let _ = akiko.read(AKIKO_BASE + 0x04, 4, &mut chip);
        }
        assert_eq!(akiko.cdcomrxinx, rx_before, "no response before the delay");

        // Advance past the turnaround: the response arrives.
        for _ in 0..8 {
            akiko.tick(CMD_EXEC_DELAY_CCK / 4, &mut chip, &mut ring);
        }
        assert_ne!(akiko.cdcomrxinx, rx_before, "response after the delay");
    }

    #[test]
    fn command_dma_folds_to_the_transmit_page_through_a_packet_wrap() {
        // Kickstart's command producer uses eight-bit index arithmetic, so a
        // packet whose bytes straddle index $FF wraps to the start of the
        // 256-byte TX page (register trace: an idle-screen LED packet at
        // txinx $FE..$00 lands its checksum at page offset 0). Akiko's DMA
        // address folds the same way; carrying into the following page reads
        // unrelated memory there and fails the packet's checksum.
        let mut ring = CdAudioRing::default();
        let mut chip = vec![0u8; 128 * 1024];
        let mut akiko = Akiko::new();
        akiko.insert_disc(test_disc());
        akiko.cd_initialized = 2;

        const MISC: u32 = 0x0000_1000;
        akiko.write(AKIKO_BASE + 0x14, 4, MISC, &mut chip);
        akiko.write(AKIKO_BASE + 0x24, 4, CDFLAG_TXD | CDFLAG_RXD, &mut chip);
        akiko.cdcomtxinx = 0xF8;
        akiko.cdcomtxcmp = 0xF8;

        let command = [
            0x04, 0x00, 0x02, 0x00, 0x00, 0x02, 0x04, 0x80, 0x40, 0x00, 0x00, 0x00,
        ];
        let tx_base = (MISC | 0x200) as usize;
        let mut checksum = 0xFFu8;
        for (i, byte) in command.iter().copied().enumerate() {
            chip[tx_base + (0xF8 + i) % 256] = byte;
            checksum = checksum.wrapping_sub(byte);
        }
        chip[tx_base + (0xF8 + command.len()) % 256] = checksum;
        // Poison the byte past the page end: a carried (non-folding) DMA
        // address would read this instead of the wrapped payload.
        chip[tx_base + 0x100] = 0x5A;
        let end = 0xF8u8.wrapping_add(command.len() as u8 + 1);
        akiko.write(AKIKO_BASE + 0x1D, 1, u32::from(end), &mut chip);

        for _ in 0..24 {
            akiko.tick(2048, &mut chip, &mut ring);
        }
        assert_eq!(akiko.cdcomtxinx, end);
        assert!(!akiko.checksum_error);
        assert_eq!(akiko.data_offset, 0, "the READ command executed");

        // A later packet starts from base + the visible index.
        akiko.receive_length = 0;
        akiko.receive_offset = 0;
        akiko.command_active = 0;
        let next = [0x15, 0x00, 0xEA];
        for (i, byte) in next.iter().copied().enumerate() {
            chip[tx_base + (end as usize + i) % 256] = byte;
        }
        let next_end = end.wrapping_add(next.len() as u8);
        akiko.write(AKIKO_BASE + 0x1D, 1, u32::from(next_end), &mut chip);
        for _ in 0..8 {
            akiko.tick(2048, &mut chip, &mut ring);
        }
        assert_eq!(akiko.cdcomtxinx, next_end);
        assert!(!akiko.checksum_error);
    }

    #[test]
    fn response_dma_wraps_within_the_receive_page_when_extended() {
        let mut ring = CdAudioRing::default();
        let mut chip = vec![0u8; 64 * 1024];
        let mut akiko = Akiko::new();
        akiko.cd_initialized = 2;

        const MISC: u32 = 0x0000_1000;
        akiko.write(AKIKO_BASE + 0x14, 4, MISC, &mut chip);
        akiko.write(AKIKO_BASE + 0x24, 4, CDFLAG_RXD, &mut chip);
        akiko.cdcomrxinx = 0xFE;
        akiko.cdcomrxcmp = 0xFE;
        chip[MISC as usize] = 0xCC;

        akiko.result_buffer[0] = 0x42;
        akiko.result_buffer[1] = 0x01;
        assert!(akiko.start_return_data(2));

        // Deliver one byte, then extend the same response across $FF. The RX
        // ring is one 256-byte page; unlike TX packet fetching, it must wrap
        // instead of spilling into the adjacent subcode page.
        akiko.write(AKIKO_BASE + 0x1F, 1, 0xFF, &mut chip);
        akiko.tick(DMA_RESTART_DELAY_CCK, &mut chip, &mut ring);
        assert_eq!(akiko.receive_offset, 1);
        akiko.write(AKIKO_BASE + 0x1F, 1, 0x01, &mut chip);
        akiko.tick(DMA_RESTART_DELAY_CCK, &mut chip, &mut ring);

        let checksum = 0xFFu8.wrapping_sub(0x42).wrapping_sub(0x01);
        assert_eq!(
            &chip[MISC as usize + 0xFE..MISC as usize + 0x100],
            &[0x42, 0x01]
        );
        assert_eq!(
            chip[MISC as usize], checksum,
            "checksum wrapped to page start"
        );
        assert_eq!(chip[MISC as usize + 0x100], 0, "subcode page stayed clean");
        assert_eq!(akiko.cdcomrxinx, 0x01);
    }

    #[test]
    fn command_dma_rebases_between_packets_queued_in_one_window() {
        let mut ring = CdAudioRing::default();
        let mut chip = vec![0u8; 64 * 1024];
        let mut akiko = Akiko::new();
        akiko.cd_initialized = 2;

        const MISC: u32 = 0x0000_1000;
        akiko.write(AKIKO_BASE + 0x14, 4, MISC, &mut chip);
        akiko.write(AKIKO_BASE + 0x24, 4, CDFLAG_TXD, &mut chip);
        akiko.cdcomtxinx = 0xFE;
        akiko.cdcomtxcmp = 0xFE;
        let tx_base = (MISC | 0x200) as usize;

        // The first LED packet wraps at the page end (its checksum lands at
        // page offset 0, where the producer's eight-bit index arithmetic put
        // it); the second is queued right behind it. One comparator window
        // covers both packets, and the parser boundary rebases the DMA
        // address for the second one.
        chip[tx_base + 0xFE..tx_base + 0x100].copy_from_slice(&[0x15, 0x00]);
        chip[tx_base] = 0xEA;
        chip[tx_base + 0x01..tx_base + 0x04].copy_from_slice(&[0x25, 0x01, 0xD9]);
        // Poison the byte past the page end: a carried (non-folding) DMA
        // address would read this instead of the wrapped checksum.
        chip[tx_base + 0x100] = 0x5A;
        akiko.write(AKIKO_BASE + 0x1D, 1, 0x04, &mut chip);

        for _ in 0..16 {
            akiko.tick(2048, &mut chip, &mut ring);
        }
        assert_eq!(akiko.cdcomtxinx, 0x04);
        assert_eq!(akiko.command, 0x25);
        assert!(!akiko.checksum_error);
    }

    #[test]
    fn response_waits_for_an_enabled_tx_completion_to_be_acknowledged() {
        // TX and RX completion share one interrupt server in the ROM driver.
        // A response becoming visible while the enabled TX completion is
        // still pending makes that server account the RX ring against the
        // wrong event and can leave it stopped at its comparator indefinitely.
        let mut ring = CdAudioRing::default();
        let mut chip = vec![0u8; 64 * 1024];
        let mut akiko = Akiko::new();
        akiko.cd_initialized = 2;

        const MISC: u32 = 0x0000_1000;
        akiko.write(AKIKO_BASE + 0x14, 4, MISC, &mut chip);
        akiko.write(AKIKO_BASE + 0x24, 4, CDFLAG_RXD, &mut chip);
        akiko.write(AKIKO_BASE + 0x1F, 1, 3, &mut chip);
        akiko.command = 0x12; // PAUSE, sequence 1
        akiko.command_buffer[0] = 0x12;
        akiko.command_length = 1;
        akiko.command_active = 1;
        akiko.intreq = CDINT_TXDMADONE;
        akiko.intena = CDINT_TXDMADONE | CDINT_RXDMADONE;

        akiko.tick(DMA_RESTART_DELAY_CCK, &mut chip, &mut ring);
        assert_eq!(akiko.command_active, 1, "command remains deferred");
        assert_eq!(akiko.receive_length, 0, "no response exposed yet");
        assert_eq!(akiko.cdcomrxinx, 0);

        // Writing TXCMP acknowledges TXDMADONE. The next controller tick may
        // execute the command and deliver the response through RX DMA.
        akiko.write(AKIKO_BASE + 0x1D, 1, 0, &mut chip);
        akiko.tick(1, &mut chip, &mut ring);
        assert_eq!(akiko.intreq & CDINT_TXDMADONE, 0);
        assert_ne!(akiko.intreq & CDINT_RXDMADONE, 0);
        assert_eq!(akiko.cdcomrxinx, 3);
    }

    #[test]
    fn toc_dump_streams_entries_after_leadin_play_command() {
        let mut ring = CdAudioRing::default();
        let mut chip = vec![0u8; 64 * 1024];
        let mut akiko = Akiko::new();
        akiko.insert_disc(test_disc());
        // The test drive is already spun up with its lead-in read.
        akiko.toc_spin_up_cck = 0;
        akiko.tick(2048, &mut chip, &mut ring);
        akiko.cd_initialized = 2;
        akiko.receive_length = 0;

        // Play from MSF 00:00:00 (inside the lead-in): a TOC request.
        let response = dma_command(
            &mut akiko,
            &mut chip,
            &[
                0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ],
        );
        assert_eq!(response[0], 0x04);
        assert_eq!(
            akiko.toc_counter, -1,
            "cached TOC should complete at the firmware transport rate"
        );

        // Find the lead-out (A2) packet in the RX ring: packet type 6,
        // status 0x0A, point byte A2, MSF = 8 sectors + lead-in =
        // 00:02:08 in BCD.
        let rx_base = 0x1000usize;
        let ring = &chip[rx_base..rx_base + 0x100];
        let found = (0..0x100).any(|i| {
            ring[i] == 6
                && ring[(i + 1) & 0xFF] == 0x0A
                && ring[(i + 5) & 0xFF] == 0xA2
                && ring[(i + 10) & 0xFF] == 0x00
                && ring[(i + 11) & 0xFF] == 0x02
                && ring[(i + 12) & 0xFF] == 0x08
        });
        assert!(found, "lead-out TOC packet not found in RX ring");
    }

    #[test]
    fn toc_transport_rate_ignores_double_speed_bit() {
        fn frame_phase_after_dump(speed_bit: u8) -> i32 {
            let mut ring = CdAudioRing::default();
            let mut chip = vec![0u8; 64 * 1024];
            let mut akiko = Akiko::new();
            akiko.insert_disc(test_disc());
            // The test drive is already spun up with its lead-in read.
            akiko.toc_spin_up_cck = 0;
            akiko.tick(2048, &mut chip, &mut ring);
            akiko.cd_initialized = 2;
            akiko.receive_length = 0;

            let response = dma_command(
                &mut akiko,
                &mut chip,
                &[
                    0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, speed_bit, 0x00, 0x00, 0x00,
                ],
            );
            assert_eq!(response[0], 0x04);
            assert_eq!(akiko.toc_counter, -1, "cached TOC should complete");
            akiko.frame_counter_cck
        }

        assert_eq!(
            frame_phase_after_dump(0),
            frame_phase_after_dump(0x40),
            "the data-speed bit must not change cached TOC pacing"
        );
    }

    #[test]
    fn tx_dma_drains_the_command_ring_while_a_toc_dump_streams() {
        // Kickstart's driver queues an unpause right behind its lead-in
        // request, then a 3-byte LED packet for every TOC entry it
        // receives. The drive acts on none of them until the dump ends,
        // but Akiko's TX DMA must keep draining the ring into the drive
        // meanwhile: on a 39-track disc the 126 LED packets (378 bytes)
        // lap the 256-byte ring otherwise, and the lapped bytes -- and
        // the held unpause, overwritten by them -- parse as garbage
        // that fails its checksum once the dump ends (observed at boot
        // with Pinball Illusions CD32, 39 tracks: four checksum-error
        // replies the real drive never sends).
        fn queue_packet(akiko: &mut Akiko, chip: &mut [u8], tx_base: usize, packet: &[u8]) {
            // Eight-bit producer index arithmetic, like Kickstart's.
            let start = akiko.cdcomtxcmp as usize;
            let mut checksum = 0xFFu8;
            for (i, b) in packet.iter().enumerate() {
                chip[tx_base + ((start + i) & 0xFF)] = *b;
                checksum = checksum.wrapping_sub(*b);
            }
            chip[tx_base + ((start + packet.len()) & 0xFF)] = checksum;
            let end = (start + packet.len() + 1) as u8;
            akiko.write(AKIKO_BASE + 0x1D, 1, u32::from(end), chip);
        }
        fn packet_len(first: u8) -> usize {
            match first & 0x0F {
                0x00 => 2,
                0x06 => 16,
                0x07 => 21,
                _ => 3,
            }
        }

        let mut ring = CdAudioRing::default();
        let mut chip = vec![0u8; 64 * 1024];
        let mut akiko = Akiko::new();
        akiko.insert_disc(test_disc());
        akiko.toc_spin_up_cck = 0;
        akiko.tick(2048, &mut chip, &mut ring);
        akiko.cd_initialized = 2;
        akiko.receive_length = 0;
        // A 39-track disc's cached TOC: three session entries plus one
        // per track, 42 points x TOC_REPEAT = 126 packets.
        const TRACKS: u8 = 39;
        let mut toc = vec![
            TocEntry {
                point: 0xA0,
                control: 0,
                address: 1,
            },
            TocEntry {
                point: 0xA1,
                control: 0,
                address: u32::from(TRACKS),
            },
            TocEntry {
                point: 0xA2,
                control: 0,
                address: 300_000,
            },
        ];
        for track in 1..=TRACKS {
            toc.push(TocEntry {
                point: track,
                control: if track == 1 { 0x04 } else { 0 },
                address: u32::from(track) * 5_000,
            });
        }
        akiko.toc = toc;
        let toc_packets = 42 * TOC_REPEAT as usize;

        const MISC: u32 = 0x0000_1000;
        akiko.write(AKIKO_BASE + 0x14, 4, MISC, &mut chip);
        akiko.write(AKIKO_BASE + 0x24, 4, CDFLAG_TXD | CDFLAG_RXD, &mut chip);
        let tx_base = (MISC | 0x200) as usize;
        let rx_base = MISC as usize;
        akiko.write(
            AKIKO_BASE + 0x1F,
            1,
            u32::from(akiko.cdcomrxinx.wrapping_sub(1)),
            &mut chip,
        );

        // Lead-in play (the TOC request), then the unpause queued behind
        // it before a single entry has arrived.
        queue_packet(
            &mut akiko,
            &mut chip,
            tx_base,
            &[
                0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00,
            ],
        );
        queue_packet(&mut akiko, &mut chip, tx_base, &[0x23]);

        // Consume the response stream as the driver would, and answer
        // every TOC entry with an LED toggle.
        let mut stream: Vec<u8> = Vec::new();
        let mut packets: Vec<Vec<u8>> = Vec::new();
        let mut parsed = 0usize;
        let mut seen = akiko.cdcomrxinx;
        let mut toc_seen = 0usize;
        let mut sequence = 3u8;
        let mut max_lag = 0u8;
        let mut dump_done_at = None;
        for step in 0..20_000 {
            akiko.tick(500, &mut chip, &mut ring);
            while seen != akiko.cdcomrxinx {
                stream.push(chip[rx_base + seen as usize]);
                seen = seen.wrapping_add(1);
            }
            if akiko.cdcomrxcmp.wrapping_sub(akiko.cdcomrxinx) < 32 {
                akiko.write(
                    AKIKO_BASE + 0x1F,
                    1,
                    u32::from(akiko.cdcomrxinx.wrapping_sub(1)),
                    &mut chip,
                );
            }
            while parsed < stream.len() && stream.len() - parsed >= packet_len(stream[parsed]) {
                let len = packet_len(stream[parsed]);
                let packet = stream[parsed..parsed + len].to_vec();
                parsed += len;
                if packet[0] & 0x0F == 0x06 {
                    toc_seen += 1;
                    let led = [(sequence << 4) | 0x05, (toc_seen & 1) as u8];
                    sequence = (sequence % 15) + 1;
                    queue_packet(&mut akiko, &mut chip, tx_base, &led);
                }
                packets.push(packet);
            }
            max_lag = max_lag.max(akiko.cdcomtxcmp.wrapping_sub(akiko.cdcomtxinx));
            if akiko.toc_counter < 0 && dump_done_at.is_none() && toc_seen == toc_packets {
                dump_done_at = Some(step);
            }
            if dump_done_at.is_some_and(|at| step > at + 64)
                && akiko.tx_fifo.is_empty()
                && akiko.command_active == 0
                && akiko.receive_length == 0
            {
                break;
            }
        }

        // The TX DMA kept up with the producer instead of letting the
        // ring lap: a packet or two in flight at most, never 256 bytes.
        assert!(max_lag < 16, "TX ring lagged by {max_lag} bytes");
        assert_eq!(akiko.cdcomtxinx, akiko.cdcomtxcmp, "ring fully drained");
        assert!(akiko.tx_fifo.is_empty(), "drive parsed every byte");

        // Response order: the request's ack, every TOC entry, and only
        // then the unpause reply -- intact, not an error, and nothing
        // else (the LED packets asked for no reply, and none of them
        // parsed as garbage).
        assert_eq!(packets[0][0], 0x14, "lead-in play acknowledged");
        let toc_replies: Vec<&Vec<u8>> = packets.iter().filter(|p| p[0] & 0x0F == 6).collect();
        assert_eq!(toc_replies.len(), toc_packets);
        assert_eq!(
            packets.len(),
            toc_packets + 2,
            "unexpected replies: {:02x?}",
            packets
                .iter()
                .filter(|p| p[0] & 0x0F != 6)
                .collect::<Vec<_>>()
        );
        let last = packets.last().unwrap();
        assert_eq!(
            last[0], 0x23,
            "unpause answered after the dump: {last:02x?}"
        );
        assert_eq!(last[1] & 0x80, 0, "unpause not an error: {last:02x?}");
        assert!(
            packets[packets.len() - 2][0] & 0x0F == 6,
            "the unpause waited for the last TOC entry"
        );
    }

    #[test]
    fn toc_dump_streams_track_entries_before_session_entries() {
        // The firmware's TOC dump delivers the track entries first and the
        // A0/A1/A2 session entries last. A driver may treat the lead-out
        // entry (A2) as end-of-TOC and stop parsing, so a track entry
        // arriving after A2 would be lost.
        let mut ring = CdAudioRing::default();
        let mut chip = vec![0u8; 64 * 1024];
        let mut akiko = Akiko::new();
        akiko.insert_disc(test_disc());
        // The test drive is already spun up with its lead-in read.
        akiko.toc_spin_up_cck = 0;
        akiko.tick(2048, &mut chip, &mut ring);
        akiko.cd_initialized = 2;
        akiko.receive_length = 0;

        let pre = akiko.cdcomrxinx as usize;
        let response = dma_command(
            &mut akiko,
            &mut chip,
            &[
                0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ],
        );
        assert_eq!(response[0], 0x04);
        assert_eq!(
            akiko.toc_counter, -1,
            "cached TOC should complete at the firmware transport rate"
        );
        // The ring serializes responses: the 3-byte command response
        // first, then the TOC packets.
        let start = pre + 3;

        // One data track: 4 entries x TOC_REPEAT packets of 16 bytes,
        // laid out in DMA order from the pre-dump RX index (no wrap).
        let rx_base = 0x1000usize;
        let ring = &chip[rx_base..rx_base + 0x100];
        let points: Vec<u8> = (0..4 * TOC_REPEAT as usize)
            .map(|p| ring[(start + p * 16 + 5) & 0xFF])
            .collect();
        let expected: Vec<u8> = [0x01, 0xA0, 0xA1, 0xA2]
            .iter()
            .flat_map(|&pt| std::iter::repeat_n(pt, TOC_REPEAT as usize))
            .collect();
        assert_eq!(points, expected, "TOC stream order: tracks then A0/A1/A2");
    }
}
