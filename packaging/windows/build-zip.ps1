#!/usr/bin/env pwsh
# Build a Copperline Windows release zip: a portable, no-install bundle that
# runs without administrator rights. Run on a Windows host (or CI); see
# .github/workflows/windows.yml.
#
# What it does:
#   1. Builds the release binary for the requested MSVC target (x86-64 by
#      default, ARM64 with -Target aarch64-pc-windows-msvc) with the pinned
#      dependency graph, and stages the shipping payload (see
#      stage-payload.ps1, which both this and the MSIX package share). The
#      CRT is statically linked (see .cargo/config.toml), so the bundle needs
#      no Visual C++ Redistributable.
#   2. Zips the staged folder into Copperline-<version>-win-<x64|arm64>.zip,
#      mirroring the AppImage/Homebrew version naming so release assets are
#      self-describing.
param(
    [string]$Target = "x86_64-pc-windows-msvc"
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $here "..\..")).Path
Set-Location $repoRoot

$target = $Target
$arch = switch ($target) {
    "x86_64-pc-windows-msvc" { "x64" }
    "aarch64-pc-windows-msvc" { "arm64" }
    default { throw "unsupported target $target" }
}

# Version from Cargo.toml, matching the AppImage/Homebrew naming convention.
$version = (Select-String -Path "Cargo.toml" -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
$stageName = "Copperline-$version-win-$arch"
$stage = Join-Path $repoRoot $stageName
$zipPath = Join-Path $repoRoot "$stageName.zip"

& (Join-Path $here "stage-payload.ps1") -Target $target -Stage $stage

Write-Host "==> Zipping $zipPath"
if (Test-Path $zipPath) { Remove-Item -Force $zipPath }
Compress-Archive -Path $stage -DestinationPath $zipPath -CompressionLevel Optimal

Write-Host "==> Built $stageName.zip"
