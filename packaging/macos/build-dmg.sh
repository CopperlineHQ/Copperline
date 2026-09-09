#!/usr/bin/env bash
# Build a Copperline macOS disk image: a drag-to-Applications Copperline.app
# wrapped in a .dmg. Run from a macOS host (or CI); see
# .github/workflows/macos.yml.
#
# What it does:
#   1. Builds the release binary for both Apple architectures with the pinned
#      dependency graph and lipo-joins them into one universal binary, so a
#      single download runs natively on Apple Silicon and Intel. With
#      PREBUILT_DIR set the cargo step is skipped and the per-architecture
#      binaries are taken from that directory instead (the macOS workflow
#      compiles each slice on its own runner and packages them here).
#   2. Stages a Copperline.app bundle: the universal binary in Contents/MacOS
#      beside the copperline-ctl (control protocol / MCP / DAP client) and
#      copperline-import-uae (config converter) companions, lipo-joined the
#      same way; the icon and AROS ROM in Contents/Resources. romsearch.rs
#      probes a bundle's Contents/Resources/aros first, so the bundled AROS
#      ROM is found with no configuration, and copperline-ctl launches the
#      copperline next to it.
#   3. Ad-hoc code-signs the bundle. lipo strips the per-slice signatures Rust
#      attaches on macOS, and an unsigned arm64 binary will not launch on Apple
#      Silicon at all, so a signature is mandatory even when it is ad-hoc.
#   4. Lays out a .dmg with the app and an Applications symlink, named
#      Copperline-<version>-macos-universal.dmg to mirror the
#      AppImage/Windows/Homebrew version naming so release assets are
#      self-describing.
#
# This build is intentionally NOT signed with a Developer ID or notarized, so
# first launch trips Gatekeeper; packaging/macos/README.txt (shipped in the
# image) explains the right-click-Open workaround.
#
# Override knobs (env):
#   MACOS_UNIVERSAL=0   build only the host architecture (faster local builds)
#   PREBUILT_DIR=<dir>  skip the cargo build and lipo <dir>/<target>/<binary>
#                       for each target and shipped binary instead
#   OUTPUT=<path>       final .dmg file name
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
cd "$repo_root"

version="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
app_name="Copperline.app"
stage="$repo_root/target/macos-dmg"
app="$stage/$app_name"
output="${OUTPUT:-$repo_root/Copperline-$version-macos-universal.dmg}"

# Architectures to build. Default is universal (both); MACOS_UNIVERSAL=0 builds
# only the host arch for a faster local turnaround.
if [ "${MACOS_UNIVERSAL:-1}" = "0" ]; then
  case "$(uname -m)" in
    arm64) targets=(aarch64-apple-darwin) ;;
    *) targets=(x86_64-apple-darwin) ;;
  esac
else
  targets=(aarch64-apple-darwin x86_64-apple-darwin)
fi

# Executables that ship in Contents/MacOS: the emulator and its command-line
# companions (all default-feature binaries of one cargo build).
binaries=(copperline copperline-ctl copperline-import-uae)

# Per-target build directories holding those binaries: either built here or
# supplied prebuilt.
slice_dirs=()
if [ -n "${PREBUILT_DIR:-}" ]; then
  echo "==> Using prebuilt binaries from $PREBUILT_DIR (${targets[*]})"
  for target in "${targets[@]}"; do
    # Catch a slice staged under the wrong target name before lipo would
    # happily join two copies of the same architecture.
    case "$target" in
      aarch64-apple-darwin) want=arm64 ;;
      x86_64-apple-darwin) want=x86_64 ;;
      *) want="" ;;
    esac
    for name in "${binaries[@]}"; do
      bin="$PREBUILT_DIR/$target/$name"
      if [ ! -f "$bin" ]; then
        echo "error: no prebuilt binary at $bin" >&2
        exit 1
      fi
      got="$(lipo -archs "$bin")"
      if [ -n "$want" ] && [ "$got" != "$want" ]; then
        echo "error: $bin is $got, expected $want for $target" >&2
        exit 1
      fi
    done
    slice_dirs+=("$PREBUILT_DIR/$target")
  done
else
  echo "==> Building release binaries (${targets[*]})"
  for target in "${targets[@]}"; do
    # Idempotent; ensures hand-builds on a fresh checkout have the cross target.
    if command -v rustup >/dev/null 2>&1; then
      rustup target add "$target" >/dev/null
    fi
    cargo build --release --locked --target "$target"
    slice_dirs+=("target/$target/release")
  done
fi

echo "==> Staging $app_name"
rm -rf "$stage"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/aros" \
  "$app/Contents/Resources/a2091" "$app/Contents/Resources/a4091" \
  "$app/Contents/Resources/lide"

# Universal binaries from the per-arch slices (a single-arch lipo is a no-op
# copy). copperline-ctl looks for the emulator next to itself, so all three
# land in Contents/MacOS together; codesign --deep below signs each of them.
for name in "${binaries[@]}"; do
  bins=()
  for dir in "${slice_dirs[@]}"; do
    bins+=("$dir/$name")
  done
  lipo -create -output "$app/Contents/MacOS/$name" "${bins[@]}"
