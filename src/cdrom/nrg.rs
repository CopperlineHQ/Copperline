// SPDX-License-Identifier: GPL-3.0-or-later

//! Nero NRG CD image backend.
//!
//! NRG stores sector data first and a chunked descriptor at the end of the
//! file. Pre-5.5 images carry a `NERO` footer with a 32-bit descriptor offset;
//! newer images carry `NER5` and a 64-bit offset. DAO images describe track
//! offsets through `CUES`/`DAOI` or `CUEX`/`DAOX`, while TAO images use
//! `ETNF`/`ETN2`. Once decoded, both layouts are ordinary byte extents over
//! the NRG file and therefore share `CdImage`'s plain-file backend.

use super::{
    BinBackend, CdImage, CdTrack, Extent, FileFormat, Source, Storage, TrackKind,
    DATA_SECTOR_BYTES, RAW_SECTOR_BYTES,
};
use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const NEW_FOOTER_BYTES: u64 = 12;
const OLD_FOOTER_BYTES: u64 = 8;
const CHUNK_HEADER_BYTES: u64 = 8;
const DAO_HEADER_BYTES: usize = 22;
const MAX_TRACKS: usize = 99;
const MAX_DISC_SECTORS: u32 = 100 * 60 * 75;
/// CUE permits indices 00 through 99 on every track. They are uncommon past
/// INDEX 01, but bounding for the complete addressable set avoids rejecting a
/// valid footer while still preventing a forged chunk from driving allocation.
const MAX_CUE_BYTES: usize = (MAX_TRACKS * 100 + 2) * 8;
const MAX_DAO_BYTES: usize = DAO_HEADER_BYTES + MAX_TRACKS * 42;
const MAX_ETN_BYTES: usize = MAX_TRACKS * 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Version {
    Old,
    New,
}

impl Version {
    fn footer_bytes(self) -> u64 {
        match self {
            Version::Old => OLD_FOOTER_BYTES,
            Version::New => NEW_FOOTER_BYTES,
        }
    }
}

#[derive(Default)]
struct Chunks {
    cue: Option<(Version, Vec<u8>)>,
    dao: Option<(Version, Vec<u8>)>,
    etn: Option<(Version, Vec<u8>)>,
}

#[derive(Debug, Clone, Copy)]
struct CueEntry {
    track: u8,
    index: u8,
    lba: i32,
}

#[derive(Debug, Clone, Copy)]
struct DaoTrack {
    kind: TrackKind,
    sector_bytes: u64,
    pregap_offset: u64,
    start_offset: u64,
    end_offset: u64,
}

pub(super) fn load(path: &Path) -> Result<CdImage> {
    let mut file =
        File::open(path).with_context(|| format!("opening NRG image {}", path.display()))?;
    let file_len = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    let (version, descriptor_start) = read_footer(&mut file, file_len, path)?;
    let descriptor_end = file_len - version.footer_bytes();
    if descriptor_start >= descriptor_end {
        bail!(
            "{}: NRG descriptor offset {descriptor_start} is not before its footer",
            path.display()
        );
    }

    let chunks = read_chunks(&mut file, descriptor_start, descriptor_end, version, path)?;
    let (tracks, extents, total_sectors) = match (chunks.cue, chunks.dao, chunks.etn) {
        (Some((cue_version, cue)), Some((dao_version, dao)), None) => {
            if cue_version != version || dao_version != version {
                bail!(
                    "{}: NRG footer and DAO chunk generations disagree",
                    path.display()
                );
            }
            parse_dao_layout(&cue, &dao, version, descriptor_start, path)?
        }
        (None, None, Some((etn_version, etn))) => {
            if etn_version != version {
                bail!(
                    "{}: NRG footer and ETN chunk generations disagree",
                    path.display()
                );
            }
            parse_tao_layout(&etn, version, descriptor_start, path)?
        }
        (Some(_), None, None) | (None, Some(_), None) => bail!(
            "{}: NRG DAO image needs both a CUE and a DAO chunk",
            path.display()
        ),
        (None, None, None) => bail!("{}: NRG descriptor contains no CD tracks", path.display()),
        _ => bail!(
            "{}: NRG descriptor mixes DAO and TAO track layouts",
            path.display()
        ),
    };

    let source = Source::open(path, FileFormat::Binary)?;
    Ok(CdImage {
        tracks,
        total_sectors,
        backend: super::Backend::Bin(BinBackend {
            sources: vec![source],
            extents,
        }),
    })
}

