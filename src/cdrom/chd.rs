// SPDX-License-Identifier: GPL-3.0-or-later

//! CHD (MAME "Compressed Hunks of Data") CD image backend.
//!
//! A CD-ROM CHD stores the disc as compressed hunks of 2448-byte frames
//! (2352 bytes of sector data followed by 96 bytes of subcode), with each
//! track's frames padded to a multiple of 4 in the hunk stream, and one
//! `CHT2` (or older `CHTR`) metadata entry per track describing its type,
//! length, and pregap/postgap. A pregap whose PGTYPE carries chdman's `V`
//! prefix is stored in the frame stream and counted in FRAMES; other
//! pregaps and all postgaps are not stored and read back as zero fill,
//! but still occupy logical disc addresses, exactly as MAME lays the
//! disc out. CD-DA frames are stored with big-endian samples and are
//! swapped back to the little-endian disc byte order on read.

use super::{CdTrack, TrackKind, DATA_SECTOR_BYTES, RAW_SECTOR_BYTES};
use anyhow::{anyhow, bail, Context, Result};
use chd::metadata::{KnownMetadata, Metadata};
use chd::Chd;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

/// One CD frame slot in a CHD: sector data plus subcode.
const FRAME_BYTES: usize = RAW_SECTOR_BYTES + 96;
/// chdman pads each track's frames to this boundary in the hunk stream.
const TRACK_PADDING: u32 = 4;

pub(super) struct ChdImage {
    chd: Chd<BufReader<File>>,
    path: PathBuf,
    regions: Vec<Region>,
    frames_per_hunk: u32,
    /// Decompressed contents of hunk `cached_hunk`.
    hunk_buf: Vec<u8>,
    /// Scratch buffer for compressed hunk bytes, kept between reads.
    comp_buf: Vec<u8>,
    cached_hunk: Option<u32>,
}

impl std::fmt::Debug for ChdImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChdImage")
            .field("path", &self.path)
            .field("regions", &self.regions)
            .finish_non_exhaustive()
    }
}

/// A contiguous run of same-kind sectors on the logical disc: a track's
/// stored frames, or an unstored pregap/postgap.
#[derive(Debug)]
struct Region {
    disc_start: u32,
    sector_count: u32,
    kind: TrackKind,
    /// CHD frame holding the region's first sector; `None` for unstored
    /// gap sectors, which read as zero fill.
    chd_frame: Option<u32>,
}

/// One track's `CHT2`/`CHTR` metadata, as written by chdman.
#[derive(Debug)]
struct RawTrack {
    number: u8,
    kind: TrackKind,
    /// Frames stored in the CHD (includes the pregap only when stored).
    frames: u32,
    pregap: u32,
    /// PGTYPE carried chdman's `V` prefix: the pregap frames are in the
    /// file and counted in `frames`.
    pregap_stored: bool,
    postgap: u32,
}

