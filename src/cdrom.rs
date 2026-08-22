// SPDX-License-Identifier: GPL-3.0-or-later

//! CD image backend: cue/bin parsing and sector access.
//!
//! Supports single-file and multi-file cue layouts with MODE1/2048,
//! MODE1/2352, and AUDIO tracks, including INDEX 00 pregaps stored in
//! the files and PREGAP/POSTGAP gaps that are not (they read as zero
//! fill, like a CHD's unstored gaps). A `FILE` is `BINARY` (raw sector
//! bytes) or, for audio tracks, `WAVE` or `MP3` -- the packaged form a
//! disc's audio tracks often come in -- which the `audio` child module
//! decodes to CD-DA sectors on demand. INDEX times are file-relative
//! running time at 75 sectors per second regardless of each track's
//! sector size; the disc address space is the concatenation of all files
//! and gaps in cue order. A bare `.iso` image (2048-byte cooked data
//! sectors, no cue sheet) loads as a single-track data disc, and a `.chd`
//! loads through the compressed CHD backend in the `chd` child module.

use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

mod audio;
mod chd;
#[cfg(feature = "cd-mp3")]
mod mp3;
mod wav;

pub use audio::FileFormat;

pub const SECTORS_PER_SECOND: u32 = 75;
pub const DATA_SECTOR_BYTES: usize = 2048;
pub const RAW_SECTOR_BYTES: usize = 2352;
/// Standard lead-in offset: LBA 0 is MSF 00:02:00 on a real disc.
pub const LEADIN_SECTORS: u32 = 150;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TrackKind {
    /// 2048-byte user-data sectors (cooked).
    Mode1_2048,
    /// 2352-byte raw sectors carrying 2048 bytes of user data at +16.
    Mode1_2352,
    /// 2352-byte CD-DA sectors.
    Audio,
}

impl TrackKind {
    pub fn sector_bytes(self) -> usize {
        match self {
            TrackKind::Mode1_2048 => DATA_SECTOR_BYTES,
            TrackKind::Mode1_2352 | TrackKind::Audio => RAW_SECTOR_BYTES,
        }
    }

    pub fn is_data(self) -> bool {
        !matches!(self, TrackKind::Audio)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CdTrack {
    pub number: u8,
    pub kind: TrackKind,
    /// Disc sector of the track's INDEX 01 (where the TOC points).
    pub start_sector: u32,
    /// Sectors from INDEX 01 to the end of the track's region.
    #[allow(dead_code)]
    pub sector_count: u32,
}

/// A contiguous run of equally-sized sectors on the disc: one track's
/// stored region in one image file (including its in-file INDEX 00
/// pregap), or an unstored PREGAP/POSTGAP.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Extent {
    disc_start: u32,
    sector_count: u32,
    kind: TrackKind,
    storage: Storage,
}

/// Where an extent's sectors come from.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
enum Storage {
    /// `kind.sector_bytes()` per sector from `byte_offset` on in the
    /// source's byte space.
    Source { source: usize, byte_offset: u64 },
    /// A PREGAP/POSTGAP no file holds: zero fill.
    Gap,
}

#[derive(Debug)]
pub struct CdImage {
    tracks: Vec<CdTrack>,
    total_sectors: u32,
    backend: Backend,
}

/// Sector storage behind a `CdImage`: cue sheet files (or a bare ISO)
/// addressed by extent, or a compressed CHD hunk store.
#[derive(Debug)]
enum Backend {
    Bin(BinBackend),
    Chd(Box<chd::ChdImage>),
}

/// The plain-file backend shared by cue sheets and bare ISO images.
#[derive(Debug)]
struct BinBackend {
    sources: Vec<Source>,
    extents: Vec<Extent>,
}

/// One `FILE` of a cue sheet (or the bare ISO itself) as sector storage.
#[derive(Debug)]
struct Source {
    /// Host path, kept so a save state can reattach the (read-only) file.
    path: PathBuf,
    format: FileFormat,
    /// Bytes the cue layout sees: the file size for BINARY, the decoded
    /// CD-DA length rounded up to whole sectors for an audio format.
    byte_len: u64,
    data: SourceData,
}

#[derive(Debug)]
enum SourceData {
    /// Raw sector bytes straight from the file.
    Binary(File),
    /// An audio file decoded to CD-DA sectors.
    Audio(Box<audio::AudioSource>),
}

impl Source {
    fn open(path: &Path, format: FileFormat) -> Result<Self> {
        let (data, byte_len) = match format {
            FileFormat::Binary => {
                let file = File::open(path)
                    .with_context(|| format!("opening CD image {}", path.display()))?;
                let len = file
                    .metadata()
                    .with_context(|| format!("stat {}", path.display()))?
                    .len();
                (SourceData::Binary(file), len)
            }
            FileFormat::Wave | FileFormat::Mp3 => {
                let audio = audio::AudioSource::open(path, format)?;
                let len = audio.byte_len();
                (SourceData::Audio(Box::new(audio)), len)
            }
        };
        Ok(Self {
            path: path.to_path_buf(),
            format,
            byte_len,
            data,
        })
    }

    /// Read `buf.len()` bytes at `offset`. An audio source serves whole
    /// CD-DA sectors, so there `offset` is sector-aligned and `buf` one
    /// sector long (the cue layout only ever addresses it that way).
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        match &mut self.data {
            SourceData::Binary(file) => {
                file.seek(SeekFrom::Start(offset))?;
                file.read_exact(buf)?;
                Ok(())
            }
            SourceData::Audio(audio) => {
                debug_assert!(offset.is_multiple_of(RAW_SECTOR_BYTES as u64));
                let sector = u32::try_from(offset / RAW_SECTOR_BYTES as u64)
                    .with_context(|| format!("{}: sector offset {offset}", self.path.display()))?;
                audio.read_sector(sector, buf)
            }
        }
    }

    fn spec(&self) -> SourceSpec {
        SourceSpec {
            path: self.path.clone(),
            format: self.format,
            byte_len: self.byte_len,
        }
    }
}

/// Serde shadow of a `Source`: the file is read-only, so only its path
/// and format are stored and deserialization reopens it (re-indexing an
/// audio file), checking it still has the length the layout was built
/// on.
#[derive(serde::Serialize, serde::Deserialize)]
struct SourceSpec {
    path: PathBuf,
    format: FileFormat,
    byte_len: u64,
}

impl SourceSpec {
    fn reopen(&self) -> Result<Source> {
        let source = Source::open(&self.path, self.format)?;
        if source.byte_len != self.byte_len {
            bail!(
                "{} changed since the state was saved ({} bytes of sector data, was {})",
                self.path.display(),
                source.byte_len,
                self.byte_len
            );
        }
        Ok(source)
    }
}

/// Serde shadow of `CdImage`: the image files are read-only, so only
/// their paths are stored and deserialization reopens them (a CHD also
/// re-reads its header and track metadata).
#[derive(serde::Serialize, serde::Deserialize)]
enum CdImageState {
    Bin {
        sources: Vec<SourceSpec>,
        tracks: Vec<CdTrack>,
        extents: Vec<Extent>,
        total_sectors: u32,
    },
    Chd {
        path: PathBuf,
    },
}