fn read_footer(file: &mut File, file_len: u64, path: &Path) -> Result<(Version, u64)> {
    if file_len < NEW_FOOTER_BYTES {
        bail!(
            "{}: file is too short to contain an NRG footer",
            path.display()
        );
    }
    file.seek(SeekFrom::End(-(NEW_FOOTER_BYTES as i64)))?;
    let mut tail = [0u8; NEW_FOOTER_BYTES as usize];
    file.read_exact(&mut tail)?;
    if &tail[..4] == b"NER5" {
        return Ok((Version::New, be_u64(&tail[4..12])));
    }
    if &tail[4..8] == b"NERO" {
        return Ok((Version::Old, u64::from(be_u32(&tail[8..12]))));
    }
    bail!("{}: missing NERO/NER5 footer", path.display())
}

fn read_chunks(
    file: &mut File,
    descriptor_start: u64,
    descriptor_end: u64,
    footer_version: Version,
    path: &Path,
) -> Result<Chunks> {
    let mut chunks = Chunks::default();
    let mut pos = descriptor_start;
    let mut saw_end = false;
    while pos < descriptor_end {
        if descriptor_end - pos < CHUNK_HEADER_BYTES {
            bail!("{}: truncated NRG chunk header", path.display());
        }
        file.seek(SeekFrom::Start(pos))?;
        let mut header = [0u8; CHUNK_HEADER_BYTES as usize];
        file.read_exact(&mut header)?;
        let id = &header[..4];
        let length = u64::from(be_u32(&header[4..8]));
        let payload_start = pos + CHUNK_HEADER_BYTES;
        let payload_end = payload_start
            .checked_add(length)
            .with_context(|| format!("{}: NRG chunk length overflows", path.display()))?;
        if payload_end > descriptor_end {
            bail!(
                "{}: truncated NRG {} chunk",
                path.display(),
                String::from_utf8_lossy(id)
            );
        }

        match id {
            b"CUEX" => set_chunk(
                &mut chunks.cue,
                Version::New,
                read_payload(file, length, MAX_CUE_BYTES, "CUEX", path)?,
                "CUE",
                path,
            )?,
            b"CUES" => set_chunk(
                &mut chunks.cue,
                Version::Old,
                read_payload(file, length, MAX_CUE_BYTES, "CUES", path)?,
                "CUE",
                path,
            )?,
            b"DAOX" => set_chunk(
                &mut chunks.dao,
                Version::New,
                read_payload(file, length, MAX_DAO_BYTES, "DAOX", path)?,
                "DAO",
                path,
            )?,
            b"DAOI" => set_chunk(
                &mut chunks.dao,
                Version::Old,
                read_payload(file, length, MAX_DAO_BYTES, "DAOI", path)?,
                "DAO",
                path,
            )?,
            b"ETN2" => set_chunk(
                &mut chunks.etn,
                Version::New,
                read_payload(file, length, MAX_ETN_BYTES, "ETN2", path)?,
                "ETN",
                path,
            )?,
            b"ETNF" => set_chunk(
                &mut chunks.etn,
                Version::Old,
                read_payload(file, length, MAX_ETN_BYTES, "ETNF", path)?,
                "ETN",
                path,
            )?,
            b"END!" => {
                if length != 0 {
                    bail!("{}: END! NRG chunk has a payload", path.display());
                }
                saw_end = true;
                pos = payload_end;
                break;
            }
            _ => file.seek(SeekFrom::Start(payload_end)).map(|_| ())?,
        }
        pos = payload_end;
    }

    if !saw_end {
        bail!("{}: NRG descriptor has no END! chunk", path.display());
    }
    if pos != descriptor_end {
        bail!("{}: data follows the NRG END! chunk", path.display());
    }
    if footer_version == Version::New
        && (chunks.cue.as_ref().is_some_and(|(v, _)| *v == Version::Old)
            || chunks.dao.as_ref().is_some_and(|(v, _)| *v == Version::Old)
            || chunks.etn.as_ref().is_some_and(|(v, _)| *v == Version::Old))
    {
        bail!(
            "{}: NER5 footer contains old-format track chunks",
            path.display()
        );
    }
    Ok(chunks)
}

