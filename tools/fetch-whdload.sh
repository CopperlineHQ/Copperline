#!/bin/sh
# Fetch the WHDLoad support archives into assets/whdboot/ for the direct
# WHDLoad boot feature (src/whdload.rs, --whdload).
#
# Both archives are freely redistributable and are fetched unmodified from
# their canonical homes; neither is committed to the repository (only
# assets/whdboot/README.md is). Release bundles run this at packaging time
# (packaging/macos/build-dmg.sh, packaging/appimage/build-appimage.sh; the
# Windows build fetches in packaging/windows/build-zip.ps1, and the Flatpak
# manifest and Homebrew formula pin the same URLs + checksums in their own
# formats), so end users never need it. Developers run it once:
#
#   tools/fetch-whdload.sh
#
# Checksums are pinned. whdload.de publishes WHDLoad_usr.lha as a rolling
# "current release" file, so a new upstream WHDLoad release changes the
# archive and this script then fails its verification: that is the signal to
# review the new release and bump the hash here AND in the other pin sites
# listed above (grep for the old hash).
set -eu

cd "$(dirname "$0")/.."
dest="assets/whdboot"

WHDLOAD_URL="https://whdload.de/whdload/WHDLoad_usr.lha"
WHDLOAD_SHA256="093333953737528d79c1eda7d21a16a0aa298698722624e7cfb31f588a0a156d"
SKICK_URL="https://aminet.net/util/boot/skick346.lha"
SKICK_SHA256="02b4d01852d12ab391c6469064f917221a0f7319fd0b3ba6c359403ec1d59f96"

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

fetch() {
    url="$1"
    sha="$2"
    file="$dest/$(basename "$url")"
    if [ -f "$file" ] && [ "$(sha256_of "$file")" = "$sha" ]; then
        echo "up to date: $file"
        return 0
    fi
    echo "fetching $url"
    curl -fsSL -o "$file.tmp" "$url"
    got="$(sha256_of "$file.tmp")"
    if [ "$got" != "$sha" ]; then
        rm -f "$file.tmp"
        echo "error: $url checksum mismatch" >&2
        echo "  expected $sha" >&2
        echo "  got      $got" >&2
        echo "If upstream released a new version, review it and update the" >&2
        echo "pinned checksums (see the header of this script)." >&2
        exit 1
    fi
    mv "$file.tmp" "$file"
    echo "fetched: $file"
}

mkdir -p "$dest"
fetch "$WHDLOAD_URL" "$WHDLOAD_SHA256"
fetch "$SKICK_URL" "$SKICK_SHA256"
