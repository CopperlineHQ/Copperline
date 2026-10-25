#!/usr/bin/env pwsh
# Run on a clean Windows host: installs into the default per-user location,
# checks the complete ZIP payload, repairs the install, then uninstalls it.
#Requires -Version 7
param(
    [Parameter(Mandatory)]
    [string]$InstallerPath,
    [Parameter(Mandatory)]
    [string]$ZipPath,
    [Parameter(Mandatory)]
    [string]$WorkDir
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$InstallerPath = (Resolve-Path -LiteralPath $InstallerPath).Path
$ZipPath = (Resolve-Path -LiteralPath $ZipPath).Path
$WorkDir = [IO.Path]::GetFullPath($WorkDir)
$installDir = Join-Path $env:LOCALAPPDATA "Programs\Copperline"
$uninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\CopperlineHQ.Copperline_is1"
$startShortcut = Join-Path ([Environment]::GetFolderPath("Programs", "DoNotVerify")) "Copperline.lnk"
$desktopShortcut = Join-Path ([Environment]::GetFolderPath("DesktopDirectory", "DoNotVerify")) "Copperline.lnk"
$uninstaller = Join-Path $installDir "unins000.exe"

# Do not alter a developer's existing installation when run by hand.
foreach ($path in @($installDir, $uninstallKey, $startShortcut, $desktopShortcut, $WorkDir)) {
    if (Test-Path -LiteralPath $path) { throw "Smoke test needs a clean host/path: $path already exists" }
}
New-Item -ItemType Directory -Path $WorkDir | Out-Null
Expand-Archive -LiteralPath $ZipPath -DestinationPath (Join-Path $WorkDir "payload")
$payload = Join-Path $WorkDir "payload\$([IO.Path]::GetFileNameWithoutExtension($ZipPath))"
$files = @(Get-ChildItem -LiteralPath $payload -Recurse -File)
if ($files.Count -eq 0) { throw "ZIP payload is empty" }

function Invoke-Installer([string]$Path, [string[]]$Arguments) {
    $process = Start-Process -FilePath $Path -ArgumentList $Arguments -PassThru -Wait
    if ($process.ExitCode -ne 0) { throw "$Path exited with $($process.ExitCode)" }
}

function Assert-Payload {
    foreach ($file in $files) {
        $relative = [IO.Path]::GetRelativePath($payload, $file.FullName)
        $installed = Join-Path $installDir $relative
        if ((Get-FileHash -LiteralPath $file.FullName).Hash -ne (Get-FileHash -LiteralPath $installed).Hash) {
            throw "Installed payload differs from ZIP: $relative"
        }
    }
}

$silent = @("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-")
try {
    Write-Host "==> Installing into the default per-user directory"
    Invoke-Installer $InstallerPath ($silent + @("/TASKS=desktopicon", "/LOG=`"$WorkDir\install.log`""))
    Assert-Payload
    $registration = Get-ItemProperty -LiteralPath $uninstallKey
    if ($registration.InstallLocation.TrimEnd('\') -ne $installDir) { throw "Unexpected install location" }
    $shell = New-Object -ComObject WScript.Shell
    foreach ($path in @($startShortcut, $desktopShortcut)) {
        if (-not (Test-Path -LiteralPath $path)) { throw "Shortcut missing: $path" }
        $shortcut = $shell.CreateShortcut($path)
        if ($shortcut.TargetPath -ne (Join-Path $installDir "copperline.exe") -or
            $shortcut.WorkingDirectory -ne $installDir) {
            throw "Shortcut has the wrong target or working directory: $path"
        }
    }

    # Reinstallation must preserve files made by the user and restore a
    # missing shipped binary, reusing the same directory and uninstall entry.
    $config = Join-Path $installDir "copperline.toml"
    $save = Join-Path $installDir "user-data\saved-game.txt"
    New-Item -ItemType Directory -Path (Split-Path $save -Parent) | Out-Null
    Set-Content -LiteralPath $config -Value "# Keep my configuration"
    Set-Content -LiteralPath $save -Value "Keep my saved game"
    $configHash = (Get-FileHash -LiteralPath $config).Hash
    $saveHash = (Get-FileHash -LiteralPath $save).Hash
    Remove-Item -LiteralPath (Join-Path $installDir "copperline-import-uae.exe")
    Write-Host "==> Reinstalling and checking preserved user files"
    Invoke-Installer $InstallerPath ($silent + "/LOG=`"$WorkDir\reinstall.log`"")
    Assert-Payload
    if ((Get-FileHash -LiteralPath $config).Hash -ne $configHash -or
        (Get-FileHash -LiteralPath $save).Hash -ne $saveHash) {
        throw "Reinstallation changed user files"
    }
    if (@(Get-ChildItem -LiteralPath $installDir -Filter "unins*.exe").Count -ne 1) {
        throw "Reinstallation created a second uninstaller"
    }

    # Boot from outside both the source tree and installation. --factory
    # prevents the sentinel config or any saved defaults affecting the run.
    Write-Host "==> Booting the installed emulator and starting its companions"
    Push-Location $WorkDir
    try {
        & (Join-Path $installDir "copperline.exe") --factory --noaudio --screenshot-after 10 installed.png
        if ($LASTEXITCODE -ne 0) { throw "Installed emulator exited with $LASTEXITCODE" }
        if ((Get-Item -LiteralPath "installed.png").Length -eq 0) { throw "Screenshot is empty" }
        foreach ($tool in @("copperline-ctl.exe", "copperline-import-uae.exe")) {
            & (Join-Path $installDir $tool) --help | Out-Null
            if ($LASTEXITCODE -ne 0) { throw "$tool --help exited with $LASTEXITCODE" }
        }
    } finally {
        Pop-Location
    }

    Write-Host "==> Uninstalling and checking removal and preserved user files"
    Invoke-Installer $uninstaller ($silent + "/LOG=`"$WorkDir\uninstall.log`"")
    foreach ($file in $files) {
        $relative = [IO.Path]::GetRelativePath($payload, $file.FullName)
        if (Test-Path -LiteralPath (Join-Path $installDir $relative)) { throw "Uninstall left $relative" }
    }
    foreach ($path in @($uninstallKey, $startShortcut, $desktopShortcut, $uninstaller)) {
        if (Test-Path -LiteralPath $path) { throw "Uninstall left $path" }
    }
    if ((Get-FileHash -LiteralPath $config).Hash -ne $configHash -or
        (Get-FileHash -LiteralPath $save).Hash -ne $saveHash) {
        throw "Uninstall changed user files"
    }
    Write-Host "==> Installer lifecycle smoke passed"
} finally {
    # Also remove a partially tested install on failure, keeping the logs,
    # screenshot and preserved user files available for diagnosis.
    if (Test-Path -LiteralPath $uninstaller) {
        Invoke-Installer $uninstaller ($silent + "/LOG=`"$WorkDir\cleanup.log`"")
    }
}