fn read_payload(
    file: &mut File,
    length: u64,
    maximum: usize,
    name: &str,
    path: &Path,
) -> Result<Vec<u8>> {
    let length = usize::try_from(length)
        .with_context(|| format!("{}: {name} NRG chunk is too large", path.display()))?;
    if length > maximum {
        bail!(
            "{}: {name} NRG chunk is too large ({length} bytes)",
            path.display()
        );
    }
    let mut payload = vec![0u8; length];
    file.read_exact(&mut payload)?;
    Ok(payload)
}

fn set_chunk(
    slot: &mut Option<(Version, Vec<u8>)>,
    version: Version,
    payload: Vec<u8>,
    name: &str,
    path: &Path,
) -> Result<()> {
    if slot.is_some() {
        bail!(
            "{}: multi-session NRG images are not supported (repeated {name} chunk)",
            path.display()
        );
    }
    *slot = Some((version, payload));
    Ok(())
}

fn parse_dao_layout(
    cue: &[u8],
    dao: &[u8],
    version: Version,
    descriptor_start: u64,
    path: &Path,
) -> Result<(Vec<CdTrack>, Vec<Extent>, u32)> {
    let cue = parse_cue(cue, version, path)?;
    let entry_bytes = match version {
        Version::Old => 30,
        Version::New => 42,
    };
    if dao.len() < DAO_HEADER_BYTES || !(dao.len() - DAO_HEADER_BYTES).is_multiple_of(entry_bytes) {
        bail!("{}: malformed NRG DAO chunk length", path.display());
    }
    let count = (dao.len() - DAO_HEADER_BYTES) / entry_bytes;
    if count == 0 || count > MAX_TRACKS {
        bail!("{}: NRG DAO track count {count} is invalid", path.display());
    }
    let first_track = dao[20];
    let last_track = dao[21];
    let header_count = last_track
        .checked_sub(first_track)
        .map(|n| usize::from(n) + 1)
        .unwrap_or(0);
    if first_track == 0 || last_track > MAX_TRACKS as u8 || header_count != count {
        bail!(
            "{}: NRG DAO header track range {first_track}..{last_track} does not match {count} entries",
            path.display()
        );
    }

    let mut raw_tracks = Vec::with_capacity(count);
    for i in 0..count {
        let entry =
            &dao[DAO_HEADER_BYTES + i * entry_bytes..DAO_HEADER_BYTES + (i + 1) * entry_bytes];
        let sector_size = u64::from(be_u16(&entry[12..14]));
        let mode = entry[14];
        let kind = decode_mode(mode, sector_size, path)?;
        let (pregap_offset, start_offset, end_offset) = match version {
            Version::Old => (
                u64::from(be_u32(&entry[18..22])),
                u64::from(be_u32(&entry[22..26])),
                u64::from(be_u32(&entry[26..30])),
            ),
            Version::New => (
                be_u64(&entry[18..26]),
                be_u64(&entry[26..34]),
                be_u64(&entry[34..42]),
            ),
        };
        if pregap_offset > start_offset || start_offset > end_offset {
            bail!(
                "{}: NRG track {} offsets are not monotonic",
                path.display(),
                first_track + i as u8
            );
        }
        if end_offset > descriptor_start {
            bail!(
                "{}: NRG track {} runs into the descriptor",
                path.display(),
                first_track + i as u8
            );
        }
        if !(start_offset - pregap_offset).is_multiple_of(sector_size)
            || !(end_offset - start_offset).is_multiple_of(sector_size)
        {
            bail!(
                "{}: NRG track {} does not end on a sector boundary",
                path.display(),
                first_track + i as u8
            );
        }
        raw_tracks.push(DaoTrack {
            kind,
            sector_bytes: sector_size,
            pregap_offset,
            start_offset,
            end_offset,
        });
    }

    let mut tracks = Vec::with_capacity(count);
    let mut extents = Vec::with_capacity(count + 1);
    let mut cursor = 0u32;
    for (i, raw) in raw_tracks.iter().enumerate() {
        let number = first_track + i as u8;
        let index1 = cue_lba(&cue, number, 1)
            .with_context(|| format!("{}: NRG track {number} has no INDEX 01", path.display()))?;
        if index1 < 0 {
            bail!(
                "{}: NRG track {number} INDEX 01 is negative",
                path.display()
            );
        }
        let pregap_sectors = u32::try_from(
            (raw.start_offset - raw.pregap_offset) / raw.sector_bytes,
        )
        .with_context(|| format!("{}: NRG track {number} pregap is too large", path.display()))?;
        let data_sectors = u32::try_from((raw.end_offset - raw.start_offset) / raw.sector_bytes)
            .with_context(|| format!("{}: NRG track {number} is too large", path.display()))?;
        if data_sectors == 0 {
            bail!("{}: NRG track {number} is empty", path.display());
        }
        let pregap_sectors_i32 = i32::try_from(pregap_sectors).with_context(|| {
            format!("{}: NRG track {number} pregap is too large", path.display())
        })?;
        let data_sectors_i32 = i32::try_from(data_sectors)
            .with_context(|| format!("{}: NRG track {number} is too large", path.display()))?;
        let derived_index0 = index1
            .checked_sub(pregap_sectors_i32)
            .with_context(|| format!("{}: NRG track {number} pregap underflows", path.display()))?;
        let index0 = cue_lba(&cue, number, 0).unwrap_or(derived_index0);
        if index0 != derived_index0 {
            bail!(
                "{}: NRG track {number} CUE pregap does not match its DAO offsets",
                path.display()
            );
        }
        let logical_end = index1
            .checked_add(data_sectors_i32)
            .with_context(|| format!("{}: NRG track {number} address overflows", path.display()))?;
        if logical_end <= 0 || logical_end as u32 > MAX_DISC_SECTORS {
            bail!(
                "{}: NRG track {number} lies outside a CD address space",
                path.display()
            );
        }

        let extent_start = index0.max(0) as u32;
        let clipped = u64::try_from(extent_start as i64 - index0 as i64).unwrap();
        let source_offset = raw
            .pregap_offset
            .checked_add(clipped * raw.sector_bytes)
            .with_context(|| format!("{}: NRG track {number} offset overflows", path.display()))?;
        let extent_count = logical_end as u32 - extent_start;
        push_source_extent(
            &mut extents,
            &mut cursor,
            extent_start,
            extent_count,
            raw.kind,
            source_offset,
            path,
        )?;
        tracks.push(CdTrack {
            number,
            kind: raw.kind,
            start_sector: index1 as u32,
            sector_count: data_sectors,
        });
    }

    let leadout = cue_lba(&cue, 0xAA, 1).unwrap_or(cursor as i32);
    if leadout < cursor as i32 || leadout < 0 || leadout as u32 > MAX_DISC_SECTORS {
        bail!("{}: NRG lead-out address is invalid", path.display());
    }
    if leadout as u32 > cursor {
        let kind = tracks.last().unwrap().kind;
        extents.push(Extent {
            disc_start: cursor,
            sector_count: leadout as u32 - cursor,
            kind,
            storage: Storage::Gap,
        });
    }
    Ok((tracks, extents, leadout as u32))
}

