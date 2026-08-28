#!/usr/bin/env python3
"""Build burnable CD32 test images from the Fightin' Spirit rip.

Outputs (plain single-data-track ISOs, burnable with drutil/Finder --
the burner regenerates sync/EDC/ECC for MODE1 sectors, and the CD32
boots the data track alone; the game's audio tracks are irrelevant to
the boot-layout question):

  fs-game-unpatched.iso  the game's data track, untouched: does the
                         real CD32 cold-boot the game at all?
  cd32-probe.iso         the same track with tools/cd32-probe/PROBE
                         spliced in place of SYS:FSCD, padded with zero
                         sectors so CD seek targets out to ~215000 exist.

Usage: make-images.py <track1.bin (MODE1/2352)> <output dir>
"""

import struct
import sys
from pathlib import Path

RAW = 2352
DATA = 2048
PAD_SECTORS = 215_000  # ~440 MB: mirrors the full game's LBA range


def extract_iso(track1: bytes) -> bytearray:
    out = bytearray()
    for off in range(0, len(track1) - RAW + 1, RAW):
        out += track1[off + 16 : off + 16 + DATA]
    return out


def find_root_dir(iso: bytes):
    pvd = iso[16 * DATA : 17 * DATA]
    assert pvd[1:6] == b"CD001", "not ISO9660"
    root = pvd[156 : 156 + 34]
    extent = struct.unpack("<I", root[2:6])[0]
    size = struct.unpack("<I", root[10:14])[0]
    return extent, size


def find_record(iso: bytes, dir_extent: int, dir_size: int, name: bytes):
    """Return (absolute offset of the directory record, extent, size)."""
    base = dir_extent * DATA
    i = 0
    while i < dir_size:
        ln = iso[base + i]
        if ln == 0:
            i = (i // DATA + 1) * DATA  # records never straddle sectors
            continue
        rec = iso[base + i : base + i + ln]
        nlen = rec[32]
        if rec[33 : 33 + nlen] == name:
            extent = struct.unpack("<I", rec[2:6])[0]
            size = struct.unpack("<I", rec[10:14])[0]
            return base + i, extent, size
        i += ln
    raise SystemExit(f"{name!r} not found in directory at extent {dir_extent}")


def main():
    track1 = Path(sys.argv[1]).read_bytes()
    outdir = Path(sys.argv[2])
    outdir.mkdir(parents=True, exist_ok=True)
    probe = (Path(__file__).parent / "PROBE").read_bytes()

    iso = extract_iso(track1)
    (outdir / "fs-game-unpatched.iso").write_bytes(iso)
    print(f"fs-game-unpatched.iso: {len(iso)} bytes ({len(iso)//DATA} sectors)")

    root_extent, root_size = find_root_dir(iso)
    rec_off, fscd_extent, fscd_size = find_record(
        iso, root_extent, root_size, b"FSCD;1"
    )
    assert len(probe) <= fscd_size, "probe larger than the FSCD slot"
    print(
        f"FSCD;1: record @{rec_off:#x} extent={fscd_extent} size={fscd_size}"
        f" -> probe {len(probe)} bytes"
    )

    # Splice the probe over FSCD's data and zero the remainder of its
    # old extent, then rewrite the record's both-endian data length.
    start = fscd_extent * DATA
    iso[start : start + fscd_size] = probe + b"\0" * (fscd_size - len(probe))
    iso[rec_off + 10 : rec_off + 14] = struct.pack("<I", len(probe))
    iso[rec_off + 14 : rec_off + 18] = struct.pack(">I", len(probe))

    # Zero padding beyond the filesystem: the burned data track then
    # physically spans PAD_SECTORS, giving CD_SEEK/CMD_READ real
    # distances to travel. The ISO structures do not reference it.
    pad = PAD_SECTORS * DATA - len(iso)
    assert pad > 0
    out = outdir / "cd32-probe.iso"
    with out.open("wb") as f:
        f.write(iso)
        f.write(b"\0" * pad)
    print(f"cd32-probe.iso: {PAD_SECTORS} sectors ({PAD_SECTORS*DATA//1_000_000} MB)")

    # Full-fidelity variant: the probe data track plus the game's own 33
    # audio tracks, so the burned TOC matches the original disc exactly
    # and the CD32 boots through the same show-vs-boot race the game
    # sees. cdrdao regenerates MODE1 sync/EDC/ECC from the 2048-byte
    # data file; the audio content is never played by the probe, so its
    # byte order is irrelevant. The unpadded probe data reuses the
    # patched iso (no seek pad: the audio tracks provide the LBA range).
    trackdir = Path(sys.argv[1]).parent
    (outdir / "cd32-probe-track01.iso").write_bytes(iso)
    toc = ["CD_ROM", "", "TRACK MODE1", 'DATAFILE "cd32-probe-track01.iso"', ""]
    for n in range(2, 35):
        audio = trackdir / f"Fightin' Spirit (Europe) (En,De,It) (Track {n:02d}).bin"
        assert audio.exists(), audio
        toc += [
            "TRACK AUDIO",
            "PREGAP 0:2:0",
            f'FILE "{audio}" 0',
            "",
        ]
    (outdir / "cd32-probe-full.toc").write_text("\n".join(toc))
    print("cd32-probe-full.toc: probe track 1 + the game's 33 audio tracks")

    # Emulator-verification cue for the full variant (Copperline reads
    # cue/bin; the audio stays the original files).
    src_cue = trackdir / "Fightin' Spirit (Europe) (En,De,It).cue"
    cue = src_cue.read_text()
    cue = cue.replace(
        'FILE "Fightin\' Spirit (Europe) (En,De,It) (Track 01).bin" BINARY',
        'FILE "cd32-probe-track01-raw.bin" BINARY',
    )
    cue = cue.replace(
        'FILE "Fightin\' Spirit',
        f'FILE "{trackdir}/Fightin\' Spirit',
    )
    (outdir / "cd32-probe-full.cue").write_text(cue)
    # Raw 2352 rebuild of the patched track for the emulator cue: reuse
    # the original raw sectors, splicing the patched 2048-byte payloads
    # back in (the emulator does not verify EDC/ECC).
    raw = bytearray(track1)
    for sec in range(len(raw) // RAW):
        raw[sec * RAW + 16 : sec * RAW + 16 + DATA] = iso[
            sec * DATA : (sec + 1) * DATA
        ]
    (outdir / "cd32-probe-track01-raw.bin").write_bytes(raw)
    print("cd32-probe-full.cue: emulator-verification cue for the full variant")


if __name__ == "__main__":
    main()
