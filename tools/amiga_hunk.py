#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Read the subset of Amiga HUNK files emitted by the pinned cross-GCC."""

from __future__ import annotations

import dataclasses
import struct
from pathlib import Path

HUNK_HEADER = 0x3F3
HUNK_CODE = 0x3E9
HUNK_DATA = 0x3EA
HUNK_BSS = 0x3EB
HUNK_RELOC32 = 0x3EC
HUNK_SYMBOL = 0x3F0
HUNK_DEBUG = 0x3F1
HUNK_END = 0x3F2


@dataclasses.dataclass
class Hunk:
    kind: int
    data: bytearray
    bss_size: int
    reloc32: list[tuple[int, int]]
    symbols: dict[str, int]


class Reader:
    def __init__(self, data: bytes):
        self.data = data
        self.offset = 0

    def word(self) -> int:
        if self.offset + 4 > len(self.data):
            raise ValueError("truncated HUNK file")
        value = struct.unpack_from(">I", self.data, self.offset)[0]
        self.offset += 4
        return value

    def bytes(self, size: int) -> bytes:
        if self.offset + size > len(self.data):
            raise ValueError("truncated HUNK payload")
        value = self.data[self.offset : self.offset + size]
        self.offset += size
        return value


def _name(reader: Reader, long_count: int) -> str:
    raw = reader.bytes(long_count * 4)
    return raw.rstrip(b"\0").decode("ascii")


def read_hunks(path: Path) -> list[Hunk]:
    reader = Reader(path.read_bytes())
    if reader.word() != HUNK_HEADER:
        raise ValueError(f"{path} is not an Amiga HUNK executable")
    if reader.word() != 0:
        raise ValueError("resident-library names are not supported")
    table_size = reader.word()
    first = reader.word()
    last = reader.word()
    if first != 0 or last + 1 != table_size:
        raise ValueError("non-contiguous HUNK table")
    sizes = [reader.word() & 0x3FFFFFFF for _ in range(table_size)]

    hunks: list[Hunk] = []
    for index, expected_longs in enumerate(sizes):
        kind = reader.word() & 0x3FFFFFFF
        if kind not in (HUNK_CODE, HUNK_DATA, HUNK_BSS):
            raise ValueError(f"unsupported HUNK type 0x{kind:x}")
        actual_longs = reader.word()
        if actual_longs > expected_longs:
            raise ValueError(f"HUNK {index} exceeds its allocation")
        payload = bytearray()
        bss_size = 0
        if kind == HUNK_BSS:
            bss_size = actual_longs * 4
        else:
            payload = bytearray(reader.bytes(actual_longs * 4))
        hunk = Hunk(kind, payload, bss_size, [], {})

        while True:
            record = reader.word() & 0x3FFFFFFF
            if record == HUNK_END:
                break
            if record == HUNK_RELOC32:
                while True:
                    count = reader.word()
                    if count == 0:
                        break
                    target = reader.word()
                    if target >= table_size:
                        raise ValueError("relocation targets an unknown HUNK")
                    for _ in range(count):
                        hunk.reloc32.append((reader.word(), target))
            elif record == HUNK_SYMBOL:
                while True:
                    name_longs = reader.word()
                    if name_longs == 0:
                        break
                    name = _name(reader, name_longs)
                    hunk.symbols[name] = reader.word()
            elif record == HUNK_DEBUG:
                reader.bytes(reader.word() * 4)
            else:
                raise ValueError(f"unsupported HUNK record 0x{record:x}")
        hunks.append(hunk)

    if reader.offset != len(reader.data):
        raise ValueError("trailing data after final HUNK")
    return hunks


def relocate_hunks(
    hunks: list[Hunk], load_addresses: list[int]
) -> tuple[list[bytes], dict[str, int]]:
    if len(hunks) != len(load_addresses):
        raise ValueError("one load address is required per HUNK")
    output: list[bytes] = []
    symbols: dict[str, int] = {}
    for hunk_index, hunk in enumerate(hunks):
        data = bytearray(hunk.data)
        for offset, target in hunk.reloc32:
            if offset + 4 > len(data):
                raise ValueError("relocation lies outside its source HUNK")
            value = struct.unpack_from(">I", data, offset)[0]
            struct.pack_into(">I", data, offset, value + load_addresses[target])
        output.append(bytes(data))
        for name, offset in hunk.symbols.items():
            address = load_addresses[hunk_index] + offset
            if name in symbols:
                raise ValueError(f"duplicate HUNK symbol {name}")
            symbols[name] = address
    return output, symbols
