#!/usr/bin/env pwsh
# Build the Microsoft Store package for Copperline: one .msix per
# architecture, wrapped in a single .msixbundle that the Store hands to x64
# and ARM64 machines from one submission.
#
# Two ways in. Either point it at payloads that are already staged, which is
# what CI does with the directories the release zips were built from:
#
#   msix\build-msix.ps1 -Payload x64=C:\stage\x64, arm64=C:\stage\arm64
#
# or let it build and stage from source for the architectures you name, which
# is the way to get a package out of a working tree on a Windows box:
#
#   msix\build-msix.ps1 -Architecture x64 -SelfSign
#
# Store submissions need no signature: Partner Center strips whatever is
# there and re-signs with a Microsoft certificate. -SelfSign exists so the
# package can be installed and tested locally before it is uploaded, and the
# certificate it makes is trusted by nothing else.
param(
    # Pre-staged payloads as "<arch>=<path>" pairs; arch is x64 or arm64.
    [string[]]$Payload = @(),
    # Architectures to build from source when -Payload does not supply them.
    [ValidateSet("x64", "arm64")][string[]]$Architecture = @(),
    # Four-part package version. Defaults to the Cargo.toml version mapped
    # into the range the Store accepts (see Get-PackageVersion).
    [string]$Version,
    [string]$IdentityName,
    [string]$Publisher,
    [string]$PublisherDisplayName,
    [string]$OutDir,
    [switch]$SelfSign
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$windowsDir = (Resolve-Path (Join-Path $here "..")).Path
$repoRoot = (Resolve-Path (Join-Path $here "..\..\..")).Path
if (-not $OutDir) { $OutDir = $repoRoot }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$ARCH_TARGETS = @{
    "x64"   = "x86_64-pc-windows-msvc"
    "arm64" = "aarch64-pc-windows-msvc"
}

# The Windows SDK tools that build and sign a package. Both live in the same
# versioned bin directory; pick the newest SDK installed, preferring a host
# architecture build of the tool but accepting x64 on an ARM64 machine, where
# it runs under emulation.
function Find-SdkTool {
    param([Parameter(Mandatory = $true)][string]$Name)
    $roots = @(
        ${env:ProgramFiles(x86)},
        $env:ProgramFiles
    ) | Where-Object { $_ } | ForEach-Object { Join-Path $_ "Windows Kits\10\bin" }
    $hostArch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" }
    $candidates = foreach ($root in $roots) {
        if (-not (Test-Path $root)) { continue }
        foreach ($sdk in Get-ChildItem $root -Directory | Sort-Object Name -Descending) {
            foreach ($flavour in @($hostArch, "x64", "x86")) {
                $tool = Join-Path $sdk.FullName "$flavour\$Name"
                if (Test-Path $tool) { $tool }
            }
        }
    }
    $found = $candidates | Select-Object -First 1
    if (-not $found) {
        throw "$Name not found. Install the Windows SDK (it ships with Visual Studio's Desktop development with C++ workload)."
    }
    return $found
}

# Map the crate version onto a package version the Store will accept.
#
# Two Store rules constrain this: the fourth part is reserved and must be 0,
# and the first part cannot be 0. Copperline is still a 0.x project, so the
# major is shifted up by one -- 0.20.0 ships as 1.20.0.0, 0.21.0 as 1.21.0.0,
# and an eventual 1.0.0 as 2.0.0.0. That keeps the sequence strictly
# increasing across the 0.x-to-1.0 boundary, which is what the Store needs to
# recognise an update. Pass -Version to override it.
function Get-PackageVersion {
    $crate = (Select-String -Path (Join-Path $repoRoot "Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
    if ($crate -match '^(\d+)\.(\d+)\.(\d+)') {
        $major = [int]$Matches[1] + 1
        return "$major.$($Matches[2]).$($Matches[3]).0"
    }
    throw "cannot parse the crate version '$crate'"
}

# Identity comes from identity.psd1 unless overridden on the command line.
$identity = Import-PowerShellDataFile (Join-Path $here "identity.psd1")
if (-not $IdentityName) { $IdentityName = $identity.Name }
if (-not $Publisher) { $Publisher = $identity.Publisher }
if (-not $PublisherDisplayName) { $PublisherDisplayName = $identity.PublisherDisplayName }
if ($IdentityName -like "REPLACE-*") {
    throw "Package identity is not set. Copy Name, Publisher and PublisherDisplayName from Partner Center (Product management -> Product identity) into packaging\windows\msix\identity.psd1, or pass -IdentityName/-Publisher/-PublisherDisplayName."
}
if (-not $Version) { $Version = Get-PackageVersion }
if ($Version -notmatch '^\d+\.\d+\.\d+\.0$') {
    throw "package version '$Version' must have four parts and end in 0"
}
if ($Version -match '^0\.') {
    throw "package version '$Version' must not start with 0; the Store rejects it"
}

# Resolve which architectures to package, and where each one's payload is.
$payloads = [ordered]@{}
foreach ($entry in $Payload) {
    $parts = $entry -split "=", 2
    if ($parts.Count -ne 2) { throw "-Payload takes <arch>=<path>, got '$entry'" }
    $arch = $parts[0].Trim()
    if (-not $ARCH_TARGETS.Contains($arch)) { throw "unknown architecture '$arch'" }
    $path = (Resolve-Path $parts[1].Trim()).Path
    if (-not (Test-Path (Join-Path $path "copperline.exe"))) {
        throw "no copperline.exe in the $arch payload at $path"
    }
    $payloads[$arch] = $path
}
foreach ($arch in $Architecture) {
    if ($payloads.Contains($arch)) { continue }
    $stage = Join-Path $OutDir "msix-stage-$arch"
    & (Join-Path $windowsDir "stage-payload.ps1") -Target $ARCH_TARGETS[$arch] -Stage $stage
    $payloads[$arch] = (Resolve-Path $stage).Path
}
if ($payloads.Count -eq 0) {
    throw "nothing to package: pass -Payload <arch>=<path> or -Architecture <arch>"
}

$makeappx = Find-SdkTool -Name "makeappx.exe"
Write-Host "==> Using $makeappx"
Write-Host "==> Package identity $IdentityName $Version ($Publisher)"

# Pack one .msix per architecture. Each gets the manifest with its own
# ProcessorArchitecture and a copy of the logo assets; everything else in the
# package is the payload staged for the release zip, byte for byte.
$packDir = Join-Path $OutDir "msix-packages"
if (Test-Path $packDir) { Remove-Item -Recurse -Force $packDir }
New-Item -ItemType Directory -Force -Path $packDir | Out-Null

$template = Get-Content (Join-Path $here "AppxManifest.xml") -Raw
foreach ($arch in $payloads.Keys) {
    $payloadDir = $payloads[$arch]
    Write-Host "==> Packing $arch from $payloadDir"

    $manifest = $template
    $manifest = $manifest.Replace("@IDENTITY_NAME@", $IdentityName)
    $manifest = $manifest.Replace("@PUBLISHER@", $Publisher)
    $manifest = $manifest.Replace("@PUBLISHER_DISPLAY_NAME@", $PublisherDisplayName)
    $manifest = $manifest.Replace("@VERSION@", $Version)
    $manifest = $manifest.Replace("@ARCHITECTURE@", $arch)
    # UTF-8 without a BOM: makeappx rejects a manifest that starts with one.
    [System.IO.File]::WriteAllText(
        (Join-Path $payloadDir "AppxManifest.xml"),
        $manifest,
        (New-Object System.Text.UTF8Encoding $false))

    $assets = Join-Path $payloadDir "Assets"
    New-Item -ItemType Directory -Force -Path $assets | Out-Null
    Copy-Item (Join-Path $here "assets\*.png") $assets -Force

    $msix = Join-Path $packDir "Copperline-$arch.msix"
    & $makeappx pack /o /d $payloadDir /p $msix
    if ($LASTEXITCODE -ne 0) { throw "makeappx pack failed for $arch" }
}

# One bundle over the per-architecture packages: the Store picks the right
# one per device from a single submission.
$bundle = Join-Path $OutDir "Copperline-$Version.msixbundle"
if (Test-Path $bundle) { Remove-Item -Force $bundle }
& $makeappx bundle /o /d $packDir /p $bundle /bv $Version
if ($LASTEXITCODE -ne 0) { throw "makeappx bundle failed" }

if ($SelfSign) {
    # A throwaway certificate whose subject matches the package publisher,
    # which is what Windows checks before it will install a package. Trust it
    # on the test machine by importing it into Local Machine -> Trusted
    # People; nothing outside that machine will accept it.
    Write-Host "==> Self-signing for local testing"
    $cert = New-SelfSignedCertificate `
        -Type Custom `
        -Subject $Publisher `
        -KeyUsage DigitalSignature `
        -FriendlyName "Copperline MSIX test signing" `
        -CertStoreLocation "Cert:\CurrentUser\My" `
        -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")
    $signtool = Find-SdkTool -Name "signtool.exe"
    & $signtool sign /fd SHA256 /sha1 $cert.Thumbprint $bundle
    if ($LASTEXITCODE -ne 0) { throw "signtool failed" }

    $cerPath = Join-Path $OutDir "Copperline-test-signing.cer"
    Export-Certificate -Cert $cert -FilePath $cerPath | Out-Null
    Write-Host "==> Test certificate exported to $cerPath"
    Write-Host "    Import it into Local Machine\TrustedPeople to install the bundle:"
    Write-Host "    Import-Certificate -FilePath '$cerPath' -CertStoreLocation Cert:\LocalMachine\TrustedPeople"
}

Write-Host "==> Built $bundle for $($payloads.Keys -join ', ')"
