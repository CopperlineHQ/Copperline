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

# Inno Setup writes Setup's version strings over fixed-width placeholders and
# leaves the rest of each placeholder as spaces (UpdateStringValue in its
# Compiler.ExeUpdateFunc.pas), so Setup's ProductName is "Copperline" followed
# by 50 spaces. Code signing signs only a Setup whose product details match
# exactly (signpath/windows-installer.xml), so turn that padding into NULs and
# shorten each string's recorded length to match. The resource keeps its size
# and place, as when Inno itself updates it, and Setup checksums only the data
# appended after its image, never the image.
function Remove-VersionStringPadding([string]$Path) {
    $bytes = [IO.File]::ReadAllBytes($Path)
    # Search the PE image only, not the setup data Inno appends after it.
    $pe = [BitConverter]::ToInt32($bytes, 0x3C)
    $sectionCount = [BitConverter]::ToUInt16($bytes, $pe + 6)
    $sectionTable = $pe + 24 + [BitConverter]::ToUInt16($bytes, $pe + 20)
    [long]$imageEnd = 0
    for ($i = 0; $i -lt $sectionCount; $i++) {
        $section = $sectionTable + 40 * $i
        $end = [long][BitConverter]::ToUInt32($bytes, $section + 20) + [BitConverter]::ToUInt32($bytes, $section + 16)
        if ($end -gt $imageEnd) { $imageEnd = $end }
    }
    $latin1 = [Text.Encoding]::GetEncoding(28591)
    $image = $latin1.GetString($bytes, 0, [int]$imageEnd)
    $key = $latin1.GetString([Text.Encoding]::Unicode.GetBytes("VS_VERSION_INFO`0"))
    $rootOffset = $image.IndexOf($key, [StringComparison]::Ordinal) - 6
    if ($rootOffset -lt 0) { throw "no version resource in $Path" }

    # Every node is wLength, wValueLength, wType and a NUL-terminated UTF-16
    # key, then its value and its children, each aligned to 32 bits.
    function Align([int]$Offset) { $Offset + ((4 - ($Offset - $rootOffset) % 4) % 4) }
    function Read-Node([int]$Offset) {
        $keyEnd = $Offset + 6
        while ([BitConverter]::ToUInt16($bytes, $keyEnd) -ne 0) { $keyEnd += 2 }
        $value = Align ($keyEnd + 2)
        [pscustomobject]@{
            Offset      = $Offset
            Length      = [int][BitConverter]::ToUInt16($bytes, $Offset)
            ValueLength = [int][BitConverter]::ToUInt16($bytes, $Offset + 2)
            IsText      = [BitConverter]::ToUInt16($bytes, $Offset + 4) -eq 1
            Key         = [Text.Encoding]::Unicode.GetString($bytes, $Offset + 6, $keyEnd - $Offset - 6)
            Value       = $value
        }
    }
    function Get-Children($Node) {
        $valueBytes = if ($Node.IsText) { 2 * $Node.ValueLength } else { $Node.ValueLength }
        $child = Align ($Node.Value + $valueBytes)
        while ($child -lt $Node.Offset + $Node.Length) {
            Read-Node $child
            $child = Align ($child + [BitConverter]::ToUInt16($bytes, $child))
        }
    }

    $versionInfo = Read-Node $rootOffset
    foreach ($fileInfo in @(Get-Children $versionInfo | Where-Object Key -eq "StringFileInfo")) {
        foreach ($table in @(Get-Children $fileInfo)) {
            foreach ($string in @(Get-Children $table)) {
                $text = [Text.Encoding]::Unicode.GetString($bytes, $string.Value, 2 * $string.ValueLength).Split([char]0)[0]
                $trimmed = $text.TrimEnd(' ')
                if ($trimmed.Length -lt $text.Length) {
                    [Array]::Clear($bytes, $string.Value + 2 * $trimmed.Length, 2 * ($text.Length - $trimmed.Length))
                    [BitConverter]::GetBytes([uint16]($trimmed.Length + 1)).CopyTo($bytes, $string.Offset + 2)
                }
            }
        }
    }
    $stream = [IO.File]::OpenWrite($Path)
    try {
        $stream.Position = $rootOffset
        $stream.Write($bytes, $rootOffset, $versionInfo.Length)
    } finally {
        $stream.Dispose()
    }
}

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
    Remove-VersionStringPadding $installer
    Write-Host "==> Built $installer"
} finally {
    if (Test-Path -LiteralPath $scratch) { Remove-Item -LiteralPath $scratch -Recurse -Force }
}
