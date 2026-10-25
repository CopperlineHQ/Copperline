# Release Checklist

Copperline is currently released as a source application rather than a
crates.io library, so the root package remains marked `publish = false`.

## Branches

Copperline has two long-lived branches:

- `main` is the default branch and the development line. New features, and
  anything that breaks compatibility, land here for the next major release.
  Nothing is released from `main`: when that release is ready for its first
  beta, cut `v2` from `main` and tag the beta there.
- `v1` is the 1.x maintenance branch, cut from `main` after 1.0.0. Fixes land
  here, and 1.x releases are tagged from it.

Fixes reach `main` by merging `v1` forward, not by cherry-picking:

```sh
git checkout main
git merge --no-ff v1
```

Merge forward after each fix lands on `v1` and after every release from it,
so `git log main..v1` stays empty between merges and no fix is left behind.
The exception is a fix that was made on `main` first and also affects 1.x:
cherry-pick it onto a branch from `v1` with `git cherry-pick -x`, so the
commit records where it came from, and open the pull request against `v1`.

What may land on `v1`: bug fixes, documentation corrections, packaging fixes,
dependency patch and security updates, and small additions that change no
existing behaviour. What may not: anything that stops existing
configurations, save states or scripts working the same way. That rules out
removed or renamed configuration keys and command-line flags, changed
defaults, save-state chunk version bumps (an additive `#[serde(default)]`
field is fine), netplay wire protocol changes, MSRV bumps and new major or
minor series of dependencies. Dependabot opens grouped patch updates against
`v1`; it raises security updates against `main` only, so port those by hand
(`cargo deny` in CI on `v1` flags the advisory).

Resolving a forward merge:

- `Cargo.toml` and the lockfiles: a release bump on `v1` edits the version
  line that `main` also owns. Keep `main`'s version.
- `packaging/linux/dev.copperline.Copperline.metainfo.xml`: keep both
  sides, so `main` lists every release.
- `Formula/copperline.rb`: take `v1`'s while `v1` is the newest stable line.
  The tap serves the formula from the default branch, so the forward merge is
  what moves `brew upgrade` users to a 1.x release. `brew install --HEAD`
  builds `main`.
- Golden renders (`timing-test/golden/`, `tests/golden/`): when both sides
  changed one, re-bless on the merge result rather than picking a side (see
  `timing-test/README.md`).

To release from `v1`, follow the rest of this checklist on `v1`: bump the
version, run the checks, push the branch, tag, then commit the Homebrew
update there and merge forward. Push the release commit to `v1` before the
tag: the `Browser demo` and `Docs site HTML` workflows publish to
copperline.dev only when the tagged commit is already on a `vN` release
branch, and skip any other `v*` tag with a notice (the release-asset
workflows still run for it). Once two release branches are live, such as
`v1` alongside a `v2` beta, the site shows whichever of their tags was pushed
last; to switch it back, run those two workflows by hand (Actions tab, "Use
workflow from") on the tag the site should show.

The push-triggered workflows run on both branches, and pull requests are
checked whichever branch they target. Those workflows save Rust build caches
only on `main`; on `v1` they restore the default branch's. (A manual run of
the nightly `cputest` workflow on `v1` does save a cache scoped to `v1`.)

## Before Creating the Public Repository

1. Create the public repository from a clean tree with rewritten history.
2. Confirm the tracked tree has no copyrighted ROM, disk, hard-disk, or CD
   images:

   ```sh
   git status --short
   git ls-files | rg -n '\.(rom|ROM|adf|ADF|adz|ADZ|dms|DMS|hdf|HDF|scp|SCP|cue|CUE|bin|BIN|iso|ISO|u12|U12|u13|U13|wasm|WASM|png|jpg|jpeg|gif|pdf|zip|7z|lha|lzx)$'
   ```

   Expected tracked binary files are:

   - `assets/brand/*.png`
   - `assets/aros/aros-amiga-m68k-rom.bin` and
     `assets/aros/aros-amiga-m68k-ext.bin`, the bundled AROS boot ROMs
     (APL-licensed; see `assets/aros/README.md`)
   - `assets/fmv/copperline-fmv.rom`, the GPLv3+ open CD32 Full Motion Video
     cartridge ROM built reproducibly from `fmv-rom/` (see
     `assets/fmv/README.md`)
   - `assets/services/services_rom.bin`, the guest-side host-filesystem
     handler built from `guest/services/`
   - `assets/hrtmon/hrtmon.rom`, HRTMon 2.39 assembled reproducibly from
     the upstream source by `hrtmon-rom/build.sh` (GPL-2.0-or-later; see
     `assets/hrtmon/README.md` and `LICENSE`)
   - `assets/a4091/a4091_cdfs.rom`, the upstream A4091 v42.39 autoboot ROM
     (mixed redistribution notices and an exact component inventory are in
     `assets/a4091/THIRD_PARTY_NOTICES.txt`)
   - `assets/hostsocket/hostsocket_rom.bin` and
     `assets/hostsocket/hostsocket_plugin.wasm`, the bundled HostSocket board
     artifacts built from `guest/hostsocket/` and `crates/hostsocket-plugin/`
   - `docs/images/*.png`; review provenance before release when these change
   - `timing-test/*.bin` probe programs: `boot.bin`, `test.bin`,
     `audprobe-*`, `bfprobe`, `bltprobe-*`, `bplprobe-*`, `clxprobe`,
     `dblpal-*`, `ddfprobe-*`, `fwdprobe`, `hamprobe-*`, `probesrv`,
     `rdprobe`, `regprobe-*`, and `sprprobe-*`; each is built from its
     adjacent `.asm`
   - `timing-test/golden/*.png`, the blessed golden renders for
     `tests/probe_golden.rs`
