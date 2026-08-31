# SPDX-License-Identifier: BSD-2-Clause

import struct
import sys
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import build_rom  # noqa: E402


def synthetic_boot() -> bytes:
    boot = bytearray(b"\x4e\x71" * 64)
    struct.pack_into(
        ">BBHHHHHH", boot, 0,
        0x90, 0, len(boot), 0x10, 0x14, 0x18, 0, 0,
    )
    boot[0x10:0x14] = bytes.fromhex("70014e75")
    boot[0x14:0x18] = bytes.fromhex("70004e75")
    boot[0x18:0x24] = b"scsi.device\0"
    struct.pack_into(">H", boot, 0x30, 0x4AFC)
    return bytes(boot)


class A2091RomLayoutTests(unittest.TestCase):
    def test_rotation_round_trips_every_supported_eprom_size(self) -> None:
        for size in build_rom.ROM_SIZES:
            span = build_rom.payload_span(size)
            payload = bytes((i * 29 + 7) & 0xFF for i in range(span))
            image = build_rom.board_linear_to_image(payload, size)
            self.assertEqual(build_rom.image_to_board_linear(image), payload)

    def test_sixty_four_kib_shadow_is_left_erased(self) -> None:
        payload = bytes([0x5A]) * (56 * 1024)
        image = build_rom.board_linear_to_image(payload, 64 * 1024)
        self.assertEqual(image[:0x2000], b"\xff" * 0x2000)
        self.assertEqual(image[0x2000:], payload)

    def test_toc_and_split_lanes_reassemble(self) -> None:
        image = build_rom.board_linear_to_image(
            build_rom.build_payload(synthetic_boot(), b"driver", 64 * 1024),
            64 * 1024,
        )
        build_rom.validate_image(image, expect_driver=True)
        even, odd = build_rom.split_eproms(image)
        merged = bytearray()
        for e, o in zip(even, odd):
            merged.extend((e, o))
        self.assertEqual(bytes(merged), image)

    def test_driver_that_exceeds_visible_window_is_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "exposes"):
            build_rom.build_payload(synthetic_boot(), b"x" * 60000, 64 * 1024)


if __name__ == "__main__":
    unittest.main()
