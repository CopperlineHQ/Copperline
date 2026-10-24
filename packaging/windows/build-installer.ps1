#!/usr/bin/env pwsh
# Build the same portable payload as build-zip.ps1, then wrap it in an Inno
# Setup installer. Supply -ZipPath to package an already-built ZIP without
# compiling Rust again. Requires Inno Setup 6.3 or newer on Windows.
param(
    [ValidateSet("x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc")]
    [string]$Target = "x86_64-pc-windows-msvc",
    [string]$ZipPath
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$here = $PSScriptRoot
$repoRoot = (Resolve-Path (Join-Path $here "..\..")).Path
$arch = if ($Target -eq "aarch64-pc-windows-msvc") { "arm64" } else { "x64" }
$version = (Select-String -Path (Join-Path $repoRoot "Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
# Windows file versions are numeric, while AppVersion and the filename keep
# the complete Cargo version, including any prerelease suffix.
$numericVersion = ($version -split '[-+]')[0]
$stageName = "Copperline-$version-win-$arch"

$compiler = Get-Command ISCC.exe -ErrorAction SilentlyContinue
if ($compiler) {
    $iscc = $compiler.Source
} else {
    $candidates = foreach ($programFiles in @(${env:ProgramFiles(x86)}, $env:ProgramFiles)) {
        if ($programFiles) {
            foreach ($directory in @("Inno Setup 7", "Inno Setup 6")) {
                Join-Path $programFiles "$directory\ISCC.exe"
            }
        }
    }
    $iscc = $candidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    if (-not $iscc) {
        throw "Install Inno Setup 6.3 or newer, or add ISCC.exe to PATH."
    }
}

if ($ZipPath) {
    $ZipPath = (Resolve-Path -LiteralPath $ZipPath).Path
} else {
    & (Join-Path $here "build-zip.ps1") -Target $Target
    $ZipPath = Join-Path $repoRoot "$stageName.zip"
}

$scratch = Join-Path $repoRoot "target\windows-installer\$([guid]::NewGuid())"
try {
    Expand-Archive -LiteralPath $ZipPath -DestinationPath $scratch
    $payload = Join-Path $scratch $stageName
    # Refuse a ZIP for another version/architecture or an incomplete payload.
    foreach ($file in @("copperline.exe", "copperline-ctl.exe", "copperline-import-uae.exe", "LICENSE.txt")) {
        if (-not (Test-Path -LiteralPath (Join-Path $payload $file) -PathType Leaf)) {
            throw "$file missing from $stageName in $ZipPath"
        }
    }
    & $iscc "/DAppVersion=$version" "/DAppVersionNumeric=$numericVersion" "/DAppArch=$arch" `
        "/DPayloadDir=$payload" "/O$repoRoot" (Join-Path $here "copperline.iss")
    if ($LASTEXITCODE -ne 0) { throw "Inno Setup compiler exited with $LASTEXITCODE" }
    $installer = Join-Path $repoRoot "$stageName-setup.exe"
    if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) {
        throw "Inno Setup did not produce $installer"
    }
    Write-Host "==> Built $installer"
} finally {
    if (Test-Path -LiteralPath $scratch) { Remove-Item -LiteralPath $scratch -Recurse -Force }
}
