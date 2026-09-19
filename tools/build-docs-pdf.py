#!/usr/bin/env python3
"""Build the manual, correcting MyST's legacy Typst table and arrow output."""

from pathlib import Path
import re
import subprocess
import sys


DOCS = Path(__file__).resolve().parents[1] / "docs"
TABLEX_IMPORT = '#import "@preview/tablex:0.0.9": tablex, cellx, hlinex, vlinex'
NATIVE_TABLES = """// Copperline's Markdown tables use integer column counts and one header row.
// Native tables repeat that header without tablex's non-converging page state.
#let tablex(columns: 1, header-rows: 0, repeat-header: false, ..args) = {
  let cells = args.pos()
  let headers = columns * header-rows
  table(columns: columns, ..args.named(),
    table.header(repeat: repeat-header, ..cells.slice(0, headers)),
    ..cells.slice(headers))
}
#let cellx = table.cell"""


def repair_generated_sources(directory: Path) -> None:
    """Only modify generated sources; Markdown and downloaded templates stay intact."""
    imports = directory / "myst-imports.typ"
    text = imports.read_text()
    if TABLEX_IMPORT in text:
        # Refuse newly introduced tablex features until they have a native mapping.
        for source in directory.glob("copperline-*.typ"):
            content = source.read_text()
            calls = re.findall(r"#tablex\(([^\n]+)", content)
            if any(not re.fullmatch(
                r"columns: [2-6], header-rows: 1, repeat-header: true, "
                r"\.\.tableStyle, \.\.columnStyle,", call
            ) for call in calls):
                raise ValueError(f"Unmapped tablex arguments in {source.name}")
            if re.search(r"\b(?:hlinex|vlinex)\s*\(", content):
                raise ValueError(f"Unmapped tablex lines in {source.name}")
            cells = re.findall(r"\bcellx\(([^\n]+)", content)
            if any(cell != "align: right, )[" for cell in cells):
                raise ValueError(f"Unmapped tablex cell in {source.name}")
        imports.write_text(text.replace(TABLEX_IMPORT, NATIVE_TABLES))

    for source in directory.glob("copperline-*.typ"):
        # MyST 1.10 emits the symbol's name without its Typst expression prefix.
        # Only standalone prose occurrences are changed; raw code is preserved.
        text = source.read_text()
        parts = re.split(r"(`+[^`]*`+)", text)
        for index in range(0, len(parts), 2):
            parts[index] = re.sub(r"(?<![\w.#])arrow\.r(?![\w.])", "#sym.arrow.r", parts[index])
        source.write_text("".join(parts))

    frontmatter = directory / "frontmatter.typ"
    if frontmatter.exists():
        text = frontmatter.read_text()
        frontmatter.write_text(text.replace("```<svg", "``` <svg"))


def main() -> None:
    temporary = DOCS / "_build" / "temp"
    before = set(temporary.glob("*/copperline.typ"))
    subprocess.run(["myst", "build", "--pdf", *sys.argv[1:]], cwd=DOCS, check=True)
    generated = set(temporary.glob("*/copperline.typ")) - before
    if len(generated) != 1:
        raise RuntimeError(f"Expected one new manual source, found {len(generated)}")
    source = generated.pop()
    repair_generated_sources(source.parent)
    subprocess.run([
        "typst", "compile", str(source),
        str(DOCS / "_build" / "exports" / "copperline.pdf"),
    ], cwd=DOCS, check=True)
    print("Manual compiled with native tables and corrected menu arrows.")


if __name__ == "__main__":
    main()
