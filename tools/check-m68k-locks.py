#!/usr/bin/env python3
"""Require every CPU consumer, including the conformance runner, to test the same core."""

from pathlib import Path
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[1]
LOCKS = (
    "Cargo.lock",
    "crates/copperline-player/Cargo.lock",
    "crates/copperline-web/Cargo.lock",
    "crates/copperline-libretro/Cargo.lock",
    "crates/cputest-runner/Cargo.lock",
)


def identity(path: str) -> tuple[str, str | None, str | None]:
    with (ROOT / path).open("rb") as source:
        packages = tomllib.load(source)["package"]
    cores = [package for package in packages if package["name"] == "m68k"]
    if len(cores) != 1:
        raise ValueError(f"{path}: expected one m68k dependency, found {len(cores)}")
    core = cores[0]
    if "source" not in core:
        raise ValueError(f"{path}: cannot verify a local path dependency from its lockfile")
    return core["version"], core["source"], core.get("checksum")


def main() -> int:
    expected = identity(LOCKS[0])
    failures = [
        f"{path}: {actual!r}"
        for path in LOCKS[1:]
        if (actual := identity(path)) != expected
    ]
    if failures:
        print(f"m68k dependency differs from Cargo.lock: {expected!r}", file=sys.stderr)
        print("\n".join(failures), file=sys.stderr)
        return 1
    print(f"All CPU consumers use m68k {expected[0]} from {expected[1]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
