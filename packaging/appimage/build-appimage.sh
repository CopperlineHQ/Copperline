#!/usr/bin/env bash
# Build a Copperline AppImage: a single self-contained, no-install binary
# that runs across Linux distributions. Run from a Linux host (or CI); see
# .github/workflows/appimage.yml.
#
# What it does:
#   1. Builds the release binary with the pinned dependency graph.
#   2. Stages an AppDir laid out like a /usr prefix, so romsearch.rs finds
#      the bundled AROS ROM via <bindir>/../share/copperline/aros.
#   3. Uses linuxdeploy to pull in the direct shared-library dependencies
#      (ALSA, udev, X11/Wayland, etc.) and wrap the AppDir into an AppImage.
#
# Notes:
#   - The GPU stack (Mesa/Vulkan/libGL) is deliberately NOT bundled; the
#     wgpu/pixels render path uses the host driver, which is what linuxdeploy's
#     default exclude list expects. Bundling those libraries breaks on hosts
#     with a different driver.
#   - Build on the OLDEST glibc you intend to support (an old runner image or
#     container); an AppImage built against a newer glibc will not start on
#     older systems.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
flatpak_meta="$repo_root/packaging/flatpak"
cd "$repo_root"

appdir="$repo_root/AppDir"
arch="$(uname -m)"
tools_dir="${LINUXDEPLOY_DIR:-$repo_root/.appimage-tools}"
linuxdeploy="$tools_dir/linuxdeploy-$arch.AppImage"

echo "==> Building release binary"
cargo build --release --locked

echo "==> Staging AppDir"
rm -rf "$appdir"
install -Dm755 target/release/copperline "$appdir/usr/bin/copperline"
# The AppImage cannot retain file capabilities itself. Ship the narrowly
# privileged Linux bridge helper and its setup/unit files inside the image;
# `copperline --install-net-helper` copies the helper to /usr/libexec and gives
# only that copy CAP_NET_RAW.
net_helper_dir="$appdir/usr/libexec/copperline"
install -Dm755 target/release/copperline-net-helper \
  "$net_helper_dir/copperline-net-helper"
install -Dm755 packaging/linux/copperline-net-helper-setup \
  "$net_helper_dir/copperline-net-helper-setup"
install -Dm644 packaging/linux/copperline-net-helper.service \
  "$net_helper_dir/copperline-net-helper.service"
install -Dm644 packaging/linux/copperline-net-helper.socket \
  "$net_helper_dir/copperline-net-helper.socket"

# Bundled AROS open-source Kickstart replacement (default boot ROM).
# romsearch.rs looks under <prefix>/share/copperline/aros relative to the
# binary; in the AppImage that resolves to usr/share/copperline/aros.
install -Dm644 assets/aros/aros-amiga-m68k-rom.bin \
  "$appdir/usr/share/copperline/aros/aros-amiga-m68k-rom.bin"
install -Dm644 assets/aros/aros-amiga-m68k-ext.bin \
  "$appdir/usr/share/copperline/aros/aros-amiga-m68k-ext.bin"
install -Dm644 assets/aros/LICENSE \
  "$appdir/usr/share/copperline/aros/LICENSE"

# Bundled open CD32 FMV cartridge ROM (the CD32 profile default).
install -Dm644 assets/fmv/copperline-fmv.rom \
  "$appdir/usr/share/copperline/fmv/copperline-fmv.rom"
install -Dm644 assets/fmv/README.md \
  "$appdir/usr/share/copperline/fmv/README.md"

# Bundled open-source A4091 autoboot ROM (default when a config fits an A4091
# without naming a ROM); romsearch.rs looks under share/copperline/a4091.
install -Dm644 assets/a4091/a4091_cdfs.rom \
  "$appdir/usr/share/copperline/a4091/a4091_cdfs.rom"
install -Dm644 assets/a4091/README.md \
  "$appdir/usr/share/copperline/a4091/README.md"
install -Dm644 assets/a4091/THIRD_PARTY_NOTICES.txt \
  "$appdir/usr/share/copperline/a4091/THIRD_PARTY_NOTICES.txt"

# Copperline's open A2091/A590 autoboot ROM.
install -Dm644 assets/a2091/copperline-a2091.rom \
  "$appdir/usr/share/copperline/a2091/copperline-a2091.rom"