fn parse_tao_layout(
    etn: &[u8],
    version: Version,
    descriptor_start: u64,
    path: &Path,
) -> Result<(Vec<CdTrack>, Vec<Extent>, u32)> {
    let entry_bytes = match version {
        Version::Old => 20,
        Version::New => 32,
    };
    if etn.is_empty() || !etn.len().is_multiple_of(entry_bytes) {
        bail!("{}: malformed NRG ETN chunk length", path.display());
    }
    let count = etn.len() / entry_bytes;
    if count > MAX_TRACKS {
        bail!("{}: NRG ETN track count {count} is invalid", path.display());
    }

    let mut tracks = Vec::with_capacity(count);
    let mut extents = Vec::with_capacity(count * 2);
    let mut cursor = 0u32;
    for i in 0..count {
        let entry = &etn[i * entry_bytes..(i + 1) * entry_bytes];
        let (offset, size, mode, raw_start) = match version {
            Version::Old => (
                u64::from(be_u32(&entry[0..4])),
                u64::from(be_u32(&entry[4..8])),
                be_u32(&entry[8..12]),
                be_u32(&entry[12..16]),
            ),
            Version::New => (
                be_u64(&entry[0..8]),
                be_u64(&entry[8..16]),
                be_u32(&entry[16..20]),
                be_u32(&entry[20..24]),
            ),
        };
        let mode = u8::try_from(mode)
            .with_context(|| format!("{}: NRG track {} mode is invalid", path.display(), i + 1))?;
        let (kind, sector_bytes) = tao_mode(mode, path)?;
        if !size.is_multiple_of(sector_bytes) {
            bail!(
                "{}: NRG track {} does not end on a sector boundary",
                path.display(),
                i + 1
            );
        }
        let end_offset = offset
            .checked_add(size)
            .with_context(|| format!("{}: NRG track {} offset overflows", path.display(), i + 1))?;
        if end_offset > descriptor_start {
            bail!(
                "{}: NRG track {} runs into the descriptor",
                path.display(),
                i + 1
            );
        }
        let sector_count = u32::try_from(size / sector_bytes)
            .with_context(|| format!("{}: NRG track {} is too large", path.display(), i + 1))?;
        if sector_count == 0 {
            bail!("{}: NRG track {} is empty", path.display(), i + 1);
        }
        // ETN start_lsn omits the standard 150-sector pregap inserted
        // before every track after the first.
        let start_sector = raw_start.checked_add(i as u32 * 150).with_context(|| {
            format!("{}: NRG track {} address overflows", path.display(), i + 1)
        })?;
        let end_sector = start_sector.checked_add(sector_count).with_context(|| {
            format!("{}: NRG track {} address overflows", path.display(), i + 1)
        })?;
        if end_sector > MAX_DISC_SECTORS {
            bail!(
                "{}: NRG track {} lies outside a CD address space",
                path.display(),
                i + 1
            );
        }
        push_source_extent(
            &mut extents,
            &mut cursor,
            start_sector,
            sector_count,
            kind,
            offset,
            path,
        )?;
        tracks.push(CdTrack {
            number: (i + 1) as u8,
            kind,
            start_sector,
            sector_count,
        });
    }
    Ok((tracks, extents, cursor))
}

