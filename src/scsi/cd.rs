// SPDX-License-Identifier: GPL-3.0-or-later

//! SCSI-2 CD-ROM target: a read-only removable-medium device (peripheral
//! type 5) backed by a cue/bin or ISO image.
//!
//! The command set covers what Amiga CD filesystems (CDFileSystem,
//! CacheCDFS, AsimCDFS and friends) drive through a host adapter's
//! scsi.device: INQUIRY, READ CAPACITY / READ TOC / READ HEADER /
//! READ SUB-CHANNEL, the READ family in 2048-byte blocks, READ CD, mode
//! pages 01h/0Dh/0Eh/2Ah, and the audio-play group. CD-DA playback is
//! real: the drive paces sectors at 75 per second of emulated time and
//! streams them into Paula's CD-audio ring, where they are mixed into
//! the host output at 44.1 kHz -- as if the drive's analogue output were
//! cabled to the machine's audio path -- and the sub-channel reports the
//! live playback position.

use super::{
    be16, be24, be32, cdb_len, ScsiExec, ASC_ILLEGAL_MODE_FOR_THIS_TRACK, ASC_INVALID_FIELD_IN_CDB,
    ASC_INVALID_OPCODE, ASC_LBA_OUT_OF_RANGE, ASC_LUN_NOT_SUPPORTED, ASC_MEDIUM_MAY_HAVE_CHANGED,
    ASC_MEDIUM_NOT_PRESENT, CHECK_CONDITION, GOOD, SENSE_LEN, SK_HARDWARE_ERROR,
    SK_ILLEGAL_REQUEST, SK_NOT_READY, SK_UNIT_ATTENTION,
};
use crate::cdrom::{
    CdImage, CdTrack, DATA_SECTOR_BYTES, LEADIN_SECTORS, RAW_SECTOR_BYTES, SECTORS_PER_SECOND,
};
use crate::chipset::paula::{CdAudioRing, PAULA_CLOCK_HZ};
use std::path::{Path, PathBuf};

// READ SUB-CHANNEL audio status codes.
const AUDIO_STATUS_PLAYING: u8 = 0x11;
const AUDIO_STATUS_PAUSED: u8 = 0x12;
const AUDIO_STATUS_COMPLETED: u8 = 0x13;
const AUDIO_STATUS_NONE: u8 = 0x15;

/// Colour clocks per CD frame: audio plays 75 sectors per second of
/// emulated time, in step with the mixer draining the ring at 44.1 kHz.
const CCK_PER_CD_FRAME: u32 = PAULA_CLOCK_HZ / SECTORS_PER_SECOND;

/// Tray travel time for a runtime disc swap: eject and mount stay far
/// enough apart in emulated time that a filesystem polling TEST UNIT
/// READY observes the absent-then-present transition.
const TRAY_CCK: i64 = PAULA_CLOCK_HZ as i64;

/// Audio playback progression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum PlayPhase {
    /// No play operation (or an explicit stop).
    Idle,
    Playing,
    Paused,
    /// The play range finished.
    Done,
}

/// A disc travelling in the tray after a runtime swap.
#[derive(serde::Serialize, serde::Deserialize)]
struct PendingDisc {
    image: CdImage,
    path: PathBuf,
    tray_cck: i64,
}

/// A SCSI-2 CD-ROM target backed by a CD image.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ScsiCdRom {
    image: CdImage,
    /// Path the drive was opened from (the cue sheet or ISO), for logs.
    path: PathBuf,
    /// Tray closed with the medium in place. START STOP UNIT's eject and
    /// load strobes toggle it; reloading raises a unit attention.
    loaded: bool,
    /// Report a medium-change unit attention on the next command.
    unit_attention: bool,
    sense: [u8; SENSE_LEN],
    /// Audio playback state; the position pair is meaningful outside
    /// `Idle`.
    play: PlayPhase,
    /// Current playback disc sector.
    play_pos: u32,
    /// One past the last sector of the play range.
    play_end: u32,
    /// Colour-clock countdown pacing audio sector production at 75 Hz.
    audio_cck: i32,
    /// Disc waiting in the tray after a runtime swap; mounts (with the
    /// medium-change unit attention) when the countdown expires.
    pending: Option<PendingDisc>,
}