3. Confirm local assets are still ignored:

   ```sh
   git check-ignore -v KICK13.ROM AmigaTestKit.adf cdtv_single.bin
   ```

## Version bump

`crates/copperline-web`, `crates/copperline-player`,
`crates/copperline-libretro`, `crates/cputest-runner`, and
`crates/hostsocket-plugin` are separate workspaces with their own committed
`Cargo.lock` files, so a root build never refreshes them. The web, player,
and libretro crates pin the root `copperline` version by path; the cputest
runner independently pins the published `m68k` dependency, and the HostSocket workspace pins the
dependency graph used to build the committed
`assets/hostsocket/hostsocket_plugin.wasm`. If a manifest changes without
regenerating its matching nested lock, every `cargo build --locked` in that
crate fails at the release tag, and tags are immutable so the breakage cannot
be fixed after the fact (issue #219: the `v0.12.0` tag shipped
`crates/copperline-web/Cargo.lock` still pinning `copperline 0.11.0`).

In the same commit as any version bump, resync the affected nested locks
and commit them:

```sh
(cd crates/copperline-web && cargo update -p copperline) # root version bump
(cd crates/copperline-player && cargo update -p copperline)
(cd crates/copperline-libretro && cargo update -p copperline)
(cd crates/cputest-runner && cargo update -p m68k)       # m68k requirement change
(cd crates/hostsocket-plugin && cargo update)            # its manifest/dependencies change
```