fn push_source_extent(
    extents: &mut Vec<Extent>,
    cursor: &mut u32,
    start: u32,
    count: u32,
    kind: TrackKind,
    byte_offset: u64,
    path: &Path,
) -> Result<()> {
    if start < *cursor {
        bail!("{}: NRG track address ranges overlap", path.display());
    }
    if start > *cursor {
        extents.push(Extent {
            disc_start: *cursor,
            sector_count: start - *cursor,
            kind,
            storage: Storage::Gap,
        });
    }
    extents.push(Extent {
        disc_start: start,
        sector_count: count,
        kind,
        storage: Storage::Source {
            source: 0,
            byte_offset,
        },
    });
    *cursor = start
        .checked_add(count)
        .with_context(|| format!("{}: NRG disc address overflows", path.display()))?;
    Ok(())
}

fn parse_cue(data: &[u8], version: Version, path: &Path) -> Result<Vec<CueEntry>> {
    if data.is_empty() || !data.len().is_multiple_of(8) {
        bail!("{}: malformed NRG CUE chunk length", path.display());
    }
    let mut entries = Vec::with_capacity(data.len() / 8);
    for raw in data.chunks_exact(8) {
        let track = if raw[1] == 0xAA {
            0xAA
        } else {
            from_bcd(raw[1], "track", path)?
        };
        let index = from_bcd(raw[2], "index", path)?;
        let lba = match version {
            Version::New => i32::from_be_bytes(raw[4..8].try_into().unwrap()),
            Version::Old => {
                let (minutes, seconds, frames) = (raw[5], raw[6], raw[7]);
                if seconds >= 60 || frames >= 75 {
                    bail!("{}: invalid MSF address in NRG CUES chunk", path.display());
                }
                let mut lba =
                    (i32::from(minutes) * 60 + i32::from(seconds)) * 75 + i32::from(frames) - 150;
                if minutes >= 90 {
                    lba -= 450_000;
                }
                lba
            }
        };
        if entries
            .iter()
            .any(|entry: &CueEntry| entry.track == track && entry.index == index)
        {
            bail!(
                "{}: duplicate NRG CUE track {track:02X} index {index}",
                path.display()
            );
        }
        entries.push(CueEntry { track, index, lba });
    }
    Ok(entries)
}