done

# Info.plist with the version substituted in; plutil -lint catches a botched
# substitution before the bundle ships.
sed "s/@VERSION@/$version/g" "$here/Info.plist.in" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null

# Classic four-character package signature; harmless but expected by some tools.
printf 'APPL????' > "$app/Contents/PkgInfo"

cp "assets/brand/copperline.icns" "$app/Contents/Resources/copperline.icns"

# Bundled AROS open-source Kickstart replacement (the default boot ROM).
# romsearch.rs resolves Contents/Resources/aros relative to the executable.
# Ship the license/readme/acknowledgements next to the ROM halves as
# redistribution requires.
for f in \
  aros-amiga-m68k-rom.bin \
  aros-amiga-m68k-ext.bin \
  LICENSE \
  README.md \
  ACKNOWLEDGEMENTS; do
  cp "assets/aros/$f" "$app/Contents/Resources/aros/$f"
done

# Bundled open CD32 FMV cartridge ROM (the CD32 profile default).
mkdir -p "$app/Contents/Resources/fmv"
for f in copperline-fmv.rom README.md; do
  cp "assets/fmv/$f" "$app/Contents/Resources/fmv/$f"
done

# Bundled open-source A4091 autoboot ROM (default when a config fits an A4091
# without naming a ROM); romsearch.rs resolves Contents/Resources/a4091. Keep
# its exact provenance, component inventory, and redistribution notices beside
# the binary artifact.
for f in a4091_cdfs.rom README.md THIRD_PARTY_NOTICES.txt; do
  cp "assets/a4091/$f" "$app/Contents/Resources/a4091/$f"
done

# Copperline's open A2091/A590 autoboot ROM.
for f in copperline-a2091.rom README.md THIRD_PARTY_NOTICES.txt; do
  cp "assets/a2091/$f" "$app/Contents/Resources/a2091/$f"
done

# Bundled open-source lide.device autoboot ROM and CD-filesystem bank
# (default for a fitted [lide] board without a named rom/rom_bank2);
# romsearch.rs resolves Contents/Resources/lide.
for f in lide.rom lide-atbus.rom cdfs.rom README.md THIRD_PARTY_NOTICES.txt; do
  cp "assets/lide/$f" "$app/Contents/Resources/lide/$f"
done

# Bundled HRTMon freezer-cartridge image (default for [cartridge] model =
# "hrtmon" without a named rom); romsearch.rs resolves
# Contents/Resources/hrtmon. GPL-2.0-or-later: ship its notice and license.
mkdir -p "$app/Contents/Resources/hrtmon"
for f in hrtmon.rom README.md LICENSE; do
  cp "assets/hrtmon/$f" "$app/Contents/Resources/hrtmon/$f"
done

# WHDLoad support archives (direct WHDLoad boot, src/whdload.rs); fetched
# with pinned checksums, shipped unmodified with their provenance README.
# whdload::find_whdboot_assets resolves Contents/Resources/whdboot.
tools/fetch-whdload.sh
mkdir -p "$app/Contents/Resources/whdboot"
for f in WHDLoad_usr.lha skick346.lha README.md; do
  cp "assets/whdboot/$f" "$app/Contents/Resources/whdboot/$f"
done

echo "==> Ad-hoc signing $app_name"
# --deep so the nested executable is signed too; "-" selects the ad-hoc
# identity (no Developer ID needed). This is what lets the universal binary
# launch on Apple Silicon; it does not satisfy notarization, so downloads are
# still Gatekeeper-quarantined (see README.txt).
cp assets/egui/THIRD_PARTY_FONTS.txt "$app/Contents/Resources/THIRD_PARTY_FONTS.txt"
# --deep covers the bundle's main executable and nested bundles, but not
# reliably the extra Mach-O tools beside it in Contents/MacOS, and an
# unsigned arm64 copperline-ctl would be killed on launch: sign each one.
for name in "${binaries[@]}"; do
  codesign --force --sign - "$app/Contents/MacOS/$name"
done
codesign --force --deep --sign - "$app"

echo "==> Laying out disk image contents"
# Top-level docs alongside the app, mirroring the Windows zip: an Applications
# shortcut for drag-installs, the README's Gatekeeper note, a starter config,
# and the Copperline license that must accompany the binary.
ln -s /Applications "$stage/Applications"
cp "$here/README.txt" "$stage/README.txt"
cp "copperline.example.toml" "$stage/copperline.example.toml"
cp "LICENSE" "$stage/LICENSE.txt"

echo "==> Building $(basename "$output")"
rm -f "$output"
hdiutil create \
  -volname "Copperline $version" \
  -srcfolder "$stage" \
  -fs HFS+ \
  -format UDZO \
  -ov \
  "$output" >/dev/null

echo "==> Built $output"
