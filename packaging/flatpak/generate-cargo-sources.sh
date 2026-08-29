#!/usr/bin/env bash
# Regenerate packaging/flatpak/cargo-sources.json from the committed
# Cargo.lock. Flathub builds run offline, so every crate Copperline depends
# on must be listed here as a vendored source. The output is intentionally
# untracked: CI runs this before every Flatpak build, and maintainers run it
# before copying the packaging files into a Flathub submission.
#
# Requires python3 and network access (to fetch the generator script). The
# generator itself only reads Cargo.lock; it does not download crates.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
gen_url="https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py"

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

echo "Fetching flatpak-cargo-generator..."
curl -fsSL -o "$workdir/flatpak-cargo-generator.py" "$gen_url"

echo "Setting up Python environment..."
python3 -m venv "$workdir/venv"
"$workdir/venv/bin/pip" -q install aiohttp toml tomlkit

echo "Generating cargo-sources.json from Cargo.lock..."
"$workdir/venv/bin/python" "$workdir/flatpak-cargo-generator.py" \
  "$repo_root/Cargo.lock" -o "$here/cargo-sources.json"

echo "Wrote $here/cargo-sources.json"