fn cue_lba(entries: &[CueEntry], track: u8, index: u8) -> Option<i32> {
    entries
        .iter()
        .find(|entry| entry.track == track && entry.index == index)
        .map(|entry| entry.lba)
}

fn decode_mode(mode: u8, sector_bytes: u64, path: &Path) -> Result<TrackKind> {
    match (mode, sector_bytes) {
        (0x00, n) if n == DATA_SECTOR_BYTES as u64 => Ok(TrackKind::Mode1_2048),
        (0x05, n) if n == RAW_SECTOR_BYTES as u64 => Ok(TrackKind::Mode1_2352),
        (0x07, n) if n == RAW_SECTOR_BYTES as u64 => Ok(TrackKind::Audio),
        (0x0F..=0x11, 2448) => bail!(
            "{}: NRG sectors with stored subchannel data are not supported",
            path.display()
        ),
        (0x02 | 0x03 | 0x06 | 0x11, _) => {
            bail!("{}: Mode 2 NRG tracks are not supported", path.display())
        }
        _ => bail!(
            "{}: unsupported NRG track mode 0x{mode:02X} with {sector_bytes}-byte sectors",
            path.display()
        ),
    }
}

fn tao_mode(mode: u8, path: &Path) -> Result<(TrackKind, u64)> {
    match mode {
        0x00 => Ok((TrackKind::Mode1_2048, DATA_SECTOR_BYTES as u64)),
        0x05 => Ok((TrackKind::Mode1_2352, RAW_SECTOR_BYTES as u64)),
        0x07 => Ok((TrackKind::Audio, RAW_SECTOR_BYTES as u64)),
        0x02 | 0x03 | 0x06 => bail!("{}: Mode 2 NRG tracks are not supported", path.display()),
        _ => bail!(
            "{}: unsupported NRG track mode 0x{mode:02X}",
            path.display()
        ),
    }
}

fn from_bcd(value: u8, field: &str, path: &Path) -> Result<u8> {
    let hi = value >> 4;
    let lo = value & 0x0F;
    if hi > 9 || lo > 9 {
        bail!("{}: invalid BCD {field} in NRG CUE chunk", path.display());
    }
    Ok(hi * 10 + lo)
}

fn be_u16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes(bytes.try_into().unwrap())
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().unwrap())
}

