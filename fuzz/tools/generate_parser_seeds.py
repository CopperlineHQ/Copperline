#!/usr/bin/env python3
"""Regenerate owned, deterministic parser seeds and boundary replay inputs.

Run from any directory. Does not remove retained crashes or discovered corpus
entries. The ELF fixture is compiled from the repository's guest probe source.
"""

import io
from pathlib import Path
import struct
import zipfile

ROOT = Path(__file__).resolve().parents[2]


def retain(target, name, data, *, corpus=True):
    destinations = [ROOT / "tests/fuzz-regressions" / target]
    if corpus:
        destinations.append(ROOT / "fuzz/corpus" / target)
    for dest in destinations:
        dest.mkdir(parents=True, exist_ok=True)
        (dest / name).write_bytes(data)


def crc16(data):
    crc = 0
    for byte in data:
        crc ^= byte
        for _ in range(8):
            crc = (crc >> 1) ^ (0xA001 if crc & 1 else 0)
    return crc


def lha_member(name, packed, size, crc, method=b"-lh0-"):
    name = name.encode("ascii")
    body = (method + struct.pack("<II", len(packed), size) + bytes(4)
            + bytes([0x20, 0, len(name)]) + name + struct.pack("<H", crc))
    return bytes([len(body), sum(body) % 256]) + body + packed


def lh5_zeros(size):
    # A single literal-zero Huffman leaf needs no bits per command. Each
    # block carries up to 65535 commands and three single-leaf trees.
    bits = ""
    remaining = size
    while remaining:
        count = min(remaining, 65535)
        bits += f"{count:016b}" + "0" * 36
        remaining -= count
    bits += "0" * (-len(bits) % 8)
    return int(bits, 2).to_bytes(len(bits) // 8, "big") + bytes(2)


def zip_members(members):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, data in members:
            info = zipfile.ZipInfo(name, date_time=(2000, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, data)
    return buffer.getvalue()


def main():
    slave = b"synthetic slave"
    retain("archives", "stored.lha",
           lha_member("Probe.slave", slave, len(slave), crc16(slave)) + b"\0")
    retain("archives", "stored.zip", zip_members([("Probe.slave", slave)]))
    size = 2 * 1024 * 1024
    retain("archives", "lh5-expansion.lha",
           lha_member("Probe.slave", lh5_zeros(size), size, 0, b"-lh5-") + b"\0")
    retain("archives", "deflate-expansion.zip", zip_members([("Probe.slave", bytes(size))]))
    lying = bytearray(zip_members([("Probe.slave", bytes(size))]))
    lying[22:26] = struct.pack("<I", 1)
    central = lying.index(b"PK\x01\x02")
    lying[central + 24:central + 28] = struct.pack("<I", 1)
    retain("archives", "lying-size.zip", lying)
    retain("config_toml", "machine.toml", b'[machine]\nprofile = "A1200"\n[memory]\nfast = "8M"\n')
    retain("config_toml", "truncated.toml", b'[machine]\nprofile = ["A1200", {')
    retain("control_protocol", "auth.jsonl",
           b'{"jsonrpc":"2.0","id":1,"method":"hello","params":{"token":"fuzz-token"}}\n'
           b'{"id":2,"method":"mem.write","params":{"addr":"0x10000","hex":"00ff"}}\n')
    retain("control_protocol", "invalid-utf8", b'{"id":1,"method":"\xff"}\n')
    words = [0x3F3, 0, 1, 0, 0, 1, 0x3E9, 1, 0x4E750000, 0x3F2]
    retain("debug_info", "minimal.hunk", struct.pack(">" + "I" * len(words), *words))
    retain("debug_info", "dwarf5.elf", (ROOT / "guest/dap-test/reloc/program-dwarf5.elf").read_bytes())
    # Truncated headers also provide stable replay cases for every existing
    # target. These are synthetic boundary cases, not claimed fuzz findings.
    for target, data in [
        ("dms", b"DMS!" + bytes(8)),
        ("floppy_image", b"UAE-1ADF" + bytes(4)),
        ("cd_image", b"MComprHD" + bytes(8)),
        ("hardfile_classification", b"MComprHD" + bytes(8)),
        ("savestate", b"CLSTATE" + bytes(8)),
    ]:
        retain(target, "truncated-header", data)


if __name__ == "__main__":
    main()