impl ScsiCdRom {
    /// Open a CD-ROM unit from a cue sheet or bare ISO image.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let image = CdImage::load(path)?;
        Ok(Self {
            image,
            path: path.to_path_buf(),
            loaded: true,
            unit_attention: false,
            sense: [0u8; SENSE_LEN],
            play: PlayPhase::Idle,
            play_pos: 0,
            play_end: 0,
            audio_cck: 0,
            pending: None,
        })
    }

    /// Whether a disc is mounted or waiting in the tray.
    pub fn has_disc(&self) -> bool {
        self.loaded || self.pending.is_some()
    }

    /// Whether CD-DA playback is running (not paused or finished).
    pub fn audio_playing(&self) -> bool {
        self.play == PlayPhase::Playing
    }

    /// Whether the drive has emulated-time work in flight (playback or a
    /// tray load), so its board must not be treated as idle.
    pub fn needs_tick(&self) -> bool {
        self.play == PlayPhase::Playing || self.pending.is_some()
    }

    /// Open the tray: stop playback and drop the medium. The next
    /// media-access command reports NOT READY.
    pub fn eject(&mut self) {
        self.loaded = false;
        self.pending = None;
        self.play = PlayPhase::Idle;
    }

    /// Runtime disc swap: eject now and mount the new disc after the
    /// tray delay, raising the medium-change unit attention at mount.
    pub fn swap_disc(&mut self, image: CdImage, path: &Path) {
        self.eject();
        self.pending = Some(PendingDisc {
            image,
            path: path.to_path_buf(),
            tray_cck: TRAY_CCK,
        });
    }

    /// Advance emulated time: the tray-load countdown, and CD-DA
    /// playback streaming into the host mixer ring.
    pub fn tick(&mut self, cck: u32, cd_audio: &mut CdAudioRing) {
        if let Some(pending) = self.pending.as_mut() {
            pending.tray_cck -= i64::from(cck);
            if pending.tray_cck <= 0 {
                let PendingDisc { image, path, .. } = self.pending.take().unwrap();
                log::info!("scsi cd: mounted {} ({})", path.display(), image.describe());
                self.image = image;
                self.path = path;
                self.loaded = true;
                self.unit_attention = true;
            }
        }
        if self.play != PlayPhase::Playing {
            return;
        }
        self.audio_cck -= cck as i32;
        while self.audio_cck <= 0 && self.play == PlayPhase::Playing {
            self.audio_cck += CCK_PER_CD_FRAME as i32;
            self.stream_audio_sector(cd_audio);
        }
    }

    /// Produce one CD frame of audio. Data sectors inside the range are
    /// skipped silently; the range end completes the play operation.
    fn stream_audio_sector(&mut self, cd_audio: &mut CdAudioRing) {
        if self.play_pos >= self.play_end {
            self.play = PlayPhase::Done;
            self.play_pos = self.play_end.saturating_sub(1);
            return;
        }
        if !cd_audio.wants_sector() {
            // Mixer backlog: hold this frame slot; production resumes as
            // the ring drains.
            return;
        }
        let sector = self.play_pos;
        if self.image.is_audio_sector(sector) {
            let mut raw = [0u8; RAW_SECTOR_BYTES];
            if self.image.read_audio_sector(sector, &mut raw).is_ok() {
                cd_audio.push_sector(&raw);
            }
        }
        self.play_pos += 1;
    }

    /// The READ SUB-CHANNEL audio status byte for the current state.
    fn audio_status(&self) -> u8 {
        match self.play {
            PlayPhase::Idle => AUDIO_STATUS_NONE,
            PlayPhase::Playing => AUDIO_STATUS_PLAYING,
            PlayPhase::Paused => AUDIO_STATUS_PAUSED,
            PlayPhase::Done => AUDIO_STATUS_COMPLETED,
        }
    }

    /// One-line playback status for the debugger's audio tab, `None`
    /// while no play operation has state to report.
    pub fn playback_line(&self) -> Option<String> {
        let verb = match self.play {
            PlayPhase::Idle => return None,
            PlayPhase::Playing => "playing",
            PlayPhase::Paused => "paused",
            PlayPhase::Done => "done",
        };
        let track = self.track_at(self.play_pos).number;
        let msf = self.play_pos + LEADIN_SECTORS;
        Some(format!(
            "{verb} trk {track:02} {:02}:{:02}:{:02}",
            msf / (60 * SECTORS_PER_SECOND),
            (msf / SECTORS_PER_SECOND) % 60,
            msf % SECTORS_PER_SECOND,
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// One-line TOC summary for the log.
    pub fn describe(&self) -> String {
        self.image.describe()
    }

    fn set_sense(&mut self, key: u8, asc: u8) {
        self.sense = [0u8; SENSE_LEN];
        self.sense[0] = 0x70; // current error, fixed format
        self.sense[2] = key;
        self.sense[7] = 10; // additional sense length
        self.sense[12] = asc;
    }

    fn clear_sense(&mut self) {
        self.sense = [0u8; SENSE_LEN];
    }

    fn check(&mut self, key: u8, asc: u8) -> (ScsiExec, u8) {
        self.set_sense(key, asc);
        (ScsiExec::NoData, CHECK_CONDITION)
    }

    fn total_sectors(&self) -> u32 {
        self.image.total_sectors()
    }

    /// Whether an opcode touches the medium (and so needs a disc loaded).
    fn needs_medium(op: u8) -> bool {
        matches!(
            op,
            0x00 | 0x08
                | 0x0B
                | 0x25
                | 0x28
                | 0x2B
                | 0x42
                | 0x43
                | 0x44
                | 0x45
                | 0x47
                | 0x48
                | 0x4B
                | 0x4E
                | 0xA8
                | 0xBE
        )
    }

    fn inquiry_data(lun: u8) -> Vec<u8> {
        let mut d = vec![0u8; 36];
        // Peripheral qualifier 011b + type 1Fh for an unsupported LUN.
        d[0] = if lun == 0 { 0x05 } else { 0x7F };
        d[1] = 0x80; // removable medium
        d[2] = 0x02; // SCSI-2
        d[3] = 0x02; // response data format: SCSI-2
        d[4] = 31; // additional length
        d[8..16].copy_from_slice(b"COPPERLN");
        d[16..32].copy_from_slice(b"SCSI CD-ROM     ");
        d[32..36].copy_from_slice(b"1.0 ");
        d
    }

    /// A 4-byte TOC/header/sub-channel address: absolute MSF (with the
    /// 2-second lead-in offset) or a plain LBA, by the CDB's TIME bit.
    fn addr4(sector: u32, msf: bool) -> [u8; 4] {
        if msf {
            let s = sector + LEADIN_SECTORS;
            [
                0,
                (s / (60 * 75)) as u8,
                ((s / 75) % 60) as u8,
                (s % 75) as u8,
            ]
        } else {
            sector.to_be_bytes()
        }
    }

    /// The ADR/control byte a TOC entry carries: position-information ADR
    /// with the data bit for data tracks.
    fn ctl_adr(track: &CdTrack) -> u8 {
        if track.kind.is_data() {
            0x14
        } else {
            0x10
        }
    }

    /// The track containing a disc sector (falling back to the outermost
    /// edges for out-of-range positions).
    fn track_at(&self, sector: u32) -> &CdTrack {
        let tracks = self.image.tracks();
        tracks
            .iter()
            .rev()
            .find(|t| sector >= t.start_sector)
            .unwrap_or(&tracks[0])
    }

    /// Convert a CDB MSF field (3 binary bytes) to a disc sector, `None`
    /// when it lies before the start of the program area.
    fn msf_to_sector(m: u8, s: u8, f: u8) -> Option<u32> {
        let abs = (u32::from(m) * 60 + u32::from(s)) * 75 + u32::from(f);
        abs.checked_sub(LEADIN_SECTORS)
    }

    /// Parse and execute a CDB up to (but not including) any data-out
    /// payload; the counterpart of [`super::ScsiDisk::execute`].
    pub fn execute(&mut self, cdb: &[u8], lun: u8) -> (ScsiExec, u8) {
        let Some(&op) = cdb.first() else {
            return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
        };
        if cdb.len() < cdb_len(op) {
            return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
        }
        // REQUEST SENSE must report (then clear) the previous command's
        // sense data; every other command starts with it cleared.
        if op != 0x03 {
            self.clear_sense();
        }
        if lun != 0 {
            return match op {
                // INQUIRY for an unsupported LUN reports qualifier 011b.
                0x12 => {
                    let alloc = usize::from(cdb[4]);
                    let data = Self::inquiry_data(lun);
                    (ScsiExec::DataIn(data[..alloc.min(36)].to_vec()), GOOD)
                }
                _ => self.check(SK_ILLEGAL_REQUEST, ASC_LUN_NOT_SUPPORTED),
            };
        }
        // A pending unit attention (the medium changed) preempts every
        // command except INQUIRY and REQUEST SENSE.
        if self.unit_attention && !matches!(op, 0x03 | 0x12) {
            self.unit_attention = false;
            return self.check(SK_UNIT_ATTENTION, ASC_MEDIUM_MAY_HAVE_CHANGED);
        }
        if !self.loaded && Self::needs_medium(op) {
            return self.check(SK_NOT_READY, ASC_MEDIUM_NOT_PRESENT);
        }
        match op {
            // TEST UNIT READY (the medium gate above did the work)
            0x00 => (ScsiExec::NoData, GOOD),
            // REQUEST SENSE
            0x03 => {
                let alloc = match cdb[4] {
                    0 => 4, // SCSI-1: zero means four bytes
                    n => usize::from(n),
                };
                let data = self.sense[..alloc.min(SENSE_LEN)].to_vec();
                self.clear_sense();
                (ScsiExec::DataIn(data), GOOD)
            }
            // READ(6)
            0x08 => {
                let lba = u64::from(be24(cdb, 1) & 0x1F_FFFF);
                let count = match cdb[4] {
                    0 => 256u64,
                    n => u64::from(n),
                };
                self.read_data(lba, count)
            }
            // SEEK(6) / SEEK(10)
            0x0B | 0x2B => (ScsiExec::NoData, GOOD),
            // INQUIRY
            0x12 => {
                if cdb[1] & 0x01 != 0 {
                    // EVPD: only the supported-pages page.
                    if cdb[2] == 0x00 {
                        let data = [0u8, 0, 0, 1, 0];
                        let alloc = usize::from(cdb[4]);
                        return (ScsiExec::DataIn(data[..alloc.min(5)].to_vec()), GOOD);
                    }
                    return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                }
                let alloc = usize::from(cdb[4]);
                let data = Self::inquiry_data(0);
                (ScsiExec::DataIn(data[..alloc.min(36)].to_vec()), GOOD)
            }
            // MODE SELECT(6)/(10): accept and ignore the parameter list
            // (the only block size served is 2048).
            0x15 => (ScsiExec::DataOut(usize::from(cdb[4])), GOOD),
            0x55 => (ScsiExec::DataOut(be16(cdb, 7) as usize), GOOD),
            // RESERVE / RELEASE
            0x16 | 0x17 => (ScsiExec::NoData, GOOD),
            // MODE SENSE(6)/(10)
            0x1A => self.mode_sense(cdb, false),
            0x5A => self.mode_sense(cdb, true),
            // START STOP UNIT: with LoEj the tray ejects/reloads the disc;
            // stopping the spindle (either way) ends audio playback.
            0x1B => {
                let load_eject = cdb[4] & 0x02 != 0;
                let start = cdb[4] & 0x01 != 0;
                if !start {
                    self.play = PlayPhase::Idle;
                }
                if load_eject {
                    if start {
                        // Reload the tray (a disc travelling after a swap
                        // keeps its own mount countdown).
                        if !self.loaded && self.pending.is_none() {
                            self.loaded = true;
                            self.unit_attention = true;
                        }
                    } else {
                        self.eject();
                    }
                }
                (ScsiExec::NoData, GOOD)
            }
            // PREVENT ALLOW MEDIUM REMOVAL
            0x1E => (ScsiExec::NoData, GOOD),
            // READ CAPACITY(10)
            0x25 => {
                let last = self.total_sectors().saturating_sub(1);
                let mut data = Vec::with_capacity(8);
                data.extend_from_slice(&last.to_be_bytes());
                data.extend_from_slice(&(DATA_SECTOR_BYTES as u32).to_be_bytes());
                (ScsiExec::DataIn(data), GOOD)
            }
            // READ(10)
            0x28 => {
                let lba = u64::from(be32(cdb, 2));
                let count = u64::from(be16(cdb, 7));
                self.read_data(lba, count)
            }
            // READ SUB-CHANNEL
            0x42 => self.read_sub_channel(cdb),
            // READ TOC
            0x43 => self.read_toc(cdb),
            // READ HEADER
            0x44 => {
                let msf = cdb[1] & 0x02 != 0;
                let lba = be32(cdb, 2);
                if lba >= self.total_sectors() {
                    return self.check(SK_ILLEGAL_REQUEST, ASC_LBA_OUT_OF_RANGE);
                }
                let alloc = be16(cdb, 7) as usize;
                let mut data = vec![0u8; 8];
                data[0] = if self.image.is_audio_sector(lba) {
                    0
                } else {
                    1
                };
                data[4..8].copy_from_slice(&Self::addr4(lba, msf));
                data.truncate(alloc);
                (ScsiExec::DataIn(data), GOOD)
            }
            // PLAY AUDIO(10)
            0x45 => {
                let lba = be32(cdb, 2);
                let count = be16(cdb, 7);
                if count == 0 {
                    return (ScsiExec::NoData, GOOD);
                }
                self.play_range(lba, lba + count)
            }
            // PLAY AUDIO MSF
            0x47 => {
                let start = match cdb[3] {
                    // FFh: continue from the current position.
                    0xFF => Some(self.play_pos),
                    _ => Self::msf_to_sector(cdb[3], cdb[4], cdb[5]),
                };
                let end = Self::msf_to_sector(cdb[6], cdb[7], cdb[8]);
                let (Some(start), Some(end)) = (start, end) else {
                    return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                };
                if end < start {
                    return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                }
                if end == start {
                    return (ScsiExec::NoData, GOOD);
                }
                self.play_range(start, end)
            }
            // PLAY AUDIO TRACK/INDEX
            0x48 => {
                let tracks = self.image.tracks();
                let start = tracks.iter().find(|t| t.number == cdb[4]);
                let end = tracks.iter().find(|t| t.number == cdb[7]);
                let (Some(start), Some(end)) = (start, end) else {
                    return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                };
                let (start, end) = (start.start_sector, end.start_sector + end.sector_count);
                self.play_range(start, end)
            }
            // PAUSE / RESUME
            0x4B => {
                let resume = cdb[8] & 0x01 != 0;
                match (resume, self.play) {
                    (true, PlayPhase::Paused) => self.play = PlayPhase::Playing,
                    (false, PlayPhase::Playing) => self.play = PlayPhase::Paused,
                    _ => {}
                }
                (ScsiExec::NoData, GOOD)
            }
            // STOP PLAY/SCAN
            0x4E => {
                self.play = PlayPhase::Idle;
                (ScsiExec::NoData, GOOD)
            }
            // READ(12)
            0xA8 => {
                let lba = u64::from(be32(cdb, 2));
                let count = u64::from(be32(cdb, 6));
                self.read_data(lba, count)
            }
            // READ CD
            0xBE => self.read_cd(cdb),
            _ => {
                log::debug!("scsi cd: unsupported opcode {op:#04X}");
                self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_OPCODE)
            }
        }
    }

    /// Complete a data-out command once the payload has arrived. The only
    /// data-out commands the CD-ROM accepts are MODE SELECT parameter
    /// lists, which are accepted and ignored.
    pub fn complete_out(&mut self, _cdb: &[u8], _data: &[u8]) -> u8 {
        GOOD
    }

    /// The READ family: user data in 2048-byte blocks. Audio tracks cannot
    /// be read this way.
    fn read_data(&mut self, lba: u64, count: u64) -> (ScsiExec, u8) {
        let total = u64::from(self.total_sectors());
        if lba + count > total {
            return self.check(SK_ILLEGAL_REQUEST, ASC_LBA_OUT_OF_RANGE);
        }
        if count == 0 {
            return (ScsiExec::NoData, GOOD);
        }
        let mut data = vec![0u8; (count as usize) * DATA_SECTOR_BYTES];
        for i in 0..count {
            let sector = (lba + i) as u32;
            if self.image.is_audio_sector(sector) {
                return self.check(SK_ILLEGAL_REQUEST, ASC_ILLEGAL_MODE_FOR_THIS_TRACK);
            }
            let mut buf = [0u8; DATA_SECTOR_BYTES];
            if let Err(e) = self.image.read_data_sector(sector, &mut buf) {
                log::warn!("scsi cd {}: read lba {sector}: {e}", self.path.display());
                return self.check(SK_HARDWARE_ERROR, 0x00);
            }
            data[(i as usize) * DATA_SECTOR_BYTES..][..DATA_SECTOR_BYTES].copy_from_slice(&buf);
        }
        (ScsiExec::DataIn(data), GOOD)
    }

    /// READ CD: raw or cooked frames by the main-channel byte field.
    fn read_cd(&mut self, cdb: &[u8]) -> (ScsiExec, u8) {
        let expected = (cdb[1] >> 2) & 0x07;
        let lba = u64::from(be32(cdb, 2));
        let count = u64::from(be24(cdb, 6));
        let main = cdb[9];
        if cdb[10] & 0x07 != 0 {
            // No sub-channel data to interleave.
            return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
        }
        let total = u64::from(self.total_sectors());
        if lba + count > total {
            return self.check(SK_ILLEGAL_REQUEST, ASC_LBA_OUT_OF_RANGE);
        }
        if count == 0 || main == 0 {
            return (ScsiExec::NoData, GOOD);
        }
        // Main-channel selection: full raw frames or user data only.
        let raw = match main {
            0xF8 => true,
            0x10 => false,
            _ => return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB),
        };
        let sector_bytes = if raw {
            RAW_SECTOR_BYTES
        } else {
            DATA_SECTOR_BYTES
        };
        let mut data = vec![0u8; (count as usize) * sector_bytes];
        for i in 0..count {
            let sector = (lba + i) as u32;
            let audio = self.image.is_audio_sector(sector);
            // Expected sector type: 1 = CD-DA, 2 = mode 1; 0 accepts any.
            let type_ok = match expected {
                1 => audio,
                2 => !audio,
                _ => true,
            };
            if !type_ok || (!raw && audio) {
                return self.check(SK_ILLEGAL_REQUEST, ASC_ILLEGAL_MODE_FOR_THIS_TRACK);
            }
            let out = &mut data[(i as usize) * sector_bytes..][..sector_bytes];
            let result = if raw {
                let mut buf = [0u8; RAW_SECTOR_BYTES];
                let r = self.image.read_raw_sector(sector, &mut buf);
                out.copy_from_slice(&buf);
                r
            } else {
                let mut buf = [0u8; DATA_SECTOR_BYTES];
                let r = self.image.read_data_sector(sector, &mut buf);
                out.copy_from_slice(&buf);
                r
            };
            if let Err(e) = result {
                log::warn!("scsi cd {}: read cd lba {sector}: {e}", self.path.display());
                return self.check(SK_HARDWARE_ERROR, 0x00);
            }
        }
        (ScsiExec::DataIn(data), GOOD)
    }

    /// Start playing an audio range: the drive streams it into the host
    /// mixer at 75 sectors per second of emulated time from `tick`.
    fn play_range(&mut self, start: u32, end: u32) -> (ScsiExec, u8) {
        if start >= self.total_sectors() || end > self.total_sectors() {
            return self.check(SK_ILLEGAL_REQUEST, ASC_LBA_OUT_OF_RANGE);
        }
        if !self.image.is_audio_sector(start) {
            return self.check(SK_ILLEGAL_REQUEST, ASC_ILLEGAL_MODE_FOR_THIS_TRACK);
        }
        self.play = PlayPhase::Playing;
        self.play_pos = start;
        self.play_end = end;
        self.audio_cck = 0;
        (ScsiExec::NoData, GOOD)
    }

    fn mode_sense(&mut self, cdb: &[u8], ten: bool) -> (ScsiExec, u8) {
        let dbd = cdb[1] & 0x08 != 0;
        let page = cdb[2] & 0x3F;
        let Some(pages) = self.mode_pages(page) else {
            return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
        };
        let mut body = Vec::new();
        if !dbd {
            // Block descriptor: density 0, all blocks, 2048-byte blocks.
            let blocks = if self.loaded { self.total_sectors() } else { 0 };
            body.push(0);
            body.extend_from_slice(&blocks.min(0x00FF_FFFF).to_be_bytes()[1..]);
            body.push(0);
            body.extend_from_slice(&(DATA_SECTOR_BYTES as u32).to_be_bytes()[1..]);
        }
        let bd_len = body.len() as u8;
        body.extend_from_slice(&pages);
        let mut data = Vec::new();
        // The device-specific parameter carries the write-protect bit: the
        // medium is read-only.
        if ten {
            let len = (body.len() + 6) as u32;
            data.extend_from_slice(&[(len >> 8) as u8, len as u8, 0, 0x80, 0, 0, 0, bd_len]);
        } else {
            data.extend_from_slice(&[(body.len() + 3) as u8, 0, 0x80, bd_len]);
        }
        data.extend_from_slice(&body);
        let alloc = if ten {
            be16(cdb, 7) as usize
        } else {
            usize::from(cdb[4])
        };
        data.truncate(alloc);
        (ScsiExec::DataIn(data), GOOD)
    }

    fn mode_pages(&self, page: u8) -> Option<Vec<u8>> {
        // Read error recovery: retries at the drive's discretion.
        let page1 = || vec![0x01u8, 6, 0, 0, 0, 0, 0, 0];
        // CD-ROM parameters: 60 seconds per minute, 75 frames per second.
        let page13 = || vec![0x0Du8, 6, 0, 0, 0, 60, 0, 75];
        // CD audio control: immediate play, ports 0/1 to channels L/R at
        // full volume.
        let page14 = || {
            let mut p = vec![0u8; 16];
            p[0] = 0x0E;
            p[1] = 14;
            p[2] = 0x04; // IMMED
            p[8] = 0x01; // port 0: channel 1
            p[9] = 0xFF;
            p[10] = 0x02; // port 1: channel 2
            p[11] = 0xFF;
            p
        };
        // Capabilities and mechanical status: audio play and CD-DA reads,
        // tray loader with eject and lock, 4x speed, 256 volume levels,
        // a 64K buffer.
        let page2a = || {
            let mut p = vec![0u8; 22];
            p[0] = 0x2A;
            p[1] = 20;
            p[4] = 0x01; // audio play
            p[5] = 0x01; // CD-DA commands
            p[6] = 0x29; // tray loader, eject, lock
            p[8..10].copy_from_slice(&706u16.to_be_bytes()); // max KB/s
            p[10..12].copy_from_slice(&256u16.to_be_bytes());
            p[12..14].copy_from_slice(&64u16.to_be_bytes());
            p[14..16].copy_from_slice(&706u16.to_be_bytes()); // current KB/s
            p
        };
        match page {
            0x01 => Some(page1()),
            0x0D => Some(page13()),
            0x0E => Some(page14()),
            0x2A => Some(page2a()),
            0x3F => {
                let mut all = page1();
                all.extend_from_slice(&page13());
                all.extend_from_slice(&page14());
                all.extend_from_slice(&page2a());
                Some(all)
            }
            _ => None,
        }
    }

    fn read_toc(&mut self, cdb: &[u8]) -> (ScsiExec, u8) {
        let msf = cdb[1] & 0x02 != 0;
        let format = match cdb[2] & 0x0F {
            // SCSI-2 drives carried the format in the control byte's top
            // bits before byte 2 was defined; honour either encoding.
            0 => cdb[9] >> 6,
            f => f,
        };
        let alloc = be16(cdb, 7) as usize;
        let tracks = self.image.tracks();
        let first = tracks.first().map_or(1, |t| t.number);
        let last = tracks.last().map_or(1, |t| t.number);
        let mut data = match format {
            // Format 0: the TOC proper, from the starting track to the
            // lead-out.
            0 => {
                let start = cdb[6];
                if start > last && start != 0xAA {
                    return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                }
                let mut d = vec![0, 0, first, last];
                for track in tracks.iter().filter(|t| t.number >= start) {
                    d.extend_from_slice(&[0, Self::ctl_adr(track), track.number, 0]);
                    d.extend_from_slice(&Self::addr4(track.start_sector, msf));
                }
                // The lead-out, as track AAh.
                d.extend_from_slice(&[0, 0x14, 0xAA, 0]);
                d.extend_from_slice(&Self::addr4(self.total_sectors(), msf));
                let len = (d.len() - 2) as u16;
                d[0..2].copy_from_slice(&len.to_be_bytes());
                d
            }
            // Format 1: session info. Images are single-session, so the
            // first track doubles as the last session's first track.
            1 => {
                let mut d = vec![0, 0x0A, 1, 1, 0, 0, first, 0];
                let (ctl, sector) = tracks
                    .first()
                    .map_or((0x14, 0), |t| (Self::ctl_adr(t), t.start_sector));
                d[5] = ctl;
                d.extend_from_slice(&Self::addr4(sector, msf));
                d
            }
            _ => return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB),
        };
        data.truncate(alloc);
        (ScsiExec::DataIn(data), GOOD)
    }

    fn read_sub_channel(&mut self, cdb: &[u8]) -> (ScsiExec, u8) {
        let msf = cdb[1] & 0x02 != 0;
        let want_subq = cdb[2] & 0x40 != 0;
        let format = cdb[3];
        let alloc = be16(cdb, 7) as usize;
        let mut data = vec![0u8, self.audio_status(), 0, 0];
        if want_subq {
            match format {
                // Current position: the live playback cursor.
                0x01 => {
                    let track = self.track_at(self.play_pos);
                    let (ctl, number, start) =
                        (Self::ctl_adr(track), track.number, track.start_sector);
                    data.extend_from_slice(&[0x01, ctl, number, 1]);
                    data.extend_from_slice(&Self::addr4(self.play_pos, msf));
                    data.extend_from_slice(&Self::addr4(self.play_pos.saturating_sub(start), msf));
                }
                // Media catalogue number / ISRC: nothing encoded (the
                // MCVal/TCVal bit stays clear).
                0x02 | 0x03 => {
                    data.push(format);
                    data.extend_from_slice(&[0u8; 19]);
                }
                _ => return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB),
            }
            let len = (data.len() - 4) as u16;
            data[2..4].copy_from_slice(&len.to_be_bytes());
        }
        data.truncate(alloc);
        (ScsiExec::DataIn(data), GOOD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scsi::{CHECK_CONDITION, GOOD};
    use std::io::Write;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "copperline-scsicd-{}-{unique}-{name}",
            std::process::id()
        ))
    }

    /// A 6-sector disc: 4 data sectors (filled with the sector number)
    /// then a 2-sector audio track.
    fn mixed_disc() -> (ScsiCdRom, Vec<PathBuf>) {
        let cue = temp_path("mixed.cue");
        let bin = temp_path("mixed.bin");
        let mut bytes = Vec::new();
        for s in 0..4u8 {
            bytes.extend(std::iter::repeat_n(s, DATA_SECTOR_BYTES));
        }
        for s in 0..2u8 {
            bytes.extend(std::iter::repeat_n(0xA0 + s, RAW_SECTOR_BYTES));
        }
        let mut f = std::fs::File::create(&bin).unwrap();
        f.write_all(&bytes).unwrap();
        std::fs::write(
            &cue,
            format!(
                "FILE \"{}\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 00:00:04\n",
                bin.file_name().unwrap().to_string_lossy()
            ),
        )
        .unwrap();
        (ScsiCdRom::open(&cue).unwrap(), vec![cue, bin])
    }

    fn cleanup(paths: &[PathBuf]) {
        for p in paths {
            let _ = std::fs::remove_file(p);
        }
    }

    fn data_in(cd: &mut ScsiCdRom, cdb: &[u8]) -> Vec<u8> {
        let (exec, status) = cd.execute(cdb, 0);
        assert_eq!(status, GOOD, "status for {:#04X}", cdb[0]);
        match exec {
            ScsiExec::DataIn(d) => d,
            _ => panic!("expected data-in for {:#04X}", cdb[0]),
        }
    }

    fn check_sense(cd: &mut ScsiCdRom, cdb: &[u8], key: u8, asc: u8) {
        let (_, status) = cd.execute(cdb, 0);
        assert_eq!(status, CHECK_CONDITION, "status for {:#04X}", cdb[0]);
        let sense = data_in(cd, &[0x03, 0, 0, 0, 18, 0]);
        assert_eq!(sense[2] & 0x0F, key, "sense key for {:#04X}", cdb[0]);
        assert_eq!(sense[12], asc, "asc for {:#04X}", cdb[0]);
    }

    #[test]
    fn inquiry_identifies_a_removable_cdrom() {
        let (mut cd, paths) = mixed_disc();
        let data = data_in(&mut cd, &[0x12, 0, 0, 0, 36, 0]);
        assert_eq!(data[0], 0x05); // CD-ROM device
        assert_eq!(data[1], 0x80); // removable
        assert_eq!(&data[8..16], b"COPPERLN");
        assert_eq!(&data[16..27], b"SCSI CD-ROM");
        // An unsupported LUN reports qualifier 011b.
        let (exec, status) = cd.execute(&[0x12, 0x20, 0, 0, 36, 0], 1);
        assert_eq!(status, GOOD);
        let ScsiExec::DataIn(data) = exec else {
            panic!("expected data-in");
        };
        assert_eq!(data[0], 0x7F);
        cleanup(&paths);
    }

    #[test]
    fn read_capacity_reports_2048_byte_blocks() {
        let (mut cd, paths) = mixed_disc();
        let data = data_in(&mut cd, &[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(u32::from_be_bytes(data[0..4].try_into().unwrap()), 5);
        assert_eq!(u32::from_be_bytes(data[4..8].try_into().unwrap()), 2048);
        cleanup(&paths);
    }

    #[test]
    fn read10_returns_user_data_and_rejects_audio_tracks() {
        let (mut cd, paths) = mixed_disc();
        let data = data_in(&mut cd, &[0x28, 0, 0, 0, 0, 2, 0, 0, 2, 0]);
        assert_eq!(data.len(), 2 * DATA_SECTOR_BYTES);
        assert!(data[..DATA_SECTOR_BYTES].iter().all(|&b| b == 2));
        assert!(data[DATA_SECTOR_BYTES..].iter().all(|&b| b == 3));
        // Reading into the audio track is an illegal mode for that track.
        check_sense(
            &mut cd,
            &[0x28, 0, 0, 0, 0, 4, 0, 0, 1, 0],
            SK_ILLEGAL_REQUEST,
            ASC_ILLEGAL_MODE_FOR_THIS_TRACK,
        );
        // Reading past the end of the disc is out of range.
        check_sense(
            &mut cd,
            &[0x28, 0, 0, 0, 0, 6, 0, 0, 1, 0],
            SK_ILLEGAL_REQUEST,
            ASC_LBA_OUT_OF_RANGE,
        );
        cleanup(&paths);
    }

    #[test]
    fn read12_matches_read10() {
        let (mut cd, paths) = mixed_disc();
        let ten = data_in(&mut cd, &[0x28, 0, 0, 0, 0, 1, 0, 0, 1, 0]);
        let twelve = data_in(&mut cd, &[0xA8, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0]);
        assert_eq!(ten, twelve);
        cleanup(&paths);
    }

    #[test]
    fn read_toc_lists_tracks_and_lead_out() {
        let (mut cd, paths) = mixed_disc();
        let data = data_in(&mut cd, &[0x43, 0, 0, 0, 0, 0, 0, 1, 0, 0]);
        assert_eq!(data[2], 1); // first track
        assert_eq!(data[3], 2); // last track
        assert_eq!(be16(&data, 0) as usize, data.len() - 2);
        // Track 1: data (control 0x14), LBA 0.
        assert_eq!(data[5], 0x14);
        assert_eq!(data[6], 1);
        assert_eq!(be32(&data, 8), 0);
        // Track 2: audio (control 0x10), LBA 4.
        assert_eq!(data[13], 0x10);
        assert_eq!(data[14], 2);
        assert_eq!(be32(&data, 16), 4);
        // Lead-out at the disc's end.
        assert_eq!(data[22], 0xAA);
        assert_eq!(be32(&data, 24), 6);

        // MSF form: track 1 starts at 00:02:00 (the 150-sector lead-in).
        let data = data_in(&mut cd, &[0x43, 0x02, 0, 0, 0, 0, 0, 1, 0, 0]);
        assert_eq!(&data[8..12], &[0, 0, 2, 0]);
        cleanup(&paths);
    }

    #[test]
    fn read_toc_session_info_names_the_single_session() {
        let (mut cd, paths) = mixed_disc();
        let data = data_in(&mut cd, &[0x43, 0, 1, 0, 0, 0, 0, 0, 12, 0]);
        assert_eq!(data[2], 1); // first session
        assert_eq!(data[3], 1); // last session
        assert_eq!(data[6], 1); // first track in it
        assert_eq!(be32(&data, 8), 0);
        cleanup(&paths);
    }

    #[test]
    fn mode_sense_carries_cd_pages_and_write_protect() {
        let (mut cd, paths) = mixed_disc();
        let data = data_in(&mut cd, &[0x1A, 0, 0x3F, 0, 254, 0]);
        assert_eq!(data[0] as usize, data.len() - 1);
        assert_eq!(data[2], 0x80); // write-protected medium
        assert_eq!(data[3], 8); // block descriptor present
        assert_eq!(be24(&data, 5), 6); // blocks
        assert_eq!(be24(&data, 9), 2048); // block size
                                          // Pages 01, 0D, 0E, 2A in ascending order after the descriptor.
        let mut off = 12;
        let mut seen = Vec::new();
        while off + 1 < data.len() {
            seen.push(data[off]);
            off += 2 + usize::from(data[off + 1]);
        }
        assert_eq!(seen, vec![0x01, 0x0D, 0x0E, 0x2A]);

        // MODE SENSE(10) returns the same pages behind the 8-byte header.
        let ten = data_in(&mut cd, &[0x5A, 0, 0x2A, 0, 0, 0, 0, 0, 254, 0]);
        assert_eq!(be16(&ten, 0) as usize, ten.len() - 2);
        assert_eq!(ten[3], 0x80);
        assert_eq!(ten[16], 0x2A);
        cleanup(&paths);
    }

    #[test]
    fn eject_reload_cycle_reports_not_ready_then_unit_attention() {
        let (mut cd, paths) = mixed_disc();
        // Eject the disc: media-access commands report NOT READY.
        let (_, status) = cd.execute(&[0x1B, 0, 0, 0, 0x02, 0], 0);
        assert_eq!(status, GOOD);
        check_sense(
            &mut cd,
            &[0x00, 0, 0, 0, 0, 0],
            SK_NOT_READY,
            ASC_MEDIUM_NOT_PRESENT,
        );
        // INQUIRY still answers while the tray is open.
        let data = data_in(&mut cd, &[0x12, 0, 0, 0, 36, 0]);
        assert_eq!(data[0], 0x05);
        // Reload: the first command reports the medium change once...
        let (_, status) = cd.execute(&[0x1B, 0, 0, 0, 0x03, 0], 0);
        assert_eq!(status, GOOD);
        check_sense(
            &mut cd,
            &[0x00, 0, 0, 0, 0, 0],
            SK_UNIT_ATTENTION,
            ASC_MEDIUM_MAY_HAVE_CHANGED,
        );
        // ...and then the unit is ready again.
        let (_, status) = cd.execute(&[0x00, 0, 0, 0, 0, 0], 0);
        assert_eq!(status, GOOD);
        cleanup(&paths);
    }

    #[test]
    fn read_cd_serves_raw_frames_with_synthesized_headers() {
        let (mut cd, paths) = mixed_disc();
        // Full raw frame of a cooked data sector: sync + BCD MSF header.
        let data = data_in(&mut cd, &[0xBE, 0, 0, 0, 0, 0, 0, 0, 1, 0xF8, 0, 0]);
        assert_eq!(data.len(), RAW_SECTOR_BYTES);
        assert_eq!(data[0], 0);
        assert!(data[1..11].iter().all(|&b| b == 0xFF));
        assert_eq!(&data[12..16], &[0, 2, 0, 1]); // 00:02:00, mode 1
        assert!(data[16..16 + DATA_SECTOR_BYTES].iter().all(|&b| b == 0));
        // User-data-only read of an audio sector is an illegal mode.
        check_sense(
            &mut cd,
            &[0xBE, 0, 0, 0, 0, 4, 0, 0, 1, 0x10, 0, 0],
            SK_ILLEGAL_REQUEST,
            ASC_ILLEGAL_MODE_FOR_THIS_TRACK,
        );
        // Raw read of the audio sector returns the CD-DA frame verbatim.
        let data = data_in(&mut cd, &[0xBE, 0x04, 0, 0, 0, 4, 0, 0, 1, 0xF8, 0, 0]);
        assert!(data.iter().all(|&b| b == 0xA0));
        cleanup(&paths);
    }

    #[test]
    fn play_audio_streams_sectors_into_the_ring_and_completes() {
        let (mut cd, paths) = mixed_disc();
        let mut ring = CdAudioRing::default();
        // Nothing played yet: no audio status.
        let data = data_in(&mut cd, &[0x42, 0, 0x40, 1, 0, 0, 0, 0, 16, 0]);
        assert_eq!(data[1], AUDIO_STATUS_NONE);
        // Play the audio track (sectors 4-6): playback starts and runs on
        // emulated time.
        let (_, status) = cd.execute(&[0x45, 0, 0, 0, 0, 4, 0, 0, 2, 0], 0);
        assert_eq!(status, GOOD);
        let data = data_in(&mut cd, &[0x42, 0, 0x40, 1, 0, 0, 0, 0, 16, 0]);
        assert_eq!(data[1], AUDIO_STATUS_PLAYING);
        assert_eq!(be32(&data, 8), 4); // playback cursor at the range start

        // The first CD frame is due immediately; the second after 1/75 s.
        cd.tick(1, &mut ring);
        // Sector 4 is all 0xA0 bytes: each s16le sample is 0xA0A0 = -24416.
        assert_eq!(ring.next_sample(), (-24416.0 / 32768.0, -24416.0 / 32768.0));
        cd.tick(CCK_PER_CD_FRAME, &mut ring);
        let data = data_in(&mut cd, &[0x42, 0, 0x40, 1, 0, 0, 0, 0, 16, 0]);
        assert_eq!(data[1], AUDIO_STATUS_PLAYING);
        assert_eq!(data[6], 2); // track 2
        assert_eq!(be32(&data, 8), 6); // both sectors produced

        // The frame after the range end completes the play operation.
        cd.tick(CCK_PER_CD_FRAME, &mut ring);
        let data = data_in(&mut cd, &[0x42, 0, 0x40, 1, 0, 0, 0, 0, 16, 0]);
        assert_eq!(data[1], AUDIO_STATUS_COMPLETED);
        assert_eq!(be32(&data, 8), 5); // last played sector
        assert_eq!(be32(&data, 12), 1); // relative to the track start

        // Playing a data track is an illegal mode.
        check_sense(
            &mut cd,
            &[0x45, 0, 0, 0, 0, 0, 0, 0, 2, 0],
            SK_ILLEGAL_REQUEST,
            ASC_ILLEGAL_MODE_FOR_THIS_TRACK,
        );
        cleanup(&paths);
    }

    #[test]
    fn pause_resume_and_stop_steer_playback() {
        let (mut cd, paths) = mixed_disc();
        let mut ring = CdAudioRing::default();
        let (_, status) = cd.execute(&[0x45, 0, 0, 0, 0, 4, 0, 0, 2, 0], 0);
        assert_eq!(status, GOOD);
        // Pause: no sectors are produced while paused.
        let (_, status) = cd.execute(&[0x4B, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0);
        assert_eq!(status, GOOD);
        cd.tick(4 * CCK_PER_CD_FRAME, &mut ring);
        assert_eq!(ring.next_sample(), (0.0, 0.0));
        let data = data_in(&mut cd, &[0x42, 0, 0x40, 1, 0, 0, 0, 0, 16, 0]);
        assert_eq!(data[1], AUDIO_STATUS_PAUSED);
        // Resume plays on; stop clears the operation.
        let (_, status) = cd.execute(&[0x4B, 0, 0, 0, 0, 0, 0, 0, 1, 0], 0);
        assert_eq!(status, GOOD);
        assert!(cd.audio_playing());
        let (_, status) = cd.execute(&[0x4E, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0);
        assert_eq!(status, GOOD);
        let data = data_in(&mut cd, &[0x42, 0, 0x40, 1, 0, 0, 0, 0, 16, 0]);
        assert_eq!(data[1], AUDIO_STATUS_NONE);
        assert!(cd.playback_line().is_none());
        cleanup(&paths);
    }

    #[test]
    fn play_audio_msf_honours_lead_in_offset() {
        let (mut cd, paths) = mixed_disc();
        // 00:02:04 - 00:02:06 = sectors 4-6.
        let (_, status) = cd.execute(&[0x47, 0, 0, 0, 2, 4, 0, 2, 6, 0], 0);
        assert_eq!(status, GOOD);
        assert!(cd.audio_playing());
        assert_eq!(
            cd.playback_line().as_deref(),
            Some("playing trk 02 00:02:04")
        );
        cleanup(&paths);
    }

    #[test]
    fn disc_swap_travels_through_the_tray_with_unit_attention() {
        let (mut cd, paths) = mixed_disc();
        let mut ring = CdAudioRing::default();
        // Build a second disc (a bare ISO) to swap in.
        let iso = temp_path("swap.iso");
        std::fs::write(&iso, vec![0x5Au8; 2 * DATA_SECTOR_BYTES]).unwrap();
        let image = CdImage::load(&iso).unwrap();

        cd.swap_disc(image, &iso);
        assert!(cd.has_disc());
        // While the tray travels the medium is absent.
        check_sense(
            &mut cd,
            &[0x00, 0, 0, 0, 0, 0],
            SK_NOT_READY,
            ASC_MEDIUM_NOT_PRESENT,
        );
        // The mount lands after the tray delay and latches the medium
        // change; the first command reports it once.
        cd.tick(TRAY_CCK as u32 + 1, &mut ring);
        check_sense(
            &mut cd,
            &[0x00, 0, 0, 0, 0, 0],
            SK_UNIT_ATTENTION,
            ASC_MEDIUM_MAY_HAVE_CHANGED,
        );
        // The new disc is now served: 2 sectors of 0x5A.
        let data = data_in(&mut cd, &[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(u32::from_be_bytes(data[0..4].try_into().unwrap()), 1);
        let data = data_in(&mut cd, &[0x28, 0, 0, 0, 0, 0, 0, 0, 1, 0]);
        assert!(data.iter().all(|&b| b == 0x5A));
        let _ = std::fs::remove_file(&iso);
        cleanup(&paths);
    }

    #[test]
    fn ring_backlog_stalls_production_without_losing_sectors() {
        let (mut cd, paths) = mixed_disc();
        let mut ring = CdAudioRing::default();
        // Fill the ring to capacity so the drive cannot push.
        let sector = [0u8; RAW_SECTOR_BYTES];
        while ring.wants_sector() {
            ring.push_sector(&sector);
        }
        let (_, status) = cd.execute(&[0x45, 0, 0, 0, 0, 4, 0, 0, 2, 0], 0);
        assert_eq!(status, GOOD);
        cd.tick(10 * CCK_PER_CD_FRAME, &mut ring);
        // Still playing at the range start: nothing was dropped.
        let data = data_in(&mut cd, &[0x42, 0, 0x40, 1, 0, 0, 0, 0, 16, 0]);
        assert_eq!(data[1], AUDIO_STATUS_PLAYING);
        assert_eq!(be32(&data, 8), 4);
        // Drain one sector: production resumes.
        for _ in 0..588 {
            ring.next_sample();
        }
        cd.tick(CCK_PER_CD_FRAME, &mut ring);
        let data = data_in(&mut cd, &[0x42, 0, 0x40, 1, 0, 0, 0, 0, 16, 0]);
        assert_eq!(be32(&data, 8), 5);
        cleanup(&paths);
    }

    #[test]
    fn read_header_names_the_sector_mode() {
        let (mut cd, paths) = mixed_disc();
        let data = data_in(&mut cd, &[0x44, 0, 0, 0, 0, 0, 0, 0, 8, 0]);
        assert_eq!(data[0], 1); // data sector
        assert_eq!(be32(&data, 4), 0);
        let data = data_in(&mut cd, &[0x44, 0, 0, 0, 0, 4, 0, 0, 8, 0]);
        assert_eq!(data[0], 0); // audio sector
        cleanup(&paths);
    }

    #[test]
    fn write_commands_are_rejected_as_invalid_opcodes() {
        let (mut cd, paths) = mixed_disc();
        check_sense(
            &mut cd,
            &[0x2A, 0, 0, 0, 0, 0, 0, 0, 1, 0],
            SK_ILLEGAL_REQUEST,
            ASC_INVALID_OPCODE,
        );
        cleanup(&paths);
    }

    #[test]
    fn cdrom_target_answers_through_the_wd33c93() {
        use crate::scsi::{
            ASR_DBR, ASR_INT, WD_CDB_1, WD_COMMAND, WD_CONTROL, WD_DATA, WD_DESTINATION_ID,
            WD_SCSI_STATUS, WD_TARGET_LUN, WD_TC_LSB, WD_TC_MID, WD_TC_MSB,
        };
        let (cd, paths) = mixed_disc();
        let mut wd = crate::scsi::Wd33c93::new();
        wd.attach_target(2, cd);
        assert!(wd.target_present(2));

        let wr = |wd: &mut crate::scsi::Wd33c93, reg: u8, val: u8| {
            wd.write_sasr(reg);
            wd.write_data_port(val);
        };
        wr(&mut wd, WD_CONTROL, 0x00); // PIO
        wr(&mut wd, WD_DESTINATION_ID, 2);
        wr(&mut wd, WD_TARGET_LUN, 0);
        wd.write_sasr(WD_CDB_1);
        for b in [0x12u8, 0, 0, 0, 36, 0] {
            wd.write_data_port(b);
        }
        wr(&mut wd, WD_TC_MSB, 0);
        wr(&mut wd, WD_TC_MID, 0);
        wr(&mut wd, WD_TC_LSB, 36);
        wr(&mut wd, WD_COMMAND, 0x08); // Select-with-ATN-and-Transfer
        wd.write_sasr(WD_DATA);
        let mut data = Vec::new();
        while wd.read_aux_status() & ASR_DBR != 0 {
            data.push(wd.read_data_port());
        }
        for _ in 0..10_000 {
            wd.tick(16);
            if wd.read_aux_status() & ASR_INT != 0 {
                break;
            }
        }
        wd.write_sasr(WD_SCSI_STATUS);
        let csr = wd.read_data_port();
        assert_eq!(csr, crate::scsi::CSR_SEL_XFER_DONE);
        assert_eq!(data[0], 0x05); // a CD-ROM answered
        cleanup(&paths);
    }
}
