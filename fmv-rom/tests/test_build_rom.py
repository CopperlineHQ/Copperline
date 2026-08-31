# SPDX-License-Identifier: GPL-3.0-or-later

import struct
import sys
import unittest
import os
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import build_rom  # noqa: E402


class OpenFmvRomTests(unittest.TestCase):
    def setUp(self) -> None:
        driver = os.environ.get("FMV_DRIVER_HUNK")
        self.assertIsNotNone(driver, "FMV_DRIVER_HUNK must name the built device HUNK")
        self.rom = build_rom.build_rom(Path(driver))

    def test_image_has_exact_cartridge_rom_size(self) -> None:
        self.assertEqual(len(self.rom), 256 * 1024)
        build_rom.validate_rom(self.rom)

    def test_diag_area_enters_the_resident_device(self) -> None:
        at = build_rom.DIAG_OFFSET
        config, flags, size, diag, boot, name, reserved1, reserved2 = struct.unpack_from(
            ">BBHHHHHH", self.rom, at
        )
        self.assertEqual(config, 0x90)
        self.assertEqual(flags, 0)
        self.assertEqual(size, build_rom.DIAG_SIZE)
        self.assertEqual((diag, boot, name), (0x1E, 0x1A, 0x0E))
        self.assertEqual((reserved1, reserved2), (0, 0))
        self.assertEqual(self.rom[at + diag : at + diag + 2], bytes.fromhex("4ef9"))
        target = struct.unpack_from(">I", self.rom, at + diag + 2)[0]
        self.assertGreaterEqual(target, 0x201000)
        self.assertLess(target, 0x200000 + build_rom.FIRMWARE_HEADER_OFFSET)
        self.assertEqual(self.rom[at + boot : at + boot + 4], bytes.fromhex("70004e75"))
        self.assertIn(b"cd32mpeg.device\0", self.rom)
        self.assertIn(struct.pack(">H", 0x4AFC), self.rom)

    def test_rebuild_is_byte_deterministic(self) -> None:
        driver = Path(os.environ["FMV_DRIVER_HUNK"])
        self.assertEqual(self.rom, build_rom.build_rom(driver))

    def test_firmware_container_is_empty(self) -> None:
        self.assertEqual(
            struct.unpack_from(">I", self.rom, build_rom.FIRMWARE_HEADER_OFFSET)[0],
            build_rom.CL450_FIRMWARE_MAGIC,
        )
        self.assertEqual(struct.unpack_from("<H", self.rom, build_rom.CL450_ENTRY_OFFSET)[0], 0)
        self.assertEqual(struct.unpack_from("<H", self.rom, build_rom.CL450_BASE_OFFSET)[0], 0)
        self.assertEqual(
            struct.unpack_from("<H", self.rom, build_rom.CL450_CHUNK_COUNT_OFFSET)[0], 0
        )


if __name__ == "__main__":
    unittest.main()
