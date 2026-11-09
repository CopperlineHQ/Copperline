#!/usr/bin/env python3
"""Reassemble every tracked timing probe and require byte-identical binaries."""

import argparse
import os
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vasm", default=os.environ.get("VASM", "vasmm68k_mot"))
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    binaries = subprocess.check_output(
        ["git", "ls-files", "timing-test/*.bin"], cwd=root, text=True, timeout=30
    ).splitlines()
    if not binaries:
        parser.error("no tracked timing-test binaries found")
    failed = []
    with tempfile.TemporaryDirectory(prefix="copperline-probe-build-") as work:
        for name in binaries:
            binary = root / name
            source = binary.with_suffix(".asm")
            output = Path(work) / binary.name
            cpu = "-m68020" if binary.stem == "jit-bus-boundary" else "-m68000"
            result = subprocess.run(
                [args.vasm, "-Fbin", cpu, "-o", str(output), str(source)],
                cwd=root / "timing-test", capture_output=True, text=True, timeout=30,
            )
            if result.returncode:
                failed.append(name)
                print(f"{name}: assembly failed\n{result.stdout}{result.stderr}")
            elif output.read_bytes() != binary.read_bytes():
                failed.append(name)
                print(f"{name}: differs from {source.relative_to(root)}; rebuild and review it")
    if failed:
        raise SystemExit(f"{len(failed)} of {len(binaries)} probe binaries failed")
    print(f"All {len(binaries)} tracked probe binaries match their assembly sources.")


if __name__ == "__main__":
    main()