The `Lockfile sync` workflow (`.github/workflows/locks.yml`) runs these
checks on every pull request, push to `main` or `v1`, and `v*` tag push, so a tag
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
(cd crates/copperline-player && cargo tree --locked > /dev/null)
(cd crates/copperline-libretro && cargo tree --locked > /dev/null)
(cd crates/cputest-runner && cargo tree --locked > /dev/null)
(cd crates/hostsocket-plugin && cargo tree --locked > /dev/null)
```

Build the documentation:

```sh
cd docs
myst build --html --ci --strict --check-links
python3 ../tools/build-docs-pdf.py --ci --strict
test -s _build/exports/copperline.pdf
```

On a `v*` tag the `Docs release PDF` workflow rebuilds the PDF and attaches
`Copperline-X.Y.Z-manual.pdf` to the GitHub Release automatically (the
everyday PDF build check stays in the `docs` job in ci.yml). To back-fill a
release whose tag predates the workflow, run it from the Actions tab against
the branch the release was tagged from, with `release_tag` set to that tag.

## Homebrew formula

This repository is its own Homebrew tap (`Formula/copperline.rb`). After
tagging a release, update the formula so `brew install copperline` picks up
the new version:

```sh
VER=X.Y.Z
curl -fsSL "https://github.com/CopperlineHQ/Copperline/archive/refs/tags/v$VER.tar.gz" | shasum -a 256
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
brew audit --strict --formula copperlinehq/copperline/copperline
brew upgrade --build-from-source copperlinehq/copperline/copperline
brew test copperlinehq/copperline/copperline
```

## Linux: AppImage

Linux releases ship as an **AppImage** (`packaging/appimage/`). Add a
`<release>` entry to `packaging/linux/dev.copperline.Copperline.metainfo.xml`
when bumping the version. The `AppImage` workflow builds it on `ubuntu-22.04`
and, on a `v*` tag, attaches
`Copperline-X.Y.Z-<arch>.AppImage` to the GitHub Release automatically. To
build one by hand on a Linux host:

```sh
./packaging/appimage/build-appimage.sh
```

## Windows

Windows distribution includes an installer and a portable ZIP per
architecture (`packaging/windows/`). The `Windows` workflow builds them
natively on `windows-latest` (x86-64) and `windows-11-arm` (ARM64) and, on a `v*` tag,
attaches `Copperline-X.Y.Z-win-x64-setup.exe`,
`Copperline-X.Y.Z-win-arm64-setup.exe`, `Copperline-X.Y.Z-win-x64.zip` and
`Copperline-X.Y.Z-win-arm64.zip` to the GitHub Release automatically. The
same workflow runs the full release build on pull requests that touch the
code, so it doubles as the Windows build check for both architectures (the
main CI runs on macOS and Linux).

The zips are self-contained: the MSVC C runtime is linked statically (see
`.cargo/config.toml`) so they need no Visual C++ Redistributable, and the
bundled AROS ROM sits in a sibling `aros\` folder that `romsearch.rs` probes
first; the other bundled ROM assets (`fmv\`, `a4091\`, `a2091\`, `lide\`,
`hrtmon\`) are staged beside it the same way. To build one by hand on a
Windows host (add
`-Target aarch64-pc-windows-msvc` for the ARM64 zip):

```pwsh
packaging/windows/build-zip.ps1
```

The installer wraps that exact ZIP payload using Inno Setup 6.3 or newer
(preinstalled on both GitHub runner images). To build both formats locally:

```pwsh
packaging/windows/build-installer.ps1
# Or reuse a ZIP already built from this checkout:
packaging/windows/build-installer.ps1 -ZipPath Copperline-X.Y.Z-win-x64.zip
```

Pass `-Target aarch64-pc-windows-msvc` for ARM64. The installer is per-user,
defaults to `%LOCALAPPDATA%\Programs\Copperline`, adds Start menu and optional
desktop shortcuts, and registers an uninstaller. Its stable Inno `AppId`
allows later installers to update the same installation. Uninstall removes
only shipped files and shortcuts, preserving user-created configuration,
saves and media. The installer is unsigned, like the portable executables.

CI silently installs each architecture on its native runner, checks every
installed payload file against the ZIP, verifies shortcuts, reinstalls to
restore a missing tool while preserving user files, boots the installed
emulator on the bundled AROS ROM from outside the checkout, starts both
command-line tools, then uninstalls and checks removal and data preservation.
Logs and the boot screenshot are workflow artifacts. The release job waits
for these checks as well as ZIP boot smoke, unit tests and golden renders
on both architectures before attaching all four packages. Manual runs with
`release_tag` also attach both formats to an existing release.

The installer smoke test requires PowerShell 7. To reproduce it on a clean
Windows VM, install PowerShell 7 first:

```pwsh
winget install --id Microsoft.PowerShell --source winget
```

Then run the test with `pwsh`:

```pwsh
pwsh -File packaging/windows/test-installer.ps1 `
  -InstallerPath Copperline-X.Y.Z-win-x64-setup.exe `
  -ZipPath Copperline-X.Y.Z-win-x64.zip `
  -WorkDir "$env:TEMP\copperline-installer-smoke"
```

The test refuses an existing installation and keeps logs, the screenshot
and preserved user-file fixtures for inspection.

## macOS disk image

The prebuilt macOS download is a disk image (`packaging/macos/`): a
drag-to-Applications `Copperline.app` wrapped in a `.dmg`. The `macOS` workflow
builds each architecture's binary on its own `macos-latest` runner, joins them
into the bundle in a package job, and, on a `v*` tag, attaches
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

## Libretro cores

The `Libretro` workflow builds and tests Linux x86-64, macOS Apple Silicon,
and Windows x86-64 cores. On a `v*` tag, it attaches a versioned ZIP for
each platform to the GitHub Release. Each ZIP includes the core, its
`.info` file, WHDLoad support archives, licences, and installation notes.
These cores are loaded manually in RetroArch; see `docs/guide/libretro.md`.

Before announcing the release, confirm the DMG, AppImage, network helper,
both Windows installers and ZIPs, PDF manual, and all three libretro ZIPs are
attached, and that the browser and documentation publishing workflows have passed.

## Crate packaging

`cargo package --offline` can be used to inspect and verify the source archive
layout after the dependencies are cached. The published `m68k` dependency is
crates.io-compatible and needs no path-dependency exception.