install -Dm644 assets/a2091/README.md \
  "$appdir/usr/share/copperline/a2091/README.md"
install -Dm644 assets/a2091/THIRD_PARTY_NOTICES.txt \
  "$appdir/usr/share/copperline/a2091/THIRD_PARTY_NOTICES.txt"

# Bundled open-source lide.device autoboot ROM and CD-filesystem bank
# (default for a fitted [lide] board without a named rom/rom_bank2);
# romsearch.rs looks under share/copperline/lide.
install -Dm644 assets/lide/lide.rom \
  "$appdir/usr/share/copperline/lide/lide.rom"
install -Dm644 assets/lide/lide-atbus.rom \
  "$appdir/usr/share/copperline/lide/lide-atbus.rom"
install -Dm644 assets/lide/cdfs.rom \
  "$appdir/usr/share/copperline/lide/cdfs.rom"
install -Dm644 assets/lide/README.md \
  "$appdir/usr/share/copperline/lide/README.md"
install -Dm644 assets/lide/THIRD_PARTY_NOTICES.txt \
  "$appdir/usr/share/copperline/lide/THIRD_PARTY_NOTICES.txt"

# WHDLoad support archives (direct WHDLoad boot, src/whdload.rs); fetched
# with pinned checksums, shipped unmodified with their provenance README.
# whdload::find_whdboot_assets looks under share/copperline/whdboot.
tools/fetch-whdload.sh
install -Dm644 assets/whdboot/WHDLoad_usr.lha \
  "$appdir/usr/share/copperline/whdboot/WHDLoad_usr.lha"
install -Dm644 assets/whdboot/skick346.lha \
  "$appdir/usr/share/copperline/whdboot/skick346.lha"
install -Dm644 assets/whdboot/README.md \
  "$appdir/usr/share/copperline/whdboot/README.md"

# Desktop integration metadata, shared with the Flatpak build.
install -Dm644 "$flatpak_meta/dev.copperline.Copperline.desktop" \
  "$appdir/usr/share/applications/dev.copperline.Copperline.desktop"
install -Dm644 "$flatpak_meta/dev.copperline.Copperline.metainfo.xml" \
  "$appdir/usr/share/metainfo/dev.copperline.Copperline.metainfo.xml"
install -Dm644 assets/brand/copperline-icon.png \
  "$appdir/usr/share/icons/hicolor/256x256/apps/dev.copperline.Copperline.png"

echo "==> Fetching linuxdeploy"
mkdir -p "$tools_dir"
if [ ! -x "$linuxdeploy" ]; then
  curl -fsSL -o "$linuxdeploy" \
    "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-$arch.AppImage"
  chmod +x "$linuxdeploy"
fi

echo "==> Building AppImage"
export VERSION="${VERSION:-$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)}"
# OUTPUT controls the final file name; default mirrors the Homebrew/version
# convention so release assets are self-describing.
export OUTPUT="${OUTPUT:-Copperline-$VERSION-$arch.AppImage}"

"$linuxdeploy" \
  --appdir "$appdir" \
  --executable "$appdir/usr/bin/copperline" \
  --desktop-file "$appdir/usr/share/applications/dev.copperline.Copperline.desktop" \
  --icon-file "$appdir/usr/share/icons/hicolor/256x256/apps/dev.copperline.Copperline.png" \
  --output appimage

# Standalone host-helper companion for Flatpak users (and native installs
# that do not use the AppImage). It contains no emulator or ROM assets.
helper_bundle="Copperline-$VERSION-$arch-net-helper"
helper_stage="$repo_root/target/$helper_bundle"
rm -rf "$helper_stage"
mkdir -p "$helper_stage"
install -m755 target/release/copperline-net-helper \
  "$helper_stage/copperline-net-helper"
install -m755 packaging/linux/copperline-net-helper-setup \
  "$helper_stage/copperline-net-helper-setup"
install -m644 packaging/linux/copperline-net-helper.service \
  "$helper_stage/copperline-net-helper.service"
install -m644 packaging/linux/copperline-net-helper.socket \
  "$helper_stage/copperline-net-helper.socket"
tar -C "$repo_root/target" -czf "$repo_root/$helper_bundle.tar.gz" "$helper_bundle"

echo "==> Built $OUTPUT and $helper_bundle.tar.gz"