fn be_u64(bytes: &[u8]) -> u64 {
    u64::from_be_bytes(bytes.try_into().unwrap())
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
            "copperline-nrg-{}-{unique}-{name}",
            std::process::id()
        ))
    }

    fn chunk(id: &[u8; 4], data: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(id);
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(data);
    }

    fn cue_entry(track: u8, index: u8, lba: i32, old: bool) -> [u8; 8] {
        let mut entry = [0u8; 8];
        entry[0] = if track == 1 { 0x41 } else { 0x01 };
        entry[1] = track;
        entry[2] = index;
        if old {
            let absolute = lba + 150;
            entry[5] = (absolute / (60 * 75)) as u8;
            entry[6] = ((absolute / 75) % 60) as u8;
            entry[7] = (absolute % 75) as u8;
        } else {
            entry[4..8].copy_from_slice(&lba.to_be_bytes());
        }
        entry
    }

    fn dao_entry(sector_bytes: u16, mode: u8, offsets: [u64; 3], old: bool) -> Vec<u8> {
        let mut entry = vec![0u8; if old { 30 } else { 42 }];
        entry[12..14].copy_from_slice(&sector_bytes.to_be_bytes());
        entry[14] = mode;
        entry[17] = 1;
        if old {
            for (i, offset) in offsets.into_iter().enumerate() {
                entry[18 + i * 4..22 + i * 4].copy_from_slice(&(offset as u32).to_be_bytes());
            }
        } else {
            for (i, offset) in offsets.into_iter().enumerate() {
                entry[18 + i * 8..26 + i * 8].copy_from_slice(&offset.to_be_bytes());
            }
        }
        entry
    }

    fn write_dao_image(path: &Path, old: bool) {
        let mut image = Vec::new();
        // Track 1 has a two-sector stored lead-in followed by three data
        // sectors. Track 2 has a one-sector stored pregap and two audio
        // sectors.
        image.extend(vec![0xEE; 2 * DATA_SECTOR_BYTES]);
        for value in [0x10, 0x11, 0x12] {
            image.extend(vec![value; DATA_SECTOR_BYTES]);
        }
        image.extend(vec![0xA0; RAW_SECTOR_BYTES]);
        image.extend(vec![0xA1; RAW_SECTOR_BYTES]);
        image.extend(vec![0xA2; RAW_SECTOR_BYTES]);
        let t1 = [
            0,
            2 * DATA_SECTOR_BYTES as u64,
            5 * DATA_SECTOR_BYTES as u64,
        ];
        let t2 = [t1[2], t1[2] + RAW_SECTOR_BYTES as u64, image.len() as u64];

        let mut descriptor = Vec::new();
        let cue = [
            cue_entry(0, 0, -2, old),
            cue_entry(1, 0, -2, old),
            cue_entry(1, 1, 0, old),
            cue_entry(2, 0, 3, old),
            cue_entry(2, 1, 4, old),
            cue_entry(0xAA, 1, 6, old),
        ]
        .concat();
        chunk(if old { b"CUES" } else { b"CUEX" }, &cue, &mut descriptor);
        let mut dao = vec![0u8; DAO_HEADER_BYTES];
        dao[20] = 1;
        dao[21] = 2;
        dao.extend(dao_entry(DATA_SECTOR_BYTES as u16, 0, t1, old));
        dao.extend(dao_entry(RAW_SECTOR_BYTES as u16, 7, t2, old));
        chunk(if old { b"DAOI" } else { b"DAOX" }, &dao, &mut descriptor);
        chunk(b"END!", &[], &mut descriptor);

        let descriptor_start = image.len() as u64;
        image.extend(descriptor);
        if old {
            image.extend_from_slice(b"NERO");
            image.extend_from_slice(&(descriptor_start as u32).to_be_bytes());
        } else {
            image.extend_from_slice(b"NER5");
            image.extend_from_slice(&descriptor_start.to_be_bytes());
        }
        File::create(path).unwrap().write_all(&image).unwrap();
    }

    fn assert_mixed_dao(path: &Path) {
        let mut image = CdImage::load(path).unwrap();
        assert_eq!(image.total_sectors(), 6);
        assert_eq!(image.tracks().len(), 2);
        assert_eq!(image.tracks()[0].start_sector, 0);
        assert_eq!(image.tracks()[0].sector_count, 3);
        assert_eq!(image.tracks()[1].start_sector, 4);
        assert_eq!(image.tracks()[1].sector_count, 2);

        let mut data = [0u8; DATA_SECTOR_BYTES];
        for (sector, value) in [(0, 0x10), (1, 0x11), (2, 0x12)] {
            image.read_data_sector(sector, &mut data).unwrap();
            assert!(data.iter().all(|&byte| byte == value));
        }
        let mut audio = [0u8; RAW_SECTOR_BYTES];
        for (sector, value) in [(3, 0xA0), (4, 0xA1), (5, 0xA2)] {
            image.read_audio_sector(sector, &mut audio).unwrap();
            assert!(audio.iter().all(|&byte| byte == value));
        }
    }

    #[test]
    fn ner5_daox_maps_stored_pregaps_and_mixed_track_sizes() {
        let path = temp_path("mixed-new.nrg");
        write_dao_image(&path, false);
        assert_mixed_dao(&path);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn nero_daoi_decodes_msf_cue_addresses_and_32_bit_offsets() {
        let path = temp_path("mixed-old.nrg");
        write_dao_image(&path, true);
        assert_mixed_dao(&path);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn serde_reopens_nrg_extents_and_serves_the_same_sectors() {
        let path = temp_path("serde.nrg");
        write_dao_image(&path, false);
        let mut image = CdImage::load(&path).unwrap();
        let encoded = bincode::serialize(&image).unwrap();
        let mut restored: CdImage = bincode::deserialize(&encoded).unwrap();
        assert_eq!(restored.total_sectors(), image.total_sectors());
        let mut expected = [0u8; RAW_SECTOR_BYTES];
        let mut actual = [0u8; RAW_SECTOR_BYTES];
        image.read_audio_sector(4, &mut expected).unwrap();
        restored.read_audio_sector(4, &mut actual).unwrap();
        assert_eq!(actual, expected);

        let _ = std::fs::remove_file(&path);
        let err = bincode::deserialize::<CdImage>(&encoded)
            .expect_err("deserializing with the NRG gone must fail");
        assert!(err.to_string().contains("reopening CD image"));
    }

    #[test]
    fn etn2_tao_inserts_virtual_intertrack_pregap() {
        let path = temp_path("tao.nrg");
        let mut image = vec![0x31; 2 * DATA_SECTOR_BYTES];
        let audio_offset = image.len() as u64;
        image.extend(vec![0x72; 2 * RAW_SECTOR_BYTES]);
        let descriptor_start = image.len() as u64;

        let mut etn = Vec::new();
        for (offset, size, mode, start) in [
            (0, (2 * DATA_SECTOR_BYTES) as u64, 0u32, 0u32),
            (audio_offset, (2 * RAW_SECTOR_BYTES) as u64, 7u32, 2u32),
        ] {
            etn.extend_from_slice(&offset.to_be_bytes());
            etn.extend_from_slice(&size.to_be_bytes());
            etn.extend_from_slice(&mode.to_be_bytes());
            etn.extend_from_slice(&start.to_be_bytes());
            etn.extend_from_slice(&0u64.to_be_bytes());
        }
        chunk(b"ETN2", &etn, &mut image);
        chunk(b"END!", &[], &mut image);
        image.extend_from_slice(b"NER5");
        image.extend_from_slice(&descriptor_start.to_be_bytes());
        File::create(&path).unwrap().write_all(&image).unwrap();

        let mut image = CdImage::load(&path).unwrap();
        assert_eq!(image.tracks()[0].start_sector, 0);
        assert_eq!(image.tracks()[1].start_sector, 152);
        assert_eq!(image.total_sectors(), 154);
        let mut audio = [1u8; RAW_SECTOR_BYTES];
        image.read_audio_sector(2, &mut audio).unwrap();
        assert!(audio.iter().all(|&byte| byte == 0));
        image.read_audio_sector(152, &mut audio).unwrap();
        assert!(audio.iter().all(|&byte| byte == 0x72));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn etnf_tao_uses_32_bit_offsets() {
        let path = temp_path("tao-old.nrg");
        let mut image = vec![0x51; 2 * DATA_SECTOR_BYTES];
        let descriptor_start = image.len() as u64;
        let mut etn = Vec::new();
        etn.extend_from_slice(&0u32.to_be_bytes());
        etn.extend_from_slice(&(2 * DATA_SECTOR_BYTES as u32).to_be_bytes());
        etn.extend_from_slice(&0u32.to_be_bytes());
        etn.extend_from_slice(&0u32.to_be_bytes());
        etn.extend_from_slice(&0u32.to_be_bytes());
        chunk(b"ETNF", &etn, &mut image);
        chunk(b"END!", &[], &mut image);
        image.extend_from_slice(b"NERO");
        image.extend_from_slice(&(descriptor_start as u32).to_be_bytes());
        File::create(&path).unwrap().write_all(&image).unwrap();

        let mut image = CdImage::load(&path).unwrap();
        assert_eq!(image.total_sectors(), 2);
        let mut data = [0u8; DATA_SECTOR_BYTES];
        image.read_data_sector(1, &mut data).unwrap();
        assert!(data.iter().all(|&byte| byte == 0x51));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn descriptor_chunk_cannot_run_into_the_footer() {
        let path = temp_path("truncated.nrg");
        let mut image = Vec::new();
        image.extend_from_slice(b"CUEX");
        image.extend_from_slice(&100u32.to_be_bytes());
        image.extend_from_slice(b"NER5");
        image.extend_from_slice(&0u64.to_be_bytes());
        File::create(&path).unwrap().write_all(&image).unwrap();
        let err = CdImage::load(&path).unwrap_err();
        assert!(err.to_string().contains("truncated NRG CUEX"), "{err:#}");
        let _ = std::fs::remove_file(path);
    }
}
