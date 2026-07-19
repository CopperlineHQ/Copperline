# Release Checklist

Copperline is currently released from source. The crate is marked
`publish = false` because it depends on a patched vendored copy of `m68k`;
resolve that dependency story before attempting a crates.io release.

## Before Creating the Public Repository

1. Create the public repository from a clean tree with rewritten history.
2. Confirm the tracked tree has no copyrighted ROM, disk, hard-disk, or CD
   images:

   ```sh
   git status --short
   git ls-files | rg -n '\.(rom|ROM|adf|ADF|adz|ADZ|dms|DMS|hdf|HDF|scp|SCP|cue|CUE|bin|BIN|iso|ISO|u12|U12|u13|U13|png|jpg|jpeg|gif|pdf|zip|7z|lha|lzx)$'
   ```

   Expected tracked binary files are:

   - `assets/brand/*.png`
   - `assets/aros/aros-amiga-m68k-rom.bin` and
     `assets/aros/aros-amiga-m68k-ext.bin`, the bundled AROS boot ROMs
     (APL-licensed; see `assets/aros/README.md`)
   - `assets/services/services_rom.bin`, the guest-side host-filesystem
     handler built from `guest/services/`
   - `docs/images/*.png`; review provenance before release when these change
   - `timing-test/*.bin` probe programs (`boot.bin`, `test.bin`, the
     `ddfprobe-*`/`bltprobe-*`/`audprobe-*`/`clxprobe`/`regprobe-*`/
     `sprprobe-*` bootblocks), each built from its adjacent `.asm`
   - `timing-test/golden/*.png`, the blessed golden renders for
     `tests/probe_golden.rs`
   - `crates/m68k/tests/fixtures/extra/**/bin/*.bin`, built from the
     adjacent assembly sources under sibling `src/` directories

3. Confirm local assets are still ignored:

   ```sh
   git check-ignore -v KICK13.ROM AmigaTestKit.adf cdtv_single.bin
   ```

## Version bump

`crates/copperline-web` and `crates/cputest-runner` are separate workspaces
with their own committed `Cargo.lock` files, so a root build never refreshes
them (`crates/m68k` is also its own workspace, but its lock is deliberately
gitignored, so it cannot drift). Both pin sibling crates by path:
`copperline-web` locks the root `copperline` version and `cputest-runner`
locks the `copperline-m68k` version. If either version is bumped without
regenerating the matching nested lock, every `cargo build --locked` in that
crate fails at the release tag, and tags are immutable so the breakage
cannot be fixed after the fact (issue #219: the `v0.12.0` tag shipped
`crates/copperline-web/Cargo.lock` still pinning `copperline 0.11.0`).

In the same commit as any version bump, resync the affected nested locks
and commit them:

```sh
(cd crates/copperline-web && cargo update -p copperline)      # root version bump
(cd crates/cputest-runner && cargo update -p copperline-m68k) # crates/m68k bump
```

The `Lockfile sync` workflow (`.github/workflows/locks.yml`) runs these
checks on every pull request, push to `main`, and `v*` tag push, so a tag
cut from a commit with a stale lock turns red within about a minute --
delete and re-cut the tag if that happens. The pre-tag check below is still
part of the checklist so the drift never reaches the tag at all; the
v0.12.0 breakage happened precisely because the tag was cut from a fresh
bump commit before any CI had run against it.

## Checks