impl ChdImage {
    /// Open a CHD CD image and lay its tracks out on the logical disc.
    pub(super) fn load(path: &Path) -> Result<(Self, Vec<CdTrack>, u32)> {
        let file =
            File::open(path).with_context(|| format!("opening CD image {}", path.display()))?;
        let mut chd = Chd::open(BufReader::new(file), None)
            .map_err(|e| anyhow!("{}: not a readable CHD image: {e}", path.display()))?;
        if chd.header().has_parent() {
            bail!(
                "{}: delta CHD with a parent is not supported; flatten it with chdman",
                path.display()
            );
        }
        let unit_bytes = chd.header().unit_bytes();
        let hunk_bytes = chd.header().hunk_size();
        // The zero-hunk check guards the frames_per_hunk divisions below;
        // the chd crate also rejects such headers, but do not rely on it.
        if unit_bytes as usize != FRAME_BYTES
            || hunk_bytes == 0
            || !(hunk_bytes as usize).is_multiple_of(FRAME_BYTES)
        {
            bail!(
                "{}: not a CD-ROM CHD (unit {unit_bytes} bytes, hunk {hunk_bytes} bytes; \
                 expected hunks of whole {FRAME_BYTES}-byte CD frames)",
                path.display()
            );
        }

        let entries: Vec<Metadata> = chd
            .metadata_refs()
            .try_into()
            .map_err(|e| anyhow!("{}: reading CHD metadata: {e}", path.display()))?;
        let mut raw_tracks = Vec::new();
        for entry in &entries {
            if entry.metatag == KnownMetadata::CdRomTrack as u32
                || entry.metatag == KnownMetadata::CdRomTrack2 as u32
            {
                let text = std::str::from_utf8(&entry.value)
                    .map(|s| s.trim_end_matches('\0'))
                    .map_err(|_| anyhow!("{}: CHD track metadata is not text", path.display()))?;
                raw_tracks.push(
                    parse_track_metadata(text)
                        .with_context(|| format!("{}: {text:?}", path.display()))?,
                );
            } else if entry.metatag == KnownMetadata::CdRomOld as u32
                || entry.metatag == KnownMetadata::GdRomOld as u32
                || entry.metatag == KnownMetadata::GdRomTrack as u32
            {
                bail!(
                    "{}: only CHT2/CHTR CD track metadata is supported; re-create the \
                     CHD with a current chdman",
                    path.display()
                );
            }
        }
        if raw_tracks.is_empty() {
            bail!("{}: no CD tracks in CHD metadata", path.display());
        }
        raw_tracks.sort_by_key(|t| t.number);
        for (i, track) in raw_tracks.iter().enumerate() {
            if usize::from(track.number) != i + 1 {
                bail!(
                    "{}: track numbers are not contiguous from 1 (found track {})",
                    path.display(),
                    track.number
                );
            }
        }

        // Lay the tracks out. The logical disc inserts unstored pregaps
        // and postgaps as addressable zero-fill regions; the CHD frame
        // stream advances by the stored frames plus chdman's padding.
        let mut regions = Vec::new();
        let mut tracks = Vec::with_capacity(raw_tracks.len());
        let mut disc = 0u32;
        let mut chd_frame = 0u32;
        let overflow = || anyhow!("{}: CHD track table overflows", path.display());
        for track in &raw_tracks {
            let region_start = disc;
            if track.pregap_stored && track.pregap > track.frames {
                bail!(
                    "{}: track {} pregap ({}) exceeds its stored frames ({})",
                    path.display(),
                    track.number,
                    track.pregap,
                    track.frames
                );
            }
            if !track.pregap_stored && track.pregap > 0 {
                regions.push(Region {
                    disc_start: disc,
                    sector_count: track.pregap,
                    kind: track.kind,
                    chd_frame: None,
                });
                disc = disc.checked_add(track.pregap).ok_or_else(overflow)?;
            }
            if track.frames > 0 {
                regions.push(Region {
                    disc_start: disc,
                    sector_count: track.frames,
                    kind: track.kind,
                    chd_frame: Some(chd_frame),
                });
                disc = disc.checked_add(track.frames).ok_or_else(overflow)?;
            }
            if track.postgap > 0 {
                regions.push(Region {
                    disc_start: disc,
                    sector_count: track.postgap,
                    kind: track.kind,
                    chd_frame: None,
                });
                disc = disc.checked_add(track.postgap).ok_or_else(overflow)?;
            }
            tracks.push(CdTrack {
                number: track.number,
                kind: track.kind,
                start_sector: region_start + track.pregap,
                sector_count: disc - (region_start + track.pregap),
            });
            let stored_end = chd_frame.checked_add(track.frames).ok_or_else(overflow)?;
            if u64::from(stored_end) > chd.header().unit_count() {
                bail!(
                    "{}: track {} claims CHD frames past the end of the image",
                    path.display(),
                    track.number
                );
            }
            let padding = (TRACK_PADDING - track.frames % TRACK_PADDING) % TRACK_PADDING;
            chd_frame = stored_end.checked_add(padding).ok_or_else(overflow)?;
        }

        let frames_per_hunk = hunk_bytes / FRAME_BYTES as u32;
        let hunk_buf = chd.get_hunksized_buffer();
        Ok((
            Self {
                chd,
                path: path.to_path_buf(),
                regions,
                frames_per_hunk,
                hunk_buf,
                comp_buf: Vec::new(),
                cached_hunk: None,
            },
            tracks,
            disc,
        ))
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    fn region_for_sector(&self, sector: u32) -> Option<&Region> {
        self.regions
            .iter()
            .find(|r| sector >= r.disc_start && sector < r.disc_start + r.sector_count)
    }

    pub(super) fn sector_kind(&self, sector: u32) -> Option<TrackKind> {
        self.region_for_sector(sector).map(|r| r.kind)
    }

    /// Read the stored payload of `sector`: `buf` is
    /// `kind.sector_bytes()` long, and comes back in disc byte order.
    pub(super) fn read_payload(&mut self, sector: u32, buf: &mut [u8]) -> Result<()> {
        let (frame, kind) = {
            let region = self
                .region_for_sector(sector)
                .with_context(|| format!("sector {sector} beyond end of disc"))?;
            match region.chd_frame {
                // Unstored pregap/postgap: zero fill (digital silence).
                None => {
                    buf.fill(0);
                    return Ok(());
                }
                Some(first) => (first + (sector - region.disc_start), region.kind),
            }
        };
        debug_assert!(matches!(buf.len(), DATA_SECTOR_BYTES | RAW_SECTOR_BYTES));
        self.load_hunk(frame / self.frames_per_hunk)?;
        let offset = (frame % self.frames_per_hunk) as usize * FRAME_BYTES;
        buf.copy_from_slice(&self.hunk_buf[offset..offset + buf.len()]);
        if kind == TrackKind::Audio {
            // CHD stores CD-DA samples big-endian; the disc byte order
            // (and every consumer here) is little-endian.
            for pair in buf.chunks_exact_mut(2) {
                pair.swap(0, 1);
            }
        }
        Ok(())
    }

    fn load_hunk(&mut self, hunk: u32) -> Result<()> {
        if self.cached_hunk == Some(hunk) {
            return Ok(());
        }
        self.cached_hunk = None;
        self.chd
            .hunk(hunk)
            .and_then(|mut h| h.read_hunk_in(&mut self.comp_buf, &mut self.hunk_buf))
            .map_err(|e| anyhow!("{}: reading CHD hunk {hunk}: {e}", self.path.display()))?;
        self.cached_hunk = Some(hunk);
        Ok(())
    }
}

/// Map a chdman track TYPE string to a track kind.
fn track_kind(name: &str, number: u8) -> Result<TrackKind> {
    match name {
        "MODE1" | "MODE1/2048" => Ok(TrackKind::Mode1_2048),
        "MODE1_RAW" | "MODE1/2352" => Ok(TrackKind::Mode1_2352),
        "AUDIO" => Ok(TrackKind::Audio),
        other => bail!("track {number} type {other:?} is not supported (MODE1, MODE1_RAW, AUDIO)"),
    }
}

/// Parse one `CHT2`/`CHTR` text entry, e.g.
/// `TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:2208 PREGAP:0
/// PGTYPE:MODE1 PGSUB:NONE POSTGAP:0` (`CHTR` stops after FRAMES).
fn parse_track_metadata(text: &str) -> Result<RawTrack> {
    let mut number = None;
    let mut kind_name = None;
    let mut frames = None;
    let mut pregap = 0u32;
    let mut pgtype = "";
    let mut postgap = 0u32;
    for word in text.split_whitespace() {
        let Some((key, value)) = word.split_once(':') else {
            continue;
        };
        match key {
            "TRACK" => number = Some(value.parse::<u8>().context("bad TRACK number")?),
            "TYPE" => kind_name = Some(value),
            "FRAMES" => frames = Some(value.parse::<u32>().context("bad FRAMES count")?),
            "PREGAP" => pregap = value.parse().context("bad PREGAP length")?,
            "PGTYPE" => pgtype = value,
            "POSTGAP" => postgap = value.parse().context("bad POSTGAP length")?,
            // SUBTYPE/PGSUB (subcode layout) do not matter here: the
            // subcode bytes of each frame are simply never read.
            _ => {}
        }
    }
    let number = number.context("track metadata without TRACK field")?;
    if number == 0 {
        bail!("track number 0 is invalid");
    }
    let kind_name = kind_name.context("track metadata without TYPE field")?;
    Ok(RawTrack {
        number,
        kind: track_kind(kind_name, number)?,
        frames: frames.context("track metadata without FRAMES field")?,
        pregap,
        pregap_stored: pregap > 0 && pgtype.starts_with('V'),
        postgap,
    })
}

#[cfg(test)]
mod tests {
    use super::super::CdImage;
    use super::*;
    use crate::cdrom::TrackKind;

