#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Build Copperline's standalone open CD32 FMV cartridge ROM."""

from __future__ import annotations

import argparse
import hashlib
import struct
from pathlib import Path

from hunk import HUNK_BSS, read_hunks, relocate_hunks


ROM_SIZE = 0x40000
DIAG_OFFSET = 0x80
FIRMWARE_HEADER_OFFSET = 0x761C

DIAG_CONFIG = 0x90  # DAC_WORDWIDE | DAC_CONFIGTIME
DIAG_NAME = b"config_mpeg\0"
DIAG_NAME_OFFSET = 0x0E
DIAG_BOOT_OFFSET = 0x1A
DIAG_POINT_OFFSET = 0x1E
DIAG_SIZE = 0x24

DRIVER_OFFSET = 0x1000
DRIVER_BSS_ADDRESS = 0x2FF000

CL450_FIRMWARE_MAGIC = 0xC3C301FD
CL450_ENTRY_OFFSET = 0x7622
CL450_BASE_OFFSET = 0x7624
CL450_CHUNK_COUNT_OFFSET = 0x762A

BUILD_ID = b"$VER: Copperline open CD32 FMV ROM 1.0 (30.08.2026)\0"


def build_rom(driver_hunk: Path | None = None) -> bytes:
    """Return a deterministic 256 KiB FMV cartridge image.

    The cartridge contains its own open cd32mpeg.device for Kickstart and
    AROS builds which execute expansion diagnostics. AROS PR #1089 carries a
    matching system-ROM device and deliberately skips the legacy cartridge
    diagnostic. Copperline initializes its command-level CL450 model when
    CPU_CONTROL is enabled and does not execute uploaded IMEM/TMEM, so an
    empty, correctly described firmware container is sufficient and contains
    no proprietary microcode.
    """

    rom = bytearray(b"\xFF" * ROM_SIZE)
    rom[: len(BUILD_ID)] = BUILD_ID

    diag_header = struct.pack(
        ">BBHHHHHH",
        DIAG_CONFIG,
        0,
        DIAG_SIZE,
        DIAG_POINT_OFFSET,
        DIAG_BOOT_OFFSET,
        DIAG_NAME_OFFSET,
        0,
        0,
    )
    rom[DIAG_OFFSET : DIAG_OFFSET + len(diag_header)] = diag_header
    rom[
        DIAG_OFFSET + DIAG_NAME_OFFSET : DIAG_OFFSET + DIAG_NAME_OFFSET + len(DIAG_NAME)
    ] = DIAG_NAME

    # The boot entry is unused. 68000: moveq #0,d0; rts.
    stub = bytes.fromhex("70 00 4E 75")
    rom[
        DIAG_OFFSET + DIAG_BOOT_OFFSET : DIAG_OFFSET + DIAG_BOOT_OFFSET + len(stub)
    ] = stub
    if driver_hunk is None:
        rom[
            DIAG_OFFSET + DIAG_POINT_OFFSET : DIAG_OFFSET
            + DIAG_POINT_OFFSET
            + len(stub)
        ] = stub
    else:
        hunks = read_hunks(driver_hunk)
        load_addresses: list[int] = []
        next_address = 0x200000 + DRIVER_OFFSET
        for hunk in hunks:
            if hunk.kind == HUNK_BSS:
                load_addresses.append(DRIVER_BSS_ADDRESS)
            else:
                load_addresses.append(next_address)
                next_address += (len(hunk.data) + 3) & ~3
        relocated, symbols = relocate_hunks(hunks, load_addresses)
        for hunk, address, data in zip(hunks, load_addresses, relocated):
            if hunk.kind == HUNK_BSS:
                if address + hunk.bss_size > 0x300000:
                    raise ValueError("driver BSS exceeds reserved module RAM")
                continue
            offset = address - 0x200000
            if offset < DRIVER_OFFSET or offset + len(data) > FIRMWARE_HEADER_OFFSET:
                raise ValueError("driver does not fit before the firmware header")
            rom[offset : offset + len(data)] = data

        entry = symbols.get("_RomDiagEntry")
        resident = symbols.get("_ROMTag")
        if entry is None or resident is None:
            raise ValueError("driver HUNK lacks RomDiagEntry or ROMTag")
        diag_jump = struct.pack(">HI", 0x4EF9, entry)
        rom[
            DIAG_OFFSET + DIAG_POINT_OFFSET : DIAG_OFFSET
            + DIAG_POINT_OFFSET
            + len(diag_jump)
        ] = diag_jump

    struct.pack_into(">I", rom, FIRMWARE_HEADER_OFFSET, CL450_FIRMWARE_MAGIC)
    struct.pack_into("<H", rom, CL450_ENTRY_OFFSET, 0)
    struct.pack_into("<H", rom, CL450_BASE_OFFSET, 0)
    struct.pack_into("<H", rom, CL450_CHUNK_COUNT_OFFSET, 0)

    return bytes(rom)


def validate_rom(rom: bytes) -> None:
    if len(rom) != ROM_SIZE:
        raise ValueError(f"ROM is {len(rom)} bytes, expected {ROM_SIZE}")
    if struct.unpack_from(">I", rom, FIRMWARE_HEADER_OFFSET)[0] != CL450_FIRMWARE_MAGIC:
        raise ValueError("missing CL450 firmware-container signature")
    if struct.unpack_from("<H", rom, CL450_CHUNK_COUNT_OFFSET)[0] != 0:
        raise ValueError("open FMV ROM unexpectedly contains firmware chunks")
    if rom[DIAG_OFFSET] != DIAG_CONFIG:
        raise ValueError("invalid DiagArea configuration")
    if rom[DIAG_OFFSET + DIAG_NAME_OFFSET :].split(b"\0", 1)[0] != DIAG_NAME[:-1]:
        raise ValueError("invalid DiagArea name")
    residents = []
    for offset in range(DRIVER_OFFSET, FIRMWARE_HEADER_OFFSET - 10, 2):
        if struct.unpack_from(">H", rom, offset)[0] != 0x4AFC:
            continue
        address = 0x200000 + offset
        match_tag, end_skip = struct.unpack_from(">II", rom, offset + 2)
        if match_tag == address:
            residents.append((address, end_skip))
    if not residents:
        raise ValueError("driver has no self-matching resident tag")
    if not any(address < end_skip <= 0x200000 + FIRMWARE_HEADER_OFFSET
               for address, end_skip in residents):
        raise ValueError("driver resident has an invalid end-skip pointer")
    if b"cd32mpeg.device\0" not in rom:
        raise ValueError("driver resident name is missing")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--driver-hunk", type=Path, required=True)
    args = parser.parse_args()

    rom = build_rom(args.driver_hunk)
    validate_rom(rom)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(rom)
    digest = hashlib.sha256(rom).hexdigest()
    print(f"wrote {args.output} ({len(rom)} bytes, sha256 {digest})")


if __name__ == "__main__":
    main()