Run these before tagging a source release:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
```

Confirm every nested workspace lockfile is in sync (fails if a version bump
missed one; see "Version bump" above):

```sh
(cd crates/copperline-web && cargo tree --locked > /dev/null)
(cd crates/cputest-runner && cargo tree --locked > /dev/null)
```

Build the documentation:

```sh
cd docs
myst build --html --ci --strict --check-links
myst build --pdf --ci --strict
test -s _build/exports/copperline.pdf
```

On a `v*` tag the `Docs release PDF` workflow rebuilds the PDF and attaches
`Copperline-X.Y.Z-manual.pdf` to the GitHub Release automatically (the
everyday PDF build check stays in the `docs` job in ci.yml). To back-fill a
release whose tag predates the workflow, run it from the Actions tab against
`main` with `release_tag` set to that tag.

## Homebrew formula

This repository is its own Homebrew tap (`Formula/copperline.rb`). After
tagging a release, update the formula so `brew install copperline` picks up
the new version:

```sh
VER=X.Y.Z
curl -fsSL "https://github.com/LinuxJedi/Copperline/archive/refs/tags/v$VER.tar.gz" | shasum -a 256
```

Set `url` to the `v$VER.tar.gz` tarball and `sha256` to the printed digest.
Check the edited formula before committing:

```sh
ruby -c Formula/copperline.rb
brew style ./Formula/copperline.rb
```

After pushing the formula update, refresh the tap and smoke-test the named
formula. Recent Homebrew releases reject path-based formula audit/install
commands unless the formula is loaded from a tap.

```sh
brew update
brew audit --strict --formula linuxjedi/copperline/copperline
brew upgrade --build-from-source linuxjedi/copperline/copperline
brew test linuxjedi/copperline/copperline
```

## Linux: Flatpak and AppImage

Linux distribution uses two channels (see `packaging/`).

**Flatpak / Flathub** (`packaging/flatpak/`) is the primary channel. After a
release commit lands, refresh the vendored crate list if dependencies changed
and point the manifest at the tag:

```sh
./packaging/flatpak/generate-cargo-sources.sh   # if Cargo.lock changed
```

Set `tag:` and `commit:` in `dev.copperline.Copperline.yaml` to the release,
add a `<release>` entry to `dev.copperline.Copperline.metainfo.xml`, then push
the same change to the `flathub/dev.copperline.Copperline` repository (the
Flathub app repo created at first acceptance). The `Flatpak` workflow builds
and lints the bundle the same way Flathub does. First-time submission steps are
in `packaging/flatpak/README.md`.

**AppImage** (`packaging/appimage/`) is the no-install fallback. The `AppImage`
workflow builds it on `ubuntu-22.04` and, on a `v*` tag, attaches
`Copperline-X.Y.Z-<arch>.AppImage` to the GitHub Release automatically. To
build one by hand on a Linux host:

```sh
./packaging/appimage/build-appimage.sh
```

## Windows

Windows distribution is a portable zip (`packaging/windows/`). The `Windows`
workflow builds it on `windows-latest` and, on a `v*` tag, attaches
`Copperline-X.Y.Z-win-x64.zip` to the GitHub Release automatically. The same
workflow runs the full release build on pull requests that touch the code, so
it doubles as the Windows build check (the main CI runs on macOS only).

The zip is self-contained: the MSVC C runtime is linked statically (see
`.cargo/config.toml`) so it needs no Visual C++ Redistributable, and the
bundled AROS ROM sits in a sibling `aros\` folder that `romsearch.rs` probes
first. To build one by hand on a Windows host:

```pwsh
packaging/windows/build-zip.ps1
```

## macOS disk image

The prebuilt macOS download is a disk image (`packaging/macos/`): a
drag-to-Applications `Copperline.app` wrapped in a `.dmg`. The `macOS` workflow
builds it on `macos-latest` and, on a `v*` tag, attaches
`Copperline-X.Y.Z-macos-universal.dmg` to the GitHub Release automatically.
Homebrew (above) remains the build-from-source channel; the disk image is the
no-compiler alternative.

The app bundle is a universal binary (the workflow builds both
`aarch64-apple-darwin` and `x86_64-apple-darwin` and `lipo`-joins them), so one
download runs natively on Apple Silicon and Intel. The bundled AROS ROM lives in
`Contents/Resources/aros`, which `romsearch.rs` probes, so it runs out of the
box. The image is ad-hoc signed (required for the arm64 slice to launch) but is
intentionally NOT Developer ID signed or notarized, so first launch trips
Gatekeeper; the right-click-Open workaround is in the image's `README.txt`. To
build one by hand on a macOS host:

```sh
./packaging/macos/build-dmg.sh
```

## Crate packaging

`cargo package --no-verify --offline` can be used to inspect the source
archive layout after the dependencies are cached. Do not use full package
verification as a release gate until the vendored `m68k` path dependency has
a crates.io-compatible replacement.
