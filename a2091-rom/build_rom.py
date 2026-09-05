#!/usr/bin/env python3
# SPDX-License-Identifier: BSD-2-Clause
"""Assemble a board-linear A2091 payload into merged and split EPROM images."""

from __future__ import annotations

import argparse
import hashlib
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
from amiga_hunk import read_hunks


ROM_SIZES = (16 * 1024, 32 * 1024, 64 * 1024)
ROM_WINDOW_OFFSET = 0x2000
TOC_SIZE = 40
TOC_MAGIC = (0xFFFF5352, 0x2F434448)


def payload_span(image_size: int) -> int:
    """Number of unique payload bytes visible from board offset $2000."""

    if image_size not in ROM_SIZES:
        raise ValueError("A2091 image size must be 16, 32, or 64 KiB")
    return image_size if image_size < 64 * 1024 else 56 * 1024


def board_linear_to_image(payload: bytes, image_size: int) -> bytes:
    """Rotate what the board sees at $2000 into physical EPROM order."""

    span = payload_span(image_size)
    if len(payload) != span:
        raise ValueError(f"payload is {len(payload)} bytes, expected {span}")
    image = bytearray(b"\xff" * image_size)
    for offset, value in enumerate(payload):
        image[(ROM_WINDOW_OFFSET + offset) % image_size] = value
    return bytes(image)


def image_to_board_linear(image: bytes) -> bytes:
    """Return the unique byte sequence visible from board offset $2000."""

    span = payload_span(len(image))
    return bytes(image[(ROM_WINDOW_OFFSET + i) % len(image)] for i in range(span))


def build_payload(boot: bytes, driver: bytes | None, image_size: int) -> bytes:
    span = payload_span(image_size)
    device_offset = (len(boot) + 15) & ~15
    device = driver or b""
    end = device_offset + len(device)
    if end > span - TOC_SIZE:
        raise ValueError(
            f"bootloader and driver need {end + TOC_SIZE} bytes, "
            f"but the {image_size // 1024} KiB image exposes {span}"
        )

    payload = bytearray(b"\xff" * span)
    payload[: len(boot)] = boot
    payload[device_offset:end] = device
    struct.pack_into(
        ">10I",
        payload,
        span - TOC_SIZE,
        0xFFFFFFFF, 0, 0,  # filesystem 2
        0xFFFFFFFF, 0, 0,  # filesystem 1
        device_offset,
        len(device),
        *TOC_MAGIC,
    )
    return bytes(payload)


def validate_image(image: bytes, *, expect_driver: bool) -> None:
    payload = image_to_board_linear(image)
    if payload[0] != 0x90:
        raise ValueError("DiagArea is not at board offset $2000")

    _, flags, diag_size, diag_point, boot_point, name, reserved1, reserved2 = (
        struct.unpack_from(">BBHHHHHH", payload)
    )
    if flags != 0 or reserved1 != 0 or reserved2 != 0:
        raise ValueError("DiagArea reserved fields are not zero")
    for label, offset in (
        ("DiagPoint", diag_point),
        ("BootPoint", boot_point),
        ("name", name),
    ):
        if not 0 < offset < diag_size:
            raise ValueError(f"DiagArea {label} lies outside da_Size")
    if b"scsi.device\0" not in payload[:diag_size]:
        raise ValueError("DiagArea does not name scsi.device")
    if not any(
        struct.unpack_from(">H", payload, offset)[0] == 0x4AFC
        for offset in range(0, diag_size - 1, 2)
    ):
        raise ValueError("DiagArea has no Resident structure")

    toc = struct.unpack_from(">10I", payload, len(payload) - TOC_SIZE)
    if toc[-2:] != TOC_MAGIC:
        raise ValueError("ROM TOC magic is missing")
    device_offset, device_len = toc[6], toc[7]
    if device_offset % 16 != 0:
        raise ValueError("device HUNK is not 16-byte aligned")
    if device_offset + device_len > len(payload) - TOC_SIZE:
        raise ValueError("device HUNK overlaps the ROM TOC")
    if expect_driver != (device_len != 0):
        raise ValueError("ROM driver presence does not match the requested build")


def split_eproms(image: bytes) -> tuple[bytes, bytes]:
    """Return U13 even and U12 odd byte lanes."""

    return image[0::2], image[1::2]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--boot", type=Path, required=True)
    parser.add_argument("--driver", type=Path)
    parser.add_argument("--size", type=int, choices=(16, 32, 64), default=64)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    boot = args.boot.read_bytes()
    driver = args.driver.read_bytes() if args.driver else None
    if args.driver:
        read_hunks(args.driver)

    image = board_linear_to_image(
        build_payload(boot, driver, args.size * 1024), args.size * 1024
    )
    validate_image(image, expect_driver=driver is not None)
    even, odd = split_eproms(image)

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(image)
    even_path = args.output.with_name(f"{args.output.stem}-U13.bin")
    odd_path = args.output.with_name(f"{args.output.stem}-U12.bin")
    even_path.write_bytes(even)
    odd_path.write_bytes(odd)
    digest = hashlib.sha256(image).hexdigest()
    sha_path = args.output.with_suffix(args.output.suffix + ".sha256")
    sha_path.write_text(f"{digest}  {args.output.name}\n", encoding="ascii")
    print(f"wrote {args.output} ({len(image)} bytes, sha256 {digest})")


if __name__ == "__main__":
    main()