    fn temp_path(name: &str) -> PathBuf {
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "copperline-chd-{}-{unique}-{name}",
            std::process::id()
        ))
    }

    /// Two frames per hunk, so a handful of frames spans several hunks.
    const HUNK_BYTES: u32 = 2 * FRAME_BYTES as u32;

    /// Write a minimal uncompressed CHD v5: 124-byte header, raw hunk
    /// map (one big-endian u32 per hunk, the hunk's file offset in
    /// hunk-size units), a metadata chain, and hunk-aligned data.
    fn write_chd_v5(
        path: &Path,
        hunk_bytes: u32,
        unit_bytes: u32,
        data: &[u8],
        metas: &[([u8; 4], Vec<u8>)],
    ) {
        // hunk_bytes == 0 writes a (broken) header that loading must
        // reject; avoid dividing by it here.
        let hunk_count = match hunk_bytes {
            0 => 0,
            _ => data.len().div_ceil(hunk_bytes as usize),
        };
        let map_offset = 124u64;
        let meta_offset = map_offset + 4 * hunk_count as u64;
        let metas_len: u64 = metas.iter().map(|(_, v)| 16 + v.len() as u64).sum();
        let data_start = match hunk_bytes {
            0 => meta_offset + metas_len,
            _ => (meta_offset + metas_len).div_ceil(u64::from(hunk_bytes)) * u64::from(hunk_bytes),
        };

        let mut out = Vec::new();
        out.extend_from_slice(b"MComprHD");
        out.extend_from_slice(&124u32.to_be_bytes());
        out.extend_from_slice(&5u32.to_be_bytes());
        out.extend_from_slice(&[0u8; 16]); // four codecs: none (uncompressed)
        out.extend_from_slice(&(data.len() as u64).to_be_bytes());
        out.extend_from_slice(&map_offset.to_be_bytes());
        out.extend_from_slice(&meta_offset.to_be_bytes());
        out.extend_from_slice(&hunk_bytes.to_be_bytes());
        out.extend_from_slice(&unit_bytes.to_be_bytes());
        out.extend_from_slice(&[0u8; 60]); // raw/combined/parent SHA1s
        assert_eq!(out.len(), 124);

        for hunk in 0..hunk_count as u64 {
            let entry = data_start / u64::from(hunk_bytes) + hunk;
            out.extend_from_slice(&(entry as u32).to_be_bytes());
        }
        let mut next = meta_offset;
        for (i, (tag, value)) in metas.iter().enumerate() {
            next += 16 + value.len() as u64;
            out.extend_from_slice(tag);
            out.extend_from_slice(&(0x01u32 << 24 | value.len() as u32).to_be_bytes());
            let next_field = if i + 1 == metas.len() { 0 } else { next };
            out.extend_from_slice(&next_field.to_be_bytes());
            out.extend_from_slice(value);
        }
        out.resize(data_start as usize, 0);
        out.extend_from_slice(data);
        out.resize(data_start as usize + hunk_count * hunk_bytes as usize, 0);
        std::fs::write(path, out).unwrap();
    }

    fn cht2(text: &str) -> ([u8; 4], Vec<u8>) {
        let mut value = text.as_bytes().to_vec();
        value.push(0);
        (*b"CHT2", value)
    }

    /// A 2448-byte frame slot with `payload` at the front of the sector
    /// data area and zeroed subcode.
    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut f = vec![0u8; FRAME_BYTES];
        f[..payload.len()].copy_from_slice(payload);
        f
    }

    /// Data track frames padded to chdman's 4-frame track boundary.
    fn track_frames(frames: &[Vec<u8>]) -> Vec<u8> {
        let mut out: Vec<u8> = frames.concat();
        let padding = (TRACK_PADDING as usize - frames.len() % TRACK_PADDING as usize)
            % TRACK_PADDING as usize;
        out.extend(std::iter::repeat_n(0u8, padding * FRAME_BYTES));
        out
    }

    /// Track 1: MODE1 data, 4 frames filled with the sector number.
    /// Track 2: AUDIO, 5 frames (2 of stored `VAUDIO` pregap), stored
    /// byte-swapped as chdman writes CD-DA.
    fn write_mixed_disc(path: &Path) {
        let mut data = Vec::new();
        data.extend(track_frames(
            &(0..4u8)
                .map(|s| frame(&[s; DATA_SECTOR_BYTES]))
                .collect::<Vec<_>>(),
        ));
        data.extend(track_frames(
            &(0..5u8)
                .map(|s| {
                    // Big-endian sample 0x00A0+s: LE on the wire is
                    // [0xA0+s, 0x00], stored swapped as [0x00, 0xA0+s].
                    let sample = [0x00, 0xA0 + s];
                    frame(&sample.repeat(RAW_SECTOR_BYTES / 2))
                })
                .collect::<Vec<_>>(),
        ));
        write_chd_v5(
            path,
            HUNK_BYTES,
            FRAME_BYTES as u32,
            &data,
            &[
                cht2(
                    "TRACK:1 TYPE:MODE1 SUBTYPE:NONE FRAMES:4 PREGAP:0 PGTYPE:MODE1 \
                     PGSUB:NONE POSTGAP:0",
                ),
                cht2(
                    "TRACK:2 TYPE:AUDIO SUBTYPE:NONE FRAMES:5 PREGAP:2 PGTYPE:VAUDIO \
                     PGSUB:NONE POSTGAP:0",
                ),
            ],
        );
    }

    #[test]
    fn mixed_disc_lays_out_stored_pregap_and_swaps_audio() {
        let path = temp_path("mixed.chd");
        write_mixed_disc(&path);
        let mut image = CdImage::load(&path).unwrap();
        assert_eq!(image.total_sectors(), 9);
        let tracks = image.tracks();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].kind, TrackKind::Mode1_2048);
        assert_eq!(tracks[0].start_sector, 0);
        assert_eq!(tracks[0].sector_count, 4);
        // INDEX 01 sits past the two stored pregap sectors.
        assert_eq!(tracks[1].kind, TrackKind::Audio);
        assert_eq!(tracks[1].start_sector, 6);
        assert_eq!(tracks[1].sector_count, 3);

        let mut data = [0u8; DATA_SECTOR_BYTES];
        for sector in 0..4 {
            image.read_data_sector(sector, &mut data).unwrap();
            assert!(data.iter().all(|&b| b == sector as u8), "sector {sector}");
        }
        assert!(image.read_data_sector(5, &mut data).is_err());
        assert!(!image.is_audio_sector(3));
        assert!(image.is_audio_sector(4));

        // Audio comes back in disc (little-endian) byte order, pregap
        // sectors included.
        let mut audio = [0u8; RAW_SECTOR_BYTES];
        for sector in 4..9u32 {
            image.read_audio_sector(sector, &mut audio).unwrap();
            let sample = [0xA0 + (sector - 4) as u8, 0x00];
            assert!(
                audio.chunks_exact(2).all(|c| c == sample),
                "sector {sector}"
            );
        }
        assert!(image.read_audio_sector(9, &mut audio).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn virtual_pregap_and_postgap_read_as_zero_fill() {
        let path = temp_path("virtualpg.chd");
        let mut data = Vec::new();
        data.extend(track_frames(
            &(0..4u8)
                .map(|s| frame(&[s; DATA_SECTOR_BYTES]))
                .collect::<Vec<_>>(),
        ));
        data.extend(track_frames(
            &(0..4u8)
                .map(|s| frame(&[0x00, 0xB0 + s].repeat(RAW_SECTOR_BYTES / 2)))
                .collect::<Vec<_>>(),
        ));
        write_chd_v5(
            &path,
            HUNK_BYTES,
            FRAME_BYTES as u32,
            &data,
            &[
                cht2(
                    "TRACK:1 TYPE:MODE1 SUBTYPE:NONE FRAMES:4 PREGAP:0 PGTYPE:MODE1 \
                     PGSUB:NONE POSTGAP:0",
                ),
                // No `V` prefix: the two pregap frames are not stored,
                // and neither is the postgap frame.
                cht2(
                    "TRACK:2 TYPE:AUDIO SUBTYPE:NONE FRAMES:4 PREGAP:2 PGTYPE:AUDIO \
                     PGSUB:NONE POSTGAP:1",
                ),
            ],
        );
        let mut image = CdImage::load(&path).unwrap();
        assert_eq!(image.total_sectors(), 11);
        assert_eq!(image.tracks()[1].start_sector, 6);
        assert_eq!(image.tracks()[1].sector_count, 5);

        let mut audio = [0u8; RAW_SECTOR_BYTES];
        for gap in [4, 5, 10] {
            assert!(image.is_audio_sector(gap), "sector {gap}");
            image.read_audio_sector(gap, &mut audio).unwrap();
            assert!(audio.iter().all(|&b| b == 0), "sector {gap}");
        }
        for sector in 6..10u32 {
            image.read_audio_sector(sector, &mut audio).unwrap();
            let sample = [0xB0 + (sector - 6) as u8, 0x00];
            assert!(
                audio.chunks_exact(2).all(|c| c == sample),
                "sector {sector}"
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn chtr_metadata_and_raw_data_track_skip_sector_header() {
        let path = temp_path("raw.chd");
        let mut raw = vec![0xEEu8; 16];
        raw.extend(std::iter::repeat_n(0x42u8, RAW_SECTOR_BYTES - 16));
        let data = track_frames(&[frame(&raw), frame(&raw)]);
        write_chd_v5(
            &path,
            HUNK_BYTES,
            FRAME_BYTES as u32,
            &data,
            &[(
                *b"CHTR",
                b"TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:2\0".to_vec(),
            )],
        );
        let mut image = CdImage::load(&path).unwrap();
        assert_eq!(image.tracks()[0].kind, TrackKind::Mode1_2352);
        assert_eq!(image.total_sectors(), 2);
        let mut data = [0u8; DATA_SECTOR_BYTES];
        image.read_data_sector(1, &mut data).unwrap();
        assert!(data.iter().all(|&b| b == 0x42));
        let mut full = [0u8; RAW_SECTOR_BYTES];
        image.read_raw_sector(0, &mut full).unwrap();
        assert_eq!(full.as_slice(), raw.as_slice());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn serde_reopens_chd_and_serves_same_sectors() {
        let path = temp_path("serde.chd");
        write_mixed_disc(&path);
        let mut image = CdImage::load(&path).unwrap();
        let encoded = bincode::serialize(&image).unwrap();
        let mut restored: CdImage = bincode::deserialize(&encoded).unwrap();
        assert_eq!(restored.total_sectors(), image.total_sectors());
        let mut a = [0u8; RAW_SECTOR_BYTES];
        let mut b = [0u8; RAW_SECTOR_BYTES];
        for sector in 4..9 {
            image.read_audio_sector(sector, &mut a).unwrap();
            restored.read_audio_sector(sector, &mut b).unwrap();
            assert_eq!(a, b, "sector {sector}");
        }

        let _ = std::fs::remove_file(&path);
        let err = bincode::deserialize::<CdImage>(&encoded)
            .expect_err("deserializing with the CHD gone must fail");
        assert!(err.to_string().contains("reopening CD image"));
    }

    #[test]
    fn non_cd_chd_is_rejected() {
        let path = temp_path("harddisk.chd");
        // A hard-disk-shaped CHD: 512-byte units, no CD track metadata.
        write_chd_v5(&path, 4096, 512, &[0u8; 8192], &[]);
        let err = CdImage::load(&path).unwrap_err();
        assert!(err.to_string().contains("not a CD-ROM CHD"), "{err:#}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn zero_hunk_size_is_rejected_not_divided_by() {
        let path = temp_path("zerohunk.chd");
        // The unit size claims CD frames but the hunk size is zero:
        // without the load-time guard, reading a sector would divide by
        // frames_per_hunk == 0.
        write_chd_v5(&path, 0, FRAME_BYTES as u32, &[], &[]);
        let err = CdImage::load(&path).unwrap_err();
        assert!(format!("{err:#}").contains("CHD"), "{err:#}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unsupported_track_type_is_rejected() {
        let path = temp_path("mode2.chd");
        let data = track_frames(&[frame(&[0u8; RAW_SECTOR_BYTES])]);
        write_chd_v5(
            &path,
            HUNK_BYTES,
            FRAME_BYTES as u32,
            &data,
            &[cht2(
                "TRACK:1 TYPE:MODE2_RAW SUBTYPE:NONE FRAMES:1 PREGAP:0 PGTYPE:MODE1 \
                 PGSUB:NONE POSTGAP:0",
            )],
        );
        let err = CdImage::load(&path).unwrap_err();
        assert!(format!("{err:#}").contains("not supported"), "{err:#}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn track_claiming_frames_past_the_image_is_rejected() {
        let path = temp_path("overrun.chd");
        let data = track_frames(&[frame(&[0u8; DATA_SECTOR_BYTES])]);
        write_chd_v5(
            &path,
            HUNK_BYTES,
            FRAME_BYTES as u32,
            &data,
            &[cht2(
                "TRACK:1 TYPE:MODE1 SUBTYPE:NONE FRAMES:500 PREGAP:0 PGTYPE:MODE1 \
                 PGSUB:NONE POSTGAP:0",
            )],
        );
        let err = CdImage::load(&path).unwrap_err();
        assert!(
            err.to_string().contains("past the end of the image"),
            "{err:#}"
        );
        let _ = std::fs::remove_file(&path);
    }
}
