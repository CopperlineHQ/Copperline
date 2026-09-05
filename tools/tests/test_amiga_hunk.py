# SPDX-License-Identifier: GPL-3.0-or-later
"""Shared HUNK reader and relocation tests, independent of cross compilers."""

import struct
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from amiga_hunk import HUNK_BSS, HUNK_CODE, HUNK_DATA, Hunk, read_hunks, relocate_hunks


def words(*values):
    return struct.pack(">" + "I" * len(values), *values)


class HunkTests(unittest.TestCase):
    def test_code_data_bss_relocations_and_symbols(self):
        image = words(
            0x3F3, 0, 3, 0, 2, 2, 1, 3,
            HUNK_CODE, 2, 4, 0x4E754E71,
            0x3EC, 1, 1, 0, 0,
            0x3F0, 1, 0x72756E00, 4, 0,
            0x3F1, 1, 0, 0x3F2,
            HUNK_DATA, 1, 0x01020304, 0x3F2,
            HUNK_BSS, 3, 0x3F2,
        )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "program"
            path.write_bytes(image)
            hunks = read_hunks(path)
        self.assertEqual([h.kind for h in hunks], [HUNK_CODE, HUNK_DATA, HUNK_BSS])
        self.assertEqual(hunks[2].bss_size, 12)
        first, symbols = relocate_hunks(hunks, [0x1000, 0x2000, 0x3000])
        self.assertEqual(first, [words(0x2004, 0x4E754E71), words(0x01020304), b""])
        self.assertEqual(symbols, {"run": 0x1004})
        second, _ = relocate_hunks(hunks, [0x4000, 0x5000, 0x6000])
        self.assertEqual(second[0], words(0x5004, 0x4E754E71))
        self.assertEqual(hunks[0].data, words(4, 0x4E754E71))

    def test_missing_relocation_destination_is_reported(self):
        with self.assertRaisesRegex(ValueError, "one load address"):
            relocate_hunks([Hunk(HUNK_DATA, bytearray(4), 0, [], {})], [])


if __name__ == "__main__":
    unittest.main()
