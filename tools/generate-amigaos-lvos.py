#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Regenerate the compact AmigaOS LVO-name table from an AROS checkout.

Usage: tools/generate-amigaos-lvos.py /path/to/AROS

The output is intentionally only ABI metadata (module name, LVO number and
public function name).  Copperline reads the generated TSV at compile time;
the AROS checkout is not a build dependency.
"""

from __future__ import annotations

import pathlib
import re
import subprocess
import sys


TABLES = (
    ("exec.library", "rom/exec/exec.conf", 1),
    ("dos.library", "rom/dos/dos.conf", 1),
    ("graphics.library", "rom/graphics/graphics.conf", 5),
    ("intuition.library", "rom/intuition/intuition.conf", 5),
    ("layers.library", "rom/hyperlayers/layers.conf", 5),
    ("expansion.library", "rom/expansion/expansion.conf", 5),
    ("utility.library", "rom/utility/utility.conf", 5),
    ("keymap.library", "rom/keymap/keymap.conf", 5),
    ("icon.library", "workbench/libs/icon/icon.conf", 5),
    ("diskfont.library", "workbench/libs/diskfont/diskfont.conf", 5),
    ("gadtools.library", "workbench/libs/gadtools/gadtools.conf", 5),
    ("workbench.library", "workbench/libs/workbench/workbench.conf", 5),
    ("asl.library", "workbench/libs/asl/asl.conf", 5),
    ("commodities.library", "workbench/libs/commodities/commodities.conf", 5),
    ("iffparse.library", "workbench/libs/iffparse/iffparse.conf", 5),
    ("locale.library", "workbench/libs/locale/locale.conf", 5),
    ("nonvolatile.library", "workbench/libs/nonvolatile/nonvolatile.conf", 5),
    ("realtime.library", "workbench/libs/realtime/realtime.conf", 5),
    ("lowlevel.library", "workbench/libs/lowlevel/lowlevel.conf", 5),
    ("amigaguide.library", "workbench/libs/amigaguide/amigaguide.conf", 5),
    ("datatypes.library", "workbench/libs/datatypes/datatypes.conf", 5),
    ("bullet.library", "workbench/libs/bullet/bullet.conf", 5),
    ("translator.library", "workbench/libs/translator/translator.conf", 5),
    ("mathffp.library", "workbench/libs/mathffp/mathffp.conf", 5),
    ("mathtrans.library", "workbench/libs/mathtrans/mathtrans.conf", 5),
    (
        "mathieeesingbas.library",
        "workbench/libs/mathieeesingbas/mathieeesingbas.conf",
        5,
    ),
    (
        "mathieeesingtrans.library",
        "workbench/libs/mathieeesingtrans/mathieeesingtrans.conf",
        5,
    ),
    (
        "mathieeedoubbas.library",
        "workbench/libs/mathieeedoubbas/mathieeedoubbas.conf",
        5,
    ),
    (
        "mathieeedoubtrans.library",
        "workbench/libs/mathieeedoubtrans/mathieeedoubtrans.conf",
        5,
    ),
    ("input.device", "rom/devs/input/input.conf", 7),
    ("console.device", "rom/devs/console/console.conf", 7),
    ("timer.device", "rom/timer/timer.conf", 7),
    ("scsi.device", "rom/devs/scsi/scsi.conf", 7),
    ("ramdrive.device", "workbench/devs/ramdrive.conf", 7),
)

# These slots are explicitly identified by the pinned AROS configuration as
# AROS-specific. They cannot safely name an arbitrary classic AmigaOS vector:
# graphics 107-108 occupy otherwise unused/private slots, and 181 onward is
# the AROS extension block in MorphOS-private space.
AROS_ONLY_LVOS = {
    "graphics.library": frozenset({107, 108, *range(181, 202)}),
}


def function_rows(path: pathlib.Path, first_lvo: int) -> list[tuple[int, str]]:
    lines = path.read_text(encoding="utf-8").splitlines()
    try:
        start = lines.index("##begin functionlist") + 1
        end = lines.index("##end functionlist", start)
    except ValueError as error:
        raise SystemExit(f"{path}: missing functionlist section") from error

    entries: list[dict[str, object]] = []
    lvo = first_lvo
    for raw in lines[start:end]:
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith(".skip"):
            lvo += int(line.split()[1])
            continue
        if line == ".private":
            if not entries:
                raise SystemExit(f"{path}: .private without a function")
            entries[-1]["private"] = True
            continue
        if line.startswith("."):
            continue
        match = re.search(r"([A-Za-z_][A-Za-z0-9_]*)\s*\(", line)
        if not match:
            raise SystemExit(f"{path}: cannot read function declaration: {raw}")
        entries.append({"lvo": lvo, "name": match.group(1), "private": False})
        lvo += 1

    return [
        (int(entry["lvo"]), str(entry["name"]))
        for entry in entries
        if not bool(entry["private"])
    ]


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    aros = pathlib.Path(sys.argv[1]).resolve()
    if not (aros / "LICENSE").is_file():
        print(f"not an AROS checkout: {aros}", file=sys.stderr)
        return 2
    try:
        revision = subprocess.check_output(
            ["git", "-C", str(aros), "rev-parse", "--verify", "HEAD"],
            text=True,
            stderr=subprocess.STDOUT,
        ).strip()
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"cannot determine AROS Git revision for {aros}: {error}", file=sys.stderr)
        return 2
    if not re.fullmatch(r"[0-9a-fA-F]{40}|[0-9a-fA-F]{64}", revision):
        print(f"invalid AROS Git revision for {aros}: {revision!r}", file=sys.stderr)
        return 2

    output = pathlib.Path(__file__).resolve().parents[1] / "assets/symbols/amigaos-lvo.tsv"
    output.parent.mkdir(parents=True, exist_ok=True)
    rows = [
        "# Generated by tools/generate-amigaos-lvos.py.",
        f"# AROS revision: {revision}",
        "# Derived from AROS public ABI metadata; see LICENSE.AROS.",
        "# module\tlvo\tname",
    ]
    for module, relative, first_lvo in TABLES:
        for lvo, name in function_rows(aros / relative, first_lvo):
            if lvo in AROS_ONLY_LVOS.get(module, ()):
                continue
            rows.append(f"{module}\t{lvo}\t{name}")
    output.write_text("\n".join(rows) + "\n", encoding="utf-8")
    print(f"wrote {output} ({len(rows) - 4} functions)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
