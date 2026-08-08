#!/usr/bin/env python3
"""Build a stored (-lh0-) LHA archive from (archive_path, host_path) pairs."""
import sys


def crc16(data):
    crc = 0
    for b in data:
        crc ^= b
        for _ in range(8):
            crc = (crc >> 1) ^ 0xA001 if crc & 1 else crc >> 1
    return crc


def member(path, data):
    name = path.encode()
    body = b"-lh0-"
    body += len(data).to_bytes(4, "little") * 2
    body += b"\0\0\0\0"  # time
    body += b"\x20"  # attr
    body += b"\0"  # level 0
    body += bytes([len(name)]) + name
    body += crc16(data).to_bytes(2, "little")
    head = bytes([len(body), sum(body) & 0xFF])
    return head + body + data


out = sys.argv[1]
blob = b""
for spec in sys.argv[2:]:
    arc_path, host_path = spec.split("=", 1)
    with open(host_path, "rb") as f:
        blob += member(arc_path, f.read())
blob += b"\0"
with open(out, "wb") as f:
    f.write(blob)
print(f"wrote {out} ({len(blob)} bytes)")
