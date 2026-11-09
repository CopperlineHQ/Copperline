#!/usr/bin/env python3
"""Generate the manual's version identity from this checkout, never a fixed series."""

from pathlib import Path
import subprocess
import tomllib


ROOT = Path(__file__).resolve().parents[1]


def git(*args: str) -> str | None:
    # A source archive nested inside another repository must not inherit its identity.
    if not (ROOT / ".git").exists():
        return None
    try:
        result = subprocess.run(
            ["git", "-C", str(ROOT), *args],
            capture_output=True, text=True, check=False,
        )
    except FileNotFoundError:
        return None
    return result.stdout.strip() if result.returncode == 0 else None


def main() -> None:
    with (ROOT / "Cargo.toml").open("rb") as manifest:
        version = tomllib.load(manifest)["package"]["version"]
    revision = git("rev-parse", "HEAD")
    tag = git("describe", "--tags", "--exact-match", "HEAD")
    clean = git("diff", "--quiet", "HEAD", "--") is not None
    release = tag == f"v{version}" and clean
    kind = "release" if release else "source snapshot"
    if revision:
        # A local or fork commit need not exist in the upstream repository.
        source = f"`{revision[:12]}`"
        identity = f"{kind} {source}"
        if not clean:
            identity += " with local changes"
    else:
        identity = "source snapshot; revision unavailable"
    text = (
        f"**Copperline {version} manual** - {identity}. "
        "Match `copperline --version`; see "
        "[version and compatibility help](guide/troubleshooting.md).\n"
    )
    output = ROOT / "docs" / "_version.md"
    if not output.exists() or output.read_text() != text:
        output.write_text(text)
    print(f"Manual: Copperline {version}, {identity}")


if __name__ == "__main__":
    main()
