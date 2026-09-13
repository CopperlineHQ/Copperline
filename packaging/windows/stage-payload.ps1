#!/usr/bin/env pwsh
# Build Copperline for a Windows target and stage the shipping payload into a
# directory: copperline.exe, its command-line companions, the bundled ROM
# assets in the sibling directories romsearch.rs probes, the WHDLoad support
# archives, and the top-level docs.
#
# This is the one definition of what a Windows release contains. Both
# packaging routes call it and then wrap the same directory: build-zip.ps1
# compresses it into the portable zip, and msix/build-msix.ps1 packs it into
# the Microsoft Store package. Anything added here ships in both.
param(
    [string]$Target = "x86_64-pc-windows-msvc",
    [Parameter(Mandatory = $true)][string]$Stage
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $here "..\..")).Path
Set-Location $repoRoot

& (Join-Path $here "enable-clang.ps1") -Target $Target

Write-Host "==> Building release binary ($Target)"
cargo build --release --locked --target $Target
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

Write-Host "==> Staging payload in $Stage"
if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
$arosDir = Join-Path $Stage "aros"
New-Item -ItemType Directory -Force -Path $arosDir | Out-Null

Copy-Item "target\$Target\release\copperline.exe" (Join-Path $Stage "copperline.exe")
# Command-line companions built by the same cargo invocation (both are
# default-feature binaries): the control-protocol / MCP / DAP client that the
# VS Code extension and coding agents run, and the WinUAE/Amiberry/FS-UAE
# config converter. copperline-ctl finds the emulator next to itself, so the
# three must stay siblings.
foreach ($tool in @("copperline-ctl.exe", "copperline-import-uae.exe")) {
    Copy-Item "target\$Target\release\$tool" (Join-Path $Stage $tool)
}
Copy-Item "assets\egui\THIRD_PARTY_FONTS.txt" (Join-Path $Stage "THIRD_PARTY_FONTS.txt")

# Bundled AROS open-source Kickstart replacement (the default boot ROM).
# romsearch.rs probes a sibling aros\ next to the executable first. Ship the
# license/readme/acknowledgements next to the ROM halves as redistribution
# requires.
foreach ($f in @(
    "aros-amiga-m68k-rom.bin",
    "aros-amiga-m68k-ext.bin",
    "LICENSE",
    "README.md",
    "ACKNOWLEDGEMENTS")) {
    Copy-Item "assets\aros\$f" (Join-Path $arosDir $f)
}

# Bundled open CD32 FMV cartridge ROM (the CD32 profile default).
$fmvDir = Join-Path $Stage "fmv"
New-Item -ItemType Directory -Force -Path $fmvDir | Out-Null
foreach ($f in @("copperline-fmv.rom", "README.md")) {
    Copy-Item (Join-Path "assets\fmv" $f) (Join-Path $fmvDir $f)
}

# Bundled open-source A4091 autoboot ROM (default when a config fits an A4091
# without naming a ROM); romsearch.rs probes a sibling a4091\ next to the exe.
$a4091Dir = Join-Path $Stage "a4091"
New-Item -ItemType Directory -Force -Path $a4091Dir | Out-Null
foreach ($f in @("a4091_cdfs.rom", "README.md", "THIRD_PARTY_NOTICES.txt")) {
    Copy-Item (Join-Path "assets\a4091" $f) (Join-Path $a4091Dir $f)
}

# Copperline's open A2091/A590 autoboot ROM.
$a2091Dir = Join-Path $Stage "a2091"
New-Item -ItemType Directory -Force -Path $a2091Dir | Out-Null
foreach ($f in @("copperline-a2091.rom", "README.md", "THIRD_PARTY_NOTICES.txt")) {
    Copy-Item (Join-Path "assets\a2091" $f) (Join-Path $a2091Dir $f)
}

# Bundled open-source lide.device autoboot ROM and CD-filesystem bank
# (default for a fitted [lide] board without a named rom/rom_bank2);
# romsearch.rs probes a sibling lide\ next to the exe.
$lideDir = Join-Path $Stage "lide"
New-Item -ItemType Directory -Force -Path $lideDir | Out-Null
foreach ($f in @("lide.rom", "lide-atbus.rom", "cdfs.rom", "README.md", "THIRD_PARTY_NOTICES.txt")) {
    Copy-Item (Join-Path "assets\lide" $f) (Join-Path $lideDir $f)
}

# Bundled HRTMon freezer-cartridge image (default for [cartridge] model =
# "hrtmon" / --cartridge hrtmon without a named rom); romsearch.rs probes a
# sibling hrtmon\ next to the exe. GPL-2.0-or-later: ship its notice and
# license.
$hrtmonDir = Join-Path $Stage "hrtmon"
New-Item -ItemType Directory -Force -Path $hrtmonDir | Out-Null
foreach ($f in @("hrtmon.rom", "README.md", "LICENSE")) {
    Copy-Item (Join-Path "assets\hrtmon" $f) (Join-Path $hrtmonDir $f)
}

# WHDLoad support archives (direct WHDLoad boot, src/whdload.rs); fetched
# with checksums pinned in step with tools/fetch-whdload.sh (the sh script
# does not run on Windows runners) and shipped unmodified next to the exe,
# where whdload::find_whdboot_assets probes a sibling whdboot\ directory.
$whdbootSources = @(
    @{ Url = "https://whdload.de/whdload/WHDLoad_usr.lha"
       Sha256 = "093333953737528d79c1eda7d21a16a0aa298698722624e7cfb31f588a0a156d" },
    @{ Url = "https://aminet.net/util/boot/skick346.lha"
       Sha256 = "02b4d01852d12ab391c6469064f917221a0f7319fd0b3ba6c359403ec1d59f96" }
)
$whdbootAssets = "assets\whdboot"
foreach ($src in $whdbootSources) {
    $file = Join-Path $whdbootAssets (Split-Path $src.Url -Leaf)
    if (-not (Test-Path $file) -or
        (Get-FileHash $file -Algorithm SHA256).Hash.ToLower() -ne $src.Sha256) {
        Write-Host "==> Fetching $($src.Url)"
        Invoke-WebRequest -Uri $src.Url -OutFile $file
        $got = (Get-FileHash $file -Algorithm SHA256).Hash.ToLower()
        if ($got -ne $src.Sha256) {
            throw "checksum mismatch for $($src.Url): expected $($src.Sha256), got $got"
        }
    }
}
$whdbootDir = Join-Path $Stage "whdboot"
New-Item -ItemType Directory -Force -Path $whdbootDir | Out-Null
foreach ($f in @("WHDLoad_usr.lha", "skick346.lha", "README.md")) {
    Copy-Item (Join-Path $whdbootAssets $f) (Join-Path $whdbootDir $f)
}

# Top-level docs and an example config to get users started.
Copy-Item "copperline.example.toml" $Stage
Copy-Item "LICENSE" (Join-Path $Stage "LICENSE.txt")
Copy-Item "packaging\windows\README.txt" (Join-Path $Stage "README.txt")