impl serde::Serialize for CdImage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.backend {
            Backend::Bin(bin) => CdImageState::Bin {
                sources: bin.sources.iter().map(Source::spec).collect(),
                tracks: self.tracks.clone(),
                extents: bin.extents.clone(),
                total_sectors: self.total_sectors,
            },
            Backend::Chd(chd) => CdImageState::Chd {
                path: chd.path().to_path_buf(),
            },
        }
        .serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for CdImage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match CdImageState::deserialize(deserializer)? {
            CdImageState::Bin {
                sources,
                tracks,
                extents,
                total_sectors,
            } => {
                let sources = sources
                    .iter()
                    .map(|spec| {
                        spec.reopen().map_err(|e| {
                            serde::de::Error::custom(format!(
                                "reopening CD image {}: {e:#}",
                                spec.path.display()
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Self {
                    tracks,
                    total_sectors,
                    backend: Backend::Bin(BinBackend { sources, extents }),
                })
            }
            CdImageState::Chd { path } => Self::load_chd(&path).map_err(|e| {
                serde::de::Error::custom(format!("reopening CD image {}: {e:#}", path.display()))
            }),
        }
    }
}

#[derive(Debug)]
struct RawTrack {
    number: u8,
    kind: TrackKind,
    /// File-relative sector of INDEX 00, when present (pregap start).
    index0: Option<u32>,
    /// File-relative sector of INDEX 01.
    index1: Option<u32>,
    /// Unstored gap sectors before the file region (PREGAP).
    pregap: u32,
    /// Unstored gap sectors after the file region (POSTGAP).
    postgap: u32,
    file_index: usize,
}

impl CdImage {
    /// Load a CD image: a cue sheet (with its BINARY/WAVE/MP3 files), a
    /// bare `.iso` data image, or a `.chd`.
    pub fn load(path: &Path) -> Result<Self> {
        let ext = path.extension().and_then(|e| e.to_str());
        if ext.is_some_and(|e| e.eq_ignore_ascii_case("chd")) {
            Self::load_chd(path)
        } else if ext.is_some_and(|e| e.eq_ignore_ascii_case("iso")) {
            Self::load_iso(path)
        } else {
            Self::load_cue(path)
        }
    }

    /// Load a CHD (MAME "Compressed Hunks of Data") CD image.
    fn load_chd(path: &Path) -> Result<Self> {
        let (backend, tracks, total_sectors) = chd::ChdImage::load(path)?;
        Ok(Self {
            tracks,
            total_sectors,
            backend: Backend::Chd(Box::new(backend)),
        })
    }

    /// Load a bare data image as one MODE1/2048 track.
    fn load_iso(path: &Path) -> Result<Self> {
        let source = Source::open(path, FileFormat::Binary)?;
        let len = source.byte_len;
        if len == 0 || !len.is_multiple_of(DATA_SECTOR_BYTES as u64) {
            bail!(
                "{}: {len} bytes is not a whole number of 2048-byte data sectors",
                path.display()
            );
        }
        let sectors = (len / DATA_SECTOR_BYTES as u64) as u32;
        Ok(Self {
            tracks: vec![CdTrack {
                number: 1,
                kind: TrackKind::Mode1_2048,
                start_sector: 0,
                sector_count: sectors,
            }],
            total_sectors: sectors,
            backend: Backend::Bin(BinBackend {
                sources: vec![source],
                extents: vec![Extent {
                    disc_start: 0,
                    sector_count: sectors,
                    kind: TrackKind::Mode1_2048,
                    storage: Storage::Source {
                        source: 0,
                        byte_offset: 0,
                    },
                }],
            }),
        })
    }

    /// Load a cue sheet and open its image file(s).
    fn load_cue(cue_path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(cue_path)
            .with_context(|| format!("reading cue sheet {}", cue_path.display()))?;
        let dir = cue_path.parent().unwrap_or_else(|| Path::new("."));

        let mut files: Vec<(String, FileFormat)> = Vec::new();
        let mut raw_tracks: Vec<RawTrack> = Vec::new();
        for line in text.lines() {
            let mut words = line.split_whitespace();
            match words.next() {
                Some("FILE") => {
                    let rest = line.trim_start().strip_prefix("FILE").unwrap_or("").trim();
                    // The type word, when present, ends the line; the
                    // name before it may be quoted (with spaces) or bare.
                    let (name, format) = match rest.rsplit_once(char::is_whitespace) {
                        Some((name, word)) if FileFormat::is_cue_type(word) => {
                            let Some(format) = FileFormat::from_cue_type(word) else {
                                bail!(
                                    "{}: FILE type {} is not supported (BINARY, WAVE, MP3)",
                                    cue_path.display(),
                                    word.to_ascii_uppercase()
                                );
                            };
                            (name.trim(), format)
                        }
                        _ => (rest, FileFormat::Binary),
                    };
                    let name = name.trim_matches('"');
                    if name.is_empty() {
                        bail!("{}: FILE line has no file name", cue_path.display());
                    }
                    files.push((name.to_string(), format));
                }
                Some("TRACK") => {
                    let number: u8 = words
                        .next()
                        .and_then(|s| s.parse().ok())
                        .with_context(|| format!("{}: bad TRACK line", cue_path.display()))?;
                    let kind = match words.next() {
                        Some("MODE1/2048") => TrackKind::Mode1_2048,
                        Some("MODE1/2352") => TrackKind::Mode1_2352,
                        Some("AUDIO") => TrackKind::Audio,
                        other => bail!(
                            "{}: track {} type {:?} is not supported (MODE1/2048, \
                             MODE1/2352, AUDIO)",
                            cue_path.display(),
                            number,
                            other
                        ),
                    };
                    let Some((file_name, format)) = files.last() else {
                        bail!("{}: TRACK before FILE", cue_path.display());
                    };
                    if kind.is_data() && *format != FileFormat::Binary {
                        bail!(
                            "{}: track {} is a data track, but {} is a {} file; \
                             data tracks need a BINARY file",
                            cue_path.display(),
                            number,
                            file_name,
                            format.cue_type()
                        );
                    }
                    raw_tracks.push(RawTrack {
                        number,
                        kind,
                        index0: None,
                        index1: None,
                        pregap: 0,
                        postgap: 0,
                        file_index: files.len() - 1,
                    });
                }
                Some("INDEX") => {
                    let idx: u8 = words.next().and_then(|s| s.parse().ok()).unwrap_or(0xFF);
                    let msf = words
                        .next()
                        .with_context(|| format!("{}: INDEX without time", cue_path.display()))?;
                    let sector = parse_msf(msf)
                        .with_context(|| format!("{}: bad INDEX time {msf}", cue_path.display()))?;
                    let last = raw_tracks
                        .last_mut()
                        .with_context(|| format!("{}: INDEX before TRACK", cue_path.display()))?;
                    match idx {
                        0 => last.index0 = Some(sector),
                        1 => last.index1 = Some(sector),
                        _ => {}
                    }
                }
                Some(word @ ("PREGAP" | "POSTGAP")) => {
                    let msf = words
                        .next()
                        .with_context(|| format!("{}: {word} without time", cue_path.display()))?;
                    let sectors = parse_msf(msf).with_context(|| {
                        format!("{}: bad {word} time {msf}", cue_path.display())
                    })?;
                    let last = raw_tracks
                        .last_mut()
                        .with_context(|| format!("{}: {word} before TRACK", cue_path.display()))?;
                    if word == "PREGAP" {
                        last.pregap = sectors;
                    } else {
                        last.postgap = sectors;
                    }
                }
                // CATALOG / PERFORMER / TITLE / REM etc. are ignored.
                _ => {}
            }
        }
        if raw_tracks.is_empty() {
            bail!("{}: no tracks", cue_path.display());
        }
        for track in &raw_tracks {
            if track.index1.is_none() {
                bail!(
                    "{}: track {} has no INDEX 01",
                    cue_path.display(),
                    track.number
                );
            }
        }

        let mut sources = Vec::with_capacity(files.len());
        for (name, format) in &files {
            sources.push(Source::open(&dir.join(name), *format)?);
        }

        // Lay the files out back to back on the disc. Within one file,
        // each track's region runs from its INDEX 00 (or INDEX 01) to
        // the next track's region start or the file's end; sector sizes
        // may differ per track, so byte offsets accumulate region by
        // region and the file size must come out exact. PREGAP/POSTGAP
        // sectors occupy disc addresses around a region without file
        // bytes behind them.
        let mut tracks = Vec::with_capacity(raw_tracks.len());
        let mut extents: Vec<Extent> = Vec::with_capacity(raw_tracks.len());
        let mut disc = 0u32;
        let advance = |disc: &mut u32, sectors: u32| -> Result<()> {
            *disc = disc.checked_add(sectors).with_context(|| {
                format!(
                    "{}: cue layout exceeds the disc address space",
                    cue_path.display()
                )
            })?;
            Ok(())
        };
        let mut i = 0usize;
        while i < raw_tracks.len() {
            let file_index = raw_tracks[i].file_index;
            let mut in_file = i;
            while in_file < raw_tracks.len() && raw_tracks[in_file].file_index == file_index {
                in_file += 1;
            }
            let file_tracks = &raw_tracks[i..in_file];
            let file_len = sources[file_index].byte_len;
            let mut byte_offset = 0u64;
            for (j, track) in file_tracks.iter().enumerate() {
                if track.pregap > 0 {
                    extents.push(Extent {
                        disc_start: disc,
                        sector_count: track.pregap,
                        kind: track.kind,
                        storage: Storage::Gap,
                    });
                    advance(&mut disc, track.pregap)?;
                }
                let region_start = track.index0.unwrap_or_else(|| track.index1.unwrap());
                let index1 = track.index1.unwrap();
                if index1 < region_start {
                    bail!(
                        "{}: track {} INDEX 01 before INDEX 00",
                        cue_path.display(),
                        track.number
                    );
                }
                let region_end_sector = match file_tracks.get(j + 1) {
                    Some(next) => next.index0.unwrap_or_else(|| next.index1.unwrap()),
                    None => {
                        // Earlier tracks' INDEX values come straight from
                        // the (untrusted) cue sheet; a corrupt/crafted one
                        // can claim more bytes than the file actually has,
                        // which would underflow this subtraction.
                        let Some(remaining) = file_len.checked_sub(byte_offset) else {
                            bail!(
                                "{}: track {} starts {} bytes past the end of {}",
                                cue_path.display(),
                                track.number,
                                byte_offset.saturating_sub(file_len),
                                files[file_index].0,
                            );
                        };
                        if !remaining.is_multiple_of(track.kind.sector_bytes() as u64) {
                            bail!(
                                "{}: track {} does not end on a sector boundary",
                                cue_path.display(),
                                track.number
                            );
                        }
                        let sectors = u32::try_from(remaining / track.kind.sector_bytes() as u64)
                            .ok()
                            .and_then(|s| region_start.checked_add(s))
                            .with_context(|| {
                                format!(
                                    "{}: cue layout exceeds the disc address space",
                                    cue_path.display()
                                )
                            })?;
                        sectors
                    }
                };
                if region_end_sector < index1 {
                    bail!(
                        "{}: track {} INDEX times are not monotonic",
                        cue_path.display(),
                        track.number
                    );
                }
                let region_sectors = region_end_sector - region_start;
                extents.push(Extent {
                    disc_start: disc,
                    sector_count: region_sectors,
                    kind: track.kind,
                    storage: Storage::Source {
                        source: file_index,
                        byte_offset,
                    },
                });
                let index1_offset = index1 - region_start;
                let region_start_disc = disc;
                advance(&mut disc, region_sectors)?;
                // The region fits the address space, so INDEX 01 (inside
                // it) does too; the track's length still has to absorb
                // its postgap, which the cue sheet sizes freely.
                let sector_count = (region_sectors - index1_offset)
                    .checked_add(track.postgap)
                    .with_context(|| {
                        format!(
                            "{}: cue layout exceeds the disc address space",
                            cue_path.display()
                        )
                    })?;
                tracks.push(CdTrack {
                    number: track.number,
                    kind: track.kind,
                    start_sector: region_start_disc + index1_offset,
                    sector_count,
                });
                byte_offset += u64::from(region_sectors) * track.kind.sector_bytes() as u64;
                if track.postgap > 0 {
                    extents.push(Extent {
                        disc_start: disc,
                        sector_count: track.postgap,
                        kind: track.kind,
                        storage: Storage::Gap,
                    });
                    advance(&mut disc, track.postgap)?;
                }
            }
            if byte_offset != file_len {
                bail!(
                    "{}: cue layout covers {} bytes of {} but the file is {} bytes",
                    cue_path.display(),
                    byte_offset,
                    files[file_index].0,
                    file_len
                );
            }
            i = in_file;
        }

        Ok(Self {
            tracks,
            total_sectors: disc,
            backend: Backend::Bin(BinBackend { sources, extents }),
        })
    }

    pub fn tracks(&self) -> &[CdTrack] {
        &self.tracks
    }

    /// Total size of the disc in sectors.
    pub fn total_sectors(&self) -> u32 {
        self.total_sectors
    }

    /// The track kind covering `sector`, if it is on the disc.
    fn sector_kind(&self, sector: u32) -> Option<TrackKind> {
        match &self.backend {
            Backend::Bin(bin) => bin.extent_for_sector(sector).map(|e| e.kind),
            Backend::Chd(chd) => chd.sector_kind(sector),
        }
    }

    /// Read the stored payload of `sector`: `buf` must be the track
    /// kind's `sector_bytes()` long, and comes back in disc byte order.
    fn read_payload(&mut self, sector: u32, buf: &mut [u8]) -> Result<()> {
        match &mut self.backend {
            Backend::Bin(bin) => bin.read_payload(sector, buf),
            Backend::Chd(chd) => chd.read_payload(sector, buf),
        }
    }

    /// Read the 2048 bytes of user data in a data sector. Fails on audio
    /// tracks.
    pub fn read_data_sector(
        &mut self,
        sector: u32,
        buf: &mut [u8; DATA_SECTOR_BYTES],
    ) -> Result<()> {
        let kind = self
            .sector_kind(sector)
            .with_context(|| format!("sector {sector} beyond end of disc"))?;
        match kind {
            TrackKind::Mode1_2048 => self.read_payload(sector, buf),
            TrackKind::Mode1_2352 => {
                let mut raw = [0u8; RAW_SECTOR_BYTES];
                self.read_payload(sector, &mut raw)?;
                buf.copy_from_slice(&raw[16..16 + DATA_SECTOR_BYTES]);
                Ok(())
            }
            TrackKind::Audio => bail!("sector {sector} is in an audio track"),
        }
    }

    /// Read one 2352-byte raw sector from an audio track.
    pub fn read_audio_sector(
        &mut self,
        sector: u32,
        buf: &mut [u8; RAW_SECTOR_BYTES],
    ) -> Result<()> {
        let kind = self
            .sector_kind(sector)
            .with_context(|| format!("sector {sector} beyond end of disc"))?;
        if kind.is_data() {
            bail!("sector {sector} is in a data track");
        }
        self.read_payload(sector, buf)
    }

    /// Read one full 2352-byte raw frame at `sector`, whatever the track
    /// type: raw images are copied through; cooked (2048-byte) data
    /// sectors get a synthesized sync + BCD MSF header (zero EDC/ECC).
    pub fn read_raw_sector(&mut self, sector: u32, buf: &mut [u8; RAW_SECTOR_BYTES]) -> Result<()> {
        let kind = self
            .sector_kind(sector)
            .with_context(|| format!("sector {sector} beyond end of disc"))?;
        match kind {
            TrackKind::Mode1_2352 | TrackKind::Audio => self.read_payload(sector, buf),
            TrackKind::Mode1_2048 => {
                let mut data = [0u8; DATA_SECTOR_BYTES];
                self.read_payload(sector, &mut data)?;
                buf.fill(0);
                buf[1..11].fill(0xFF);
                let msf = sector + LEADIN_SECTORS;
                buf[12] = to_bcd((msf / (60 * 75)) as u8);
                buf[13] = to_bcd(((msf / 75) % 60) as u8);
                buf[14] = to_bcd((msf % 75) as u8);
                buf[15] = 1; // mode 1
                buf[16..16 + DATA_SECTOR_BYTES].copy_from_slice(&data);
                Ok(())
            }
        }
    }

    /// Whether `sector` falls inside an audio track region.
    pub fn is_audio_sector(&self, sector: u32) -> bool {
        self.sector_kind(sector).is_some_and(|k| !k.is_data())
    }

    /// One-line TOC summary for the log.
    pub fn describe(&self) -> String {
        let data = self.tracks.iter().filter(|t| t.kind.is_data()).count();
        let audio = self.tracks.len() - data;
        format!(
            "{} tracks ({} data, {} audio), {} sectors",
            self.tracks.len(),
            data,
            audio,
            self.total_sectors()
        )
    }
}

impl BinBackend {
    fn extent_for_sector(&self, sector: u32) -> Option<&Extent> {
        self.extents
            .iter()
            .find(|e| sector >= e.disc_start && sector < e.disc_start + e.sector_count)
    }

    fn read_payload(&mut self, sector: u32, buf: &mut [u8]) -> Result<()> {
        let extent = self
            .extent_for_sector(sector)
            .with_context(|| format!("sector {sector} beyond end of disc"))?;
        let in_extent = u64::from(sector - extent.disc_start);
        let sector_bytes = extent.kind.sector_bytes() as u64;
        match extent.storage {
            Storage::Gap => {
                buf.fill(0);
                Ok(())
            }
            Storage::Source {
                source,
                byte_offset,
            } => self.sources[source].read_at(byte_offset + in_extent * sector_bytes, buf),
        }
    }
}

pub(crate) fn to_bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

/// Parse a cue MM:SS:FF time to a sector number.
fn parse_msf(s: &str) -> Result<u32> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        bail!("expected MM:SS:FF");
    }
    let mm: u32 = parts[0].parse()?;
    let ss: u32 = parts[1].parse()?;
    let ff: u32 = parts[2].parse()?;
    if ss >= 60 || ff >= SECTORS_PER_SECOND {
        bail!("out-of-range MSF field");
    }
    mm.checked_mul(60)
        .and_then(|m| m.checked_add(ss))
        .and_then(|s| s.checked_mul(SECTORS_PER_SECOND))
        .and_then(|s| s.checked_add(ff))
        .context("MSF time too large")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "copperline-cd-{}-{unique}-{name}",
            std::process::id()
        ))
    }

    fn write_file(path: &Path, bytes: &[u8]) {
        let mut f = File::create(path).unwrap();
        f.write_all(bytes).unwrap();
    }

    #[test]
    fn parses_msf_times() {
        assert_eq!(parse_msf("00:00:00").unwrap(), 0);
        assert_eq!(parse_msf("00:32:49").unwrap(), 32 * 75 + 49);
        assert_eq!(parse_msf("62:03:74").unwrap(), (62 * 60 + 3) * 75 + 74);
        assert!(parse_msf("00:60:00").is_err());
        assert!(parse_msf("00:00:75").is_err());
    }

    #[test]
    fn single_file_mixed_mode_layout_round_trips() {
        let cue = temp_path("mixed.cue");
        let bin = temp_path("mixed.bin");
        // 4 data sectors at 2048 bytes, then 2 audio sectors at 2352.
        let mut bytes = Vec::new();
        for s in 0..4u8 {
            bytes.extend(std::iter::repeat_n(s, DATA_SECTOR_BYTES));
        }
        for s in 0..2u8 {
            bytes.extend(std::iter::repeat_n(0xA0 + s, RAW_SECTOR_BYTES));
        }
        write_file(&bin, &bytes);
        std::fs::write(
            &cue,
            format!(
                "FILE \"{}\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 00:00:04\n",
                bin.file_name().unwrap().to_string_lossy()
            ),
        )
        .unwrap();
        let mut image = CdImage::load(&cue).unwrap();
        assert_eq!(image.tracks().len(), 2);
        assert_eq!(image.total_sectors(), 6);
        assert_eq!(image.tracks()[1].start_sector, 4);

        let mut data = [0u8; DATA_SECTOR_BYTES];
        image.read_data_sector(3, &mut data).unwrap();
        assert!(data.iter().all(|&b| b == 3));
        assert!(image.read_data_sector(4, &mut data).is_err());

        let mut audio = [0u8; RAW_SECTOR_BYTES];
        image.read_audio_sector(5, &mut audio).unwrap();
        assert!(audio.iter().all(|&b| b == 0xA1));
        assert!(image.is_audio_sector(5));
        assert!(!image.is_audio_sector(2));

        let _ = std::fs::remove_file(&cue);
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn serde_reopens_image_files_and_serves_same_sectors() {
        let cue = temp_path("serde.cue");
        let bin = temp_path("serde.bin");
        let mut bytes = Vec::new();
        for s in 0..4u8 {
            bytes.extend(std::iter::repeat_n(s, DATA_SECTOR_BYTES));
        }
        write_file(&bin, &bytes);
        std::fs::write(
            &cue,
            format!(
                "FILE \"{}\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n",
                bin.file_name().unwrap().to_string_lossy()
            ),
        )
        .unwrap();
        let mut image = CdImage::load(&cue).unwrap();

        let encoded = bincode::serialize(&image).unwrap();
        let mut restored: CdImage = bincode::deserialize(&encoded).unwrap();
        assert_eq!(restored.total_sectors(), image.total_sectors());
        assert_eq!(restored.tracks().len(), image.tracks().len());
        let mut a = [0u8; DATA_SECTOR_BYTES];
        let mut b = [0u8; DATA_SECTOR_BYTES];
        for sector in 0..4 {
            image.read_data_sector(sector, &mut a).unwrap();
            restored.read_data_sector(sector, &mut b).unwrap();
            assert_eq!(a, b, "sector {sector}");
        }

        // A missing image file must fail the load with the path named,
        // not deserialize into an image that panics on first read.
        let _ = std::fs::remove_file(&bin);
        let err = bincode::deserialize::<CdImage>(&encoded)
            .expect_err("deserializing with the image file gone must fail");
        assert!(err.to_string().contains("reopening CD image"));

        let _ = std::fs::remove_file(&cue);
    }

    #[test]
    fn raw_data_track_skips_sector_header() {
        let cue = temp_path("raw.cue");
        let bin = temp_path("raw.bin");
        let mut sector = vec![0u8; RAW_SECTOR_BYTES];
        for (i, b) in sector.iter_mut().enumerate() {
            *b = if i < 16 { 0xEE } else { 0x42 };
        }
        write_file(&bin, &sector);
        std::fs::write(
            &cue,
            format!(
                "FILE \"{}\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n",
                bin.file_name().unwrap().to_string_lossy()
            ),
        )
        .unwrap();
        let mut image = CdImage::load(&cue).unwrap();
        let mut data = [0u8; DATA_SECTOR_BYTES];
        image.read_data_sector(0, &mut data).unwrap();
        assert!(data.iter().all(|&b| b == 0x42));
        let _ = std::fs::remove_file(&cue);
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn multi_file_cue_with_pregap_lays_out_disc_addresses() {
        // Track 1: data, 3 raw sectors in its own file. Track 2: audio
        // with a 2-sector in-file pregap (INDEX 00) plus 4 sectors of
        // content. Track 3: audio, 2 sectors.
        let cue = temp_path("multi.cue");
        let bin1 = temp_path("t1.bin");
        let bin2 = temp_path("t2.bin");
        let bin3 = temp_path("t3.bin");
        write_file(&bin1, &vec![0x11u8; 3 * RAW_SECTOR_BYTES]);
        write_file(&bin2, &vec![0x22u8; 6 * RAW_SECTOR_BYTES]);
        write_file(&bin3, &vec![0x33u8; 2 * RAW_SECTOR_BYTES]);
        std::fs::write(
            &cue,
            format!(
                concat!(
                    "CATALOG 0000000000000\n",
                    "FILE \"{}\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n",
                    "FILE \"{}\" BINARY\n  TRACK 02 AUDIO\n    INDEX 00 00:00:00\n    INDEX 01 00:00:02\n",
                    "FILE \"{}\" BINARY\n  TRACK 03 AUDIO\n    INDEX 01 00:00:00\n",
                ),
                bin1.file_name().unwrap().to_string_lossy(),
                bin2.file_name().unwrap().to_string_lossy(),
                bin3.file_name().unwrap().to_string_lossy(),
            ),
        )
        .unwrap();
        let mut image = CdImage::load(&cue).unwrap();
        assert_eq!(image.total_sectors(), 3 + 6 + 2);
        let tracks = image.tracks();
        assert_eq!(tracks[0].start_sector, 0);
        assert_eq!(tracks[0].sector_count, 3);
        // Track 2's INDEX 01 lands after the 2-sector pregap.
        assert_eq!(tracks[1].start_sector, 5);
        assert_eq!(tracks[1].sector_count, 4);
        assert_eq!(tracks[2].start_sector, 9);
        assert_eq!(tracks[2].sector_count, 2);

        // Reads address the disc across files; the pregap is readable
        // as part of track 2's region.
        let mut audio = [0u8; RAW_SECTOR_BYTES];
        image.read_audio_sector(3, &mut audio).unwrap(); // pregap
        assert!(audio.iter().all(|&b| b == 0x22));
        image.read_audio_sector(9, &mut audio).unwrap();
        assert!(audio.iter().all(|&b| b == 0x33));
        let mut data = [0u8; DATA_SECTOR_BYTES];
        image.read_data_sector(2, &mut data).unwrap();
        assert!(data.iter().all(|&b| b == 0x11));

        let _ = std::fs::remove_file(&cue);
        let _ = std::fs::remove_file(&bin1);
        let _ = std::fs::remove_file(&bin2);
        let _ = std::fs::remove_file(&bin3);
    }

    #[test]
    fn track_index_claiming_more_than_the_file_holds_is_rejected_not_a_panic() {
        // Track 1's INDEX 01 (used as track 2's region start, i.e. track
        // 1's region end) claims a sector far beyond what the shared file
        // actually contains. That inflates `byte_offset` past `file_len`
        // by the time the last track computes its remaining-bytes span,
        // which must be rejected as a malformed cue sheet instead of
        // underflowing the subtraction.
        let cue = temp_path("overrun.cue");
        let bin = temp_path("overrun.bin");
        write_file(&bin, &vec![0u8; 2 * RAW_SECTOR_BYTES]);
        std::fs::write(
            &cue,
            format!(
                concat!(
                    "FILE \"{}\" BINARY\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
                    "  TRACK 02 AUDIO\n    INDEX 01 00:10:00\n",
                ),
                bin.file_name().unwrap().to_string_lossy(),
            ),
        )
        .unwrap();
        let err = CdImage::load(&cue).expect_err("overrunning cue sheet must be rejected");
        assert!(
            err.to_string().contains("past the end of"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_file(&cue);
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn bare_iso_loads_as_single_data_track() {
        let iso = temp_path("plain.iso");
        let mut bytes = Vec::new();
        for s in 0..3u8 {
            bytes.extend(std::iter::repeat_n(s, DATA_SECTOR_BYTES));
        }
        write_file(&iso, &bytes);
        let mut image = CdImage::load(&iso).unwrap();
        assert_eq!(image.tracks().len(), 1);
        assert_eq!(image.tracks()[0].kind, TrackKind::Mode1_2048);
        assert_eq!(image.total_sectors(), 3);
        let mut data = [0u8; DATA_SECTOR_BYTES];
        image.read_data_sector(2, &mut data).unwrap();
        assert!(data.iter().all(|&b| b == 2));
        assert!(!image.is_audio_sector(0));
        let _ = std::fs::remove_file(&iso);
    }

    #[test]
    fn iso_with_partial_sector_is_rejected() {
        let iso = temp_path("ragged.iso");
        write_file(&iso, &vec![0u8; DATA_SECTOR_BYTES + 100]);
        let err = CdImage::load(&iso).unwrap_err();
        assert!(err.to_string().contains("2048-byte"), "{err:#}");
        let _ = std::fs::remove_file(&iso);
    }

    #[test]
    fn size_mismatch_is_rejected() {
        let cue = temp_path("short.cue");
        let bin = temp_path("short.bin");
        write_file(&bin, &vec![0u8; DATA_SECTOR_BYTES + 7]);
        std::fs::write(
            &cue,
            format!(
                "FILE \"{}\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n",
                bin.file_name().unwrap().to_string_lossy()
            ),
        )
        .unwrap();
        let err = CdImage::load(&cue).unwrap_err();
        assert!(err.to_string().contains("sector boundary"), "{err:#}");
        let _ = std::fs::remove_file(&cue);
        let _ = std::fs::remove_file(&bin);
    }

    // ---- WAVE / MP3 files and PREGAP/POSTGAP ----

    /// Write a WAV of 16-bit stereo `samples` at `rate` Hz.
    fn write_wav16(path: &Path, rate: u32, samples: &[(i16, i16)]) {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for &(l, r) in samples {
            w.write_sample(l).unwrap();
            w.write_sample(r).unwrap();
        }
        w.finalize().unwrap();
    }

    fn file_line(path: &Path, cue_type: &str) -> String {
        format!(
            "FILE \"{}\" {cue_type}\n",
            path.file_name().unwrap().to_string_lossy()
        )
    }

    /// A cue sheet next to `media` holding it as one AUDIO track.
    fn single_audio_track_cue(media: &Path, cue_type: &str) -> PathBuf {
        let cue = media.with_extension("cue");
        std::fs::write(
            &cue,
            format!(
                "{}  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
                file_line(media, cue_type)
            ),
        )
        .unwrap();
        cue
    }

    /// A CD-DA sector as (left, right) sample pairs.
    fn pairs(sector: &[u8; RAW_SECTOR_BYTES]) -> Vec<(i16, i16)> {
        sector
            .chunks_exact(4)
            .map(|c| {
                (
                    i16::from_le_bytes([c[0], c[1]]),
                    i16::from_le_bytes([c[2], c[3]]),
                )
            })
            .collect()
    }

    fn audio_sector(image: &mut CdImage, sector: u32) -> Vec<(i16, i16)> {
        let mut raw = [0u8; RAW_SECTOR_BYTES];
        image.read_audio_sector(sector, &mut raw).unwrap();
        pairs(&raw)
    }

    fn zero_crossings(samples: &[i16]) -> usize {
        samples
            .windows(2)
            .filter(|w| (w[0] < 0) != (w[1] < 0))
            .count()
    }

    #[test]
    fn wave_file_serves_cdda_sectors_padded_to_whole_sectors() {
        let cue = temp_path("wave.cue");
        let bin = temp_path("wave.bin");
        // A name with a space: the FILE line's type word must not be
        // mistaken for part of it, nor the name's last word for a type.
        let wav = temp_path("audio track.wav");
        let mut data = Vec::new();
        data.extend(std::iter::repeat_n(0x11u8, DATA_SECTOR_BYTES));
        data.extend(std::iter::repeat_n(0x22u8, DATA_SECTOR_BYTES));
        write_file(&bin, &data);
        let samples: Vec<(i16, i16)> = (0..1000i16).map(|i| (i * 3, -(i * 3))).collect();
        write_wav16(&wav, 44_100, &samples);
        std::fs::write(
            &cue,
            format!(
                "{}  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n\
                 {}  TRACK 02 AUDIO\n    INDEX 01 00:00:00\n",
                file_line(&bin, "BINARY"),
                file_line(&wav, "WAVE")
            ),
        )
        .unwrap();
        let mut image = CdImage::load(&cue).unwrap();

        // 1000 frames is one full sector and a 412-frame remainder.
        assert_eq!(image.total_sectors(), 4);
        assert_eq!(image.tracks()[1].kind, TrackKind::Audio);
        assert_eq!(image.tracks()[1].start_sector, 2);
        assert_eq!(image.tracks()[1].sector_count, 2);
        assert!(!image.is_audio_sector(1));
        assert!(image.is_audio_sector(2));
        assert!(image.is_audio_sector(3));
        assert_eq!(audio_sector(&mut image, 2), samples[..588]);
        let last = audio_sector(&mut image, 3);
        assert_eq!(last[..412], samples[588..]);
        assert!(last[412..].iter().all(|&s| s == (0, 0)), "tail zero-padded");
        // Reading out of order is plain random access for a WAV.
        assert_eq!(audio_sector(&mut image, 2), samples[..588]);
        let mut data = [0u8; DATA_SECTOR_BYTES];
        image.read_data_sector(1, &mut data).unwrap();
        assert!(data.iter().all(|&b| b == 0x22));

        for p in [&cue, &bin, &wav] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn wave_sample_formats_scale_to_16_bit_stereo() {
        // 8-bit mono plays on both channels at 16-bit scale.
        let wav8 = temp_path("mono8.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 8,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&wav8, spec).unwrap();
        for v in [-128i8, -1, 0, 1, 127] {
            w.write_sample(v).unwrap();
        }
        w.finalize().unwrap();
        let cue = single_audio_track_cue(&wav8, "WAVE");
        let mut image = CdImage::load(&cue).unwrap();
        let got = audio_sector(&mut image, 0);
        assert_eq!(
            got[..5],
            [
                (-32768, -32768),
                (-256, -256),
                (0, 0),
                (256, 256),
                (32512, 32512)
            ]
        );
        assert!(got[5..].iter().all(|&s| s == (0, 0)));

        // 24-bit stereo keeps the top 16 bits.
        let wav24 = temp_path("st24.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 24,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&wav24, spec).unwrap();
        for v in [0x7F_FFFFi32, -0x80_0000, 0x12_3456, -0x12_3456] {
            w.write_sample(v).unwrap();
        }
        w.finalize().unwrap();
        let cue24 = single_audio_track_cue(&wav24, "WAVE");
        let mut image = CdImage::load(&cue24).unwrap();
        let got = audio_sector(&mut image, 0);
        assert_eq!(got[..2], [(0x7FFF, -0x8000), (0x1234, -0x1235)]);

        // 32-bit float is scaled and clamped.
        let wavf = temp_path("float.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&wavf, spec).unwrap();
        for v in [0.5f32, -1.0, 2.0, -2.0] {
            w.write_sample(v).unwrap();
        }
        w.finalize().unwrap();
        let cuef = single_audio_track_cue(&wavf, "WAVE");
        let mut image = CdImage::load(&cuef).unwrap();
        let got = audio_sector(&mut image, 0);
        assert_eq!(got[..2], [(16384, -32767), (32767, -32767)]);

        for p in [&wav8, &cue, &wav24, &cue24, &wavf, &cuef] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn wave_at_48_khz_resamples_to_cdda_length_and_values() {
        // A linear ramp resamples to the interpolation formula exactly,
        // so every output frame can be checked, not just the length.
        let wav = temp_path("48k.wav");
        let samples: Vec<(i16, i16)> = (0..4800i16).map(|i| (i * 5, -(i * 5))).collect();
        write_wav16(&wav, 48_000, &samples);
        let cue = single_audio_track_cue(&wav, "WAVE");
        let mut image = CdImage::load(&cue).unwrap();
        // 4800 frames at 48 kHz are 4410 at 44.1 kHz: 7.5 sectors.
        assert_eq!(image.total_sectors(), 8);
        let mut got = Vec::new();
        for sector in 0..8 {
            got.extend(audio_sector(&mut image, sector));
        }
        for (f, &(l, r)) in got.iter().enumerate().take(4410) {
            let pos = f as u64 * 48_000;
            let i = (pos / 44_100) as i64;
            let frac = (pos % 44_100) as i64;
            let expect = (5 * i + 5 * frac / 44_100) as i16;
            assert_eq!((l, r), (expect, -expect), "frame {f}");
        }
        assert!(got[4410..].iter().all(|&s| s == (0, 0)));
        let _ = std::fs::remove_file(&wav);
        let _ = std::fs::remove_file(&cue);
    }

    #[test]
    fn resampled_padding_past_the_declared_length_is_silent() {
        // Two frames at 48 kHz are one frame at 44.1 kHz: output frame 1
        // would interpolate between the last real sample and the zero
        // fill, but it lies past the declared length and must be silent.
        let wav = temp_path("48k-tail.wav");
        write_wav16(&wav, 48_000, &[(20_000, -20_000), (20_000, -20_000)]);
        let cue = single_audio_track_cue(&wav, "WAVE");
        let mut image = CdImage::load(&cue).unwrap();
        assert_eq!(image.total_sectors(), 1);
        let got = audio_sector(&mut image, 0);
        assert_eq!(got[0], (20_000, -20_000));
        assert!(got[1..].iter().all(|&s| s == (0, 0)), "{:?}", &got[..3]);
        let _ = std::fs::remove_file(&wav);
        let _ = std::fs::remove_file(&cue);
    }

    #[test]
    fn pregap_and_postgap_occupy_zero_filled_disc_sectors() {
        let cue = temp_path("gaps.cue");
        let bin = temp_path("gaps.bin");
        let wav = temp_path("gaps.wav");
        write_file(&bin, &[0x5Au8; 2 * DATA_SECTOR_BYTES]);
        let samples: Vec<(i16, i16)> = (0..600i16).map(|i| (i, i + 1)).collect();
        write_wav16(&wav, 44_100, &samples);
        std::fs::write(
            &cue,
            format!(
                "{}  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n\
                 {}  TRACK 02 AUDIO\n    PREGAP 00:02:00\n    INDEX 01 00:00:00\n    POSTGAP 00:00:02\n",
                file_line(&bin, "BINARY"),
                file_line(&wav, "WAVE")
            ),
        )
        .unwrap();
        let mut image = CdImage::load(&cue).unwrap();

        // 2 data + 150 pregap + 2 audio + 2 postgap sectors.
        assert_eq!(image.total_sectors(), 156);
        let track = &image.tracks()[1];
        assert_eq!(track.start_sector, 152, "INDEX 01 follows the pregap");
        assert_eq!(track.sector_count, 4, "postgap belongs to the track");
        assert!(image.is_audio_sector(2), "the pregap is audio");
        assert!(audio_sector(&mut image, 2).iter().all(|&s| s == (0, 0)));
        assert!(audio_sector(&mut image, 151).iter().all(|&s| s == (0, 0)));
        assert_eq!(audio_sector(&mut image, 152), samples[..588]);
        assert!(audio_sector(&mut image, 155).iter().all(|&s| s == (0, 0)));
        let mut raw = [0u8; RAW_SECTOR_BYTES];
        assert!(image.read_audio_sector(156, &mut raw).is_err());

        for p in [&cue, &bin, &wav] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn file_line_without_a_type_is_binary() {
        let cue = temp_path("notype.cue");
        let bin = temp_path("notype.bin");
        write_file(&bin, &[0x33u8; DATA_SECTOR_BYTES]);
        std::fs::write(
            &cue,
            format!(
                "FILE \"{}\"\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n",
                bin.file_name().unwrap().to_string_lossy()
            ),
        )
        .unwrap();
        let mut image = CdImage::load(&cue).unwrap();
        let mut data = [0u8; DATA_SECTOR_BYTES];
        image.read_data_sector(0, &mut data).unwrap();
        assert!(data.iter().all(|&b| b == 0x33));
        let _ = std::fs::remove_file(&cue);
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn data_tracks_and_unsupported_file_types_are_rejected() {
        let wav = temp_path("data.wav");
        write_wav16(&wav, 44_100, &[(0, 0); 588]);
        let cue = temp_path("data.cue");
        std::fs::write(
            &cue,
            format!(
                "{}  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n",
                file_line(&wav, "WAVE")
            ),
        )
        .unwrap();
        let err = CdImage::load(&cue).unwrap_err().to_string();
        assert!(err.contains("data tracks need a BINARY file"), "{err}");

        for cue_type in ["MOTOROLA", "AIFF"] {
            std::fs::write(
                &cue,
                format!(
                    "{}  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
                    file_line(&wav, cue_type)
                ),
            )
            .unwrap();
            let err = CdImage::load(&cue).unwrap_err().to_string();
            assert!(
                err.contains(&format!("FILE type {cue_type} is not supported")),
                "{err}"
            );
        }
        let _ = std::fs::remove_file(&cue);
        let _ = std::fs::remove_file(&wav);
    }

    #[test]
    fn gaps_that_overflow_the_disc_address_space_are_rejected_not_a_panic() {
        // An 800-sector audio file with a POSTGAP sized so that the
        // track's sector count (region + postgap) passes u32::MAX, and a
        // PREGAP that pushes INDEX 01 of the region past it.
        let cue = temp_path("bigap.cue");
        let bin = temp_path("biggap.bin");
        write_file(&bin, &vec![0u8; 800 * RAW_SECTOR_BYTES]);
        for (name, lines) in [
            (
                "postgap",
                "    INDEX 01 00:00:00\n    POSTGAP 954437:00:00\n",
            ),
            (
                "pregap",
                "    PREGAP 954437:00:00\n    INDEX 00 00:00:00\n    INDEX 01 00:10:46\n",
            ),
        ] {
            std::fs::write(
                &cue,
                format!("{}  TRACK 01 AUDIO\n{lines}", file_line(&bin, "BINARY")),
            )
            .unwrap();
            let err = CdImage::load(&cue).unwrap_err().to_string();
            assert!(
                err.contains("exceeds the disc address space"),
                "{name}: {err}"
            );
        }
        let _ = std::fs::remove_file(&cue);
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn msf_times_that_overflow_are_rejected_not_a_panic() {
        assert!(parse_msf("99999999:00:00").is_err());
        assert!(parse_msf("4294967295:59:74").is_err());
    }

    /// Six tenths of a second of stereo tones -- 440 Hz left, 1320 Hz
    /// right, at half scale (ffmpeg's `sine` source is an eighth of full
    /// scale) -- encoded with ffmpeg's libmp3lame at 64 kbps CBR,
    /// carrying an ID3v2 tag and a LAME Info frame the way a packaged
    /// track does:
    ///
    /// ```text
    /// ffmpeg -f lavfi -i "sine=frequency=440:sample_rate=44100:duration=0.6" \
    ///   -f lavfi -i "sine=frequency=1320:sample_rate=44100:duration=0.6" \
    ///   -filter_complex "[0:a][1:a]join=inputs=2:channel_layout=stereo,volume=4[a]" \
    ///   -map "[a]" -ar 44100 -c:a libmp3lame -b:a 64k \
    ///   tests/data/cdrom/stereo_440l_1320r_cbr64.mp3
    /// ```
    #[cfg(feature = "cd-mp3")]
    const STEREO_TONES_MP3: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/cdrom/stereo_440l_1320r_cbr64.mp3"
    );

    /// The MHI board's 3-second mono VBR fixture: a second stream shape
    /// (VBR, mono, Xing frame) for the seek-consistency check.
    #[cfg(feature = "cd-mp3")]
    const MONO_VBR_MP3: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/mhi/vbr_sweep.mp3");

    /// Three seconds of stereo tones (440 Hz left, 660 Hz right) at the
    /// bottom of the MPEG-2 range: 8 kbps CBR at 16 kHz and at 24 kHz,
    /// stereo, CRC-protected. A 16 kHz frame is 36 bytes with 13 bytes of
    /// main data; a 24 kHz frame is 24 bytes with a single main-data
    /// byte, so `main_data_begin` can reach back hundreds of frames --
    /// the seek warm-up has to be sized in reservoir bytes, not frames.
    ///
    /// ```text
    /// ffmpeg -f lavfi -i "sine=frequency=440:sample_rate=16000:duration=3" \
    ///   -f lavfi -i "sine=frequency=660:sample_rate=16000:duration=3" \
    ///   -filter_complex "[0:a][1:a]join=inputs=2:channel_layout=stereo,volume=4[a]" \
    ///   -map "[a]" tones.wav
    /// lame -b 8 --resample 16 -m s -p tones.wav stereo_440l_660r_mpeg2_16k_cbr8_crc.mp3
    /// lame -b 8 --resample 24 -m s -p tones.wav stereo_440l_660r_mpeg2_24k_cbr8_crc.mp3
    /// ```
    #[cfg(feature = "cd-mp3")]
    const LOW_RATE_16K_MP3: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/cdrom/stereo_440l_660r_mpeg2_16k_cbr8_crc.mp3"
    );
    #[cfg(feature = "cd-mp3")]
    const LOW_RATE_24K_MP3: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/cdrom/stereo_440l_660r_mpeg2_24k_cbr8_crc.mp3"
    );

    /// A cue sheet in the temp dir naming `media` by absolute path as one
    /// AUDIO track (a FILE path may be absolute).
    #[cfg(feature = "cd-mp3")]
    fn absolute_media_cue(name: &str, media: &str, cue_type: &str) -> PathBuf {
        let cue = temp_path(name);
        std::fs::write(
            &cue,
            format!("FILE \"{media}\" {cue_type}\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n"),
        )
        .unwrap();
        cue
    }

    #[cfg(feature = "cd-mp3")]
    #[test]
    fn mp3_track_decodes_to_stereo_cdda_of_the_encoded_length() {
        let cue = absolute_media_cue("mp3.cue", STEREO_TONES_MP3, "MP3");
        let mut image = CdImage::load(&cue).unwrap();
        // 0.6 s is 26460 frames, exactly 45 sectors, once the Info frame
        // is dropped and the LAME encoder delay/padding trimmed.
        assert_eq!(image.total_sectors(), 45);
        assert_eq!(image.tracks()[0].kind, TrackKind::Audio);
        let mut frames = Vec::new();
        for sector in 0..45 {
            frames.extend(audio_sector(&mut image, sector));
        }
        let left: Vec<i16> = frames.iter().map(|s| s.0).collect();
        let right: Vec<i16> = frames.iter().map(|s| s.1).collect();
        // Two zero crossings per cycle: 528 for 440 Hz, 1584 for 1320 Hz.
        let zl = zero_crossings(&left);
        let zr = zero_crossings(&right);
        assert!((500..=556).contains(&zl), "left zero crossings {zl}");
        assert!((1500..=1668).contains(&zr), "right zero crossings {zr}");
        let peak = |s: &[i16]| s.iter().map(|v| i32::from(*v).abs()).max().unwrap();
        assert!(
            (12_000..=20_000).contains(&peak(&left)),
            "left peak {}",
            peak(&left)
        );
        assert!(
            (12_000..=20_000).contains(&peak(&right)),
            "right peak {}",
            peak(&right)
        );
        let _ = std::fs::remove_file(&cue);
    }

    /// The samples a sector decodes to must not depend on how the decode
    /// cursor reached it: a run resumed from a save state mid-track has
    /// to produce the bytes an uninterrupted run did.
    #[cfg(feature = "cd-mp3")]
    #[test]
    fn mp3_seek_decode_matches_linear_decode() {
        // The LAME-tagged fixtures are sample-exact (0.6 s and 3 s); the
        // 8 kbps ones carry no gapless tag, so only their playback is
        // checked.
        for (name, media, sectors) in [
            ("lin-stereo.cue", STEREO_TONES_MP3, Some(45)),
            ("lin-mono.cue", MONO_VBR_MP3, Some(225)),
            ("lin-16k.cue", LOW_RATE_16K_MP3, None),
            ("lin-24k.cue", LOW_RATE_24K_MP3, None),
        ] {
            let cue = absolute_media_cue(name, media, "MP3");
            let mut linear = CdImage::load(&cue).unwrap();
            if let Some(sectors) = sectors {
                assert_eq!(linear.total_sectors(), sectors, "{media}");
            }
            let sectors = linear.total_sectors();
            let reference: Vec<Vec<(i16, i16)>> =
                (0..sectors).map(|s| audio_sector(&mut linear, s)).collect();
            assert!(
                reference.iter().flatten().any(|&s| s != (0, 0)),
                "{media}: decoded to silence"
            );

            // Backwards, then in strides: every read but the first
            // continuation is a seek.
            let mut scrambled = CdImage::load(&cue).unwrap();
            let order: Vec<u32> = (0..sectors)
                .rev()
                .chain((0..sectors).step_by(7))
                .chain((3..sectors).step_by(11))
                .collect();
            for s in order {
                assert_eq!(
                    audio_sector(&mut scrambled, s),
                    reference[s as usize],
                    "{media}: sector {s}"
                );
            }
            // A fresh image whose very first read lands mid-track.
            let mut cold = CdImage::load(&cue).unwrap();
            let mid = sectors / 2;
            assert_eq!(
                audio_sector(&mut cold, mid),
                reference[mid as usize],
                "{media}"
            );
            let _ = std::fs::remove_file(&cue);
        }
    }

    #[cfg(feature = "cd-mp3")]
    #[test]
    fn serde_reopens_decoded_audio_files_and_rejects_changed_ones() {
        let cue = temp_path("audio-serde.cue");
        let wav = temp_path("audio-serde.wav");
        let samples: Vec<(i16, i16)> = (0..700i16).map(|i| (i * 7, i * -7)).collect();
        write_wav16(&wav, 44_100, &samples);
        std::fs::write(
            &cue,
            format!(
                "FILE \"{STEREO_TONES_MP3}\" MP3\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n\
                 {}  TRACK 02 AUDIO\n    PREGAP 00:00:01\n    INDEX 01 00:00:00\n",
                file_line(&wav, "WAVE")
            ),
        )
        .unwrap();
        let mut image = CdImage::load(&cue).unwrap();
        assert_eq!(image.total_sectors(), 45 + 1 + 2);

        let encoded = bincode::serialize(&image).unwrap();
        let mut restored: CdImage = bincode::deserialize(&encoded).unwrap();
        assert_eq!(restored.total_sectors(), image.total_sectors());
        for sector in [0, 20, 44, 45, 46, 47] {
            assert_eq!(
                audio_sector(&mut restored, sector),
                audio_sector(&mut image, sector),
                "sector {sector}"
            );
        }

        // A WAV that changed length since the save cannot back the
        // layout the state recorded.
        write_wav16(&wav, 44_100, &samples[..100]);
        let err = bincode::deserialize::<CdImage>(&encoded)
            .expect_err("a changed audio file must fail the reopen");
        assert!(
            err.to_string()
                .contains("changed since the state was saved"),
            "{err}"
        );

        let _ = std::fs::remove_file(&cue);
        let _ = std::fs::remove_file(&wav);
    }

    #[cfg(not(feature = "cd-mp3"))]
    #[test]
    fn mp3_tracks_need_the_cd_mp3_feature() {
        let cue = temp_path("nomp3.cue");
        let mp3 = temp_path("nomp3.mp3");
        write_file(&mp3, &[0u8; 16]);
        std::fs::write(
            &cue,
            format!(
                "{}  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
                file_line(&mp3, "MP3")
            ),
        )
        .unwrap();
        let err = CdImage::load(&cue).unwrap_err().to_string();
        assert!(err.contains("cd-mp3"), "{err}");
        let _ = std::fs::remove_file(&cue);
        let _ = std::fs::remove_file(&mp3);
    }
}
