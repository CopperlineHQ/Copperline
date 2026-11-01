#!/usr/bin/env pwsh
# Run on a clean Windows host: for each install mode, installs, checks the
# complete ZIP payload, repairs the install, then uninstalls it. The
# per-user leg takes Setup's default mode (%LOCALAPPDATA%\Programs); the
# all-users leg asks for Program Files with /ALLUSERS and needs an elevated
# prompt. Both reinstall with no mode flag, which must reuse the earlier
# install's mode instead of registering a second installation.
#Requires -Version 7
param(
    [Parameter(Mandatory)]
    [string]$InstallerPath,
    [Parameter(Mandatory)]
    [string]$ZipPath,
    [Parameter(Mandatory)]
    [string]$WorkDir,
    [ValidateSet("CurrentUser", "AllUsers")]
    [string[]]$Mode = @("CurrentUser", "AllUsers")
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$InstallerPath = (Resolve-Path -LiteralPath $InstallerPath).Path
$ZipPath = (Resolve-Path -LiteralPath $ZipPath).Path
$WorkDir = [IO.Path]::GetFullPath($WorkDir)
$uninstallSubkey = "Software\Microsoft\Windows\CurrentVersion\Uninstall\CopperlineHQ.Copperline_is1"

# Where each install mode puts the program, its uninstall entry and its
# shortcuts: the user's own folders and HKCU, or the shared ones and HKLM.
function Get-Layout([string]$InstallMode) {
    if ($InstallMode -eq "AllUsers") {
        $dir = Join-Path $env:ProgramFiles "Copperline"
        $root = "HKLM:"
        $programs = "CommonPrograms"
        $desktop = "CommonDesktopDirectory"
    } else {
        $dir = Join-Path $env:LOCALAPPDATA "Programs\Copperline"
        $root = "HKCU:"
        $programs = "Programs"
        $desktop = "DesktopDirectory"
    }
    [pscustomobject]@{
        InstallDir      = $dir
        UninstallKey    = "$root\$uninstallSubkey"
        StartShortcut   = Join-Path ([Environment]::GetFolderPath($programs, "DoNotVerify")) "Copperline.lnk"
        DesktopShortcut = Join-Path ([Environment]::GetFolderPath($desktop, "DoNotVerify")) "Copperline.lnk"
        Uninstaller     = Join-Path $dir "unins000.exe"
    }
}
$layouts = @{ CurrentUser = Get-Layout "CurrentUser"; AllUsers = Get-Layout "AllUsers" }

# Do not alter a developer's existing installation when run by hand. Both
# modes must be absent whichever legs run: Setup reuses the mode of any
# install it finds, so either one would change what is being tested.
foreach ($layout in $layouts.Values) {
    foreach ($path in @($layout.InstallDir, $layout.UninstallKey, $layout.StartShortcut, $layout.DesktopShortcut)) {
        if (Test-Path -LiteralPath $path) { throw "Smoke test needs a clean host/path: $path already exists" }
    }
}
if (Test-Path -LiteralPath $WorkDir) { throw "Smoke test needs a clean host/path: $WorkDir already exists" }
if ($Mode -contains "AllUsers") {
    $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw "The AllUsers leg installs into Program Files: run elevated, or pass -Mode CurrentUser"
    }
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

function Assert-Payload([string]$InstallDir) {
    foreach ($file in $files) {
        $relative = [IO.Path]::GetRelativePath($payload, $file.FullName)
        $installed = Join-Path $InstallDir $relative
        if ((Get-FileHash -LiteralPath $file.FullName).Hash -ne (Get-FileHash -LiteralPath $installed).Hash) {
            throw "Installed payload differs from ZIP: $relative"
        }
    }
}

# One registration, in this mode's location and registry root.
function Assert-Registration($Layout, $Other) {
    $registration = Get-ItemProperty -LiteralPath $Layout.UninstallKey
    if ($registration.InstallLocation.TrimEnd('\') -ne $Layout.InstallDir) { throw "Unexpected install location" }
    if (Test-Path -LiteralPath $Other.UninstallKey) { throw "Setup also registered $($Other.UninstallKey)" }
}

$silent = @("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-")

function Test-InstallMode([string]$InstallMode) {
    $layout = $layouts[$InstallMode]
    $other = $layouts[$(if ($InstallMode -eq "AllUsers") { "CurrentUser" } else { "AllUsers" })]
    $installDir = $layout.InstallDir
    # Only all-users is asked for: the per-user leg checks Setup's default.
    $modeFlag = @(if ($InstallMode -eq "AllUsers") { "/ALLUSERS" })
    try {
        Write-Host "==> [$InstallMode] Installing into $installDir"
        Invoke-Installer $InstallerPath ($silent + $modeFlag + @("/TASKS=desktopicon", "/LOG=`"$WorkDir\$InstallMode-install.log`""))
        Assert-Payload $installDir
        Assert-Registration $layout $other
        $shell = New-Object -ComObject WScript.Shell
        foreach ($path in @($layout.StartShortcut, $layout.DesktopShortcut)) {
            if (-not (Test-Path -LiteralPath $path)) { throw "Shortcut missing: $path" }
            $shortcut = $shell.CreateShortcut($path)
            if ($shortcut.TargetPath -ne (Join-Path $installDir "copperline.exe") -or
                $shortcut.WorkingDirectory -ne $installDir) {
                throw "Shortcut has the wrong target or working directory: $path"
            }
        }

        # Reinstallation must preserve files made by the user and restore a
        # missing shipped binary, reusing the same directory, mode and
        # uninstall entry with no mode flag given.
        $config = Join-Path $installDir "copperline.toml"
        $save = Join-Path $installDir "user-data\saved-game.txt"
        New-Item -ItemType Directory -Path (Split-Path $save -Parent) | Out-Null
        Set-Content -LiteralPath $config -Value "# Keep my configuration"
        Set-Content -LiteralPath $save -Value "Keep my saved game"
        $configHash = (Get-FileHash -LiteralPath $config).Hash
        $saveHash = (Get-FileHash -LiteralPath $save).Hash
        Remove-Item -LiteralPath (Join-Path $installDir "copperline-import-uae.exe")
        Write-Host "==> [$InstallMode] Reinstalling and checking preserved user files"
        Invoke-Installer $InstallerPath ($silent + "/LOG=`"$WorkDir\$InstallMode-reinstall.log`"")
        Assert-Payload $installDir
        Assert-Registration $layout $other
        if ((Get-FileHash -LiteralPath $config).Hash -ne $configHash -or
            (Get-FileHash -LiteralPath $save).Hash -ne $saveHash) {
            throw "Reinstallation changed user files"
        }
        if (@(Get-ChildItem -LiteralPath $installDir -Filter "unins*.exe").Count -ne 1) {
            throw "Reinstallation created a second uninstaller"
        }

        # Boot from outside both the source tree and installation. --factory
        # prevents the sentinel config or any saved defaults affecting the run.
        Write-Host "==> [$InstallMode] Booting the installed emulator and starting its companions"
        Push-Location $WorkDir
        try {
            & (Join-Path $installDir "copperline.exe") --factory --noaudio --screenshot-after 10 "installed-$InstallMode.png"
            if ($LASTEXITCODE -ne 0) { throw "Installed emulator exited with $LASTEXITCODE" }
            if ((Get-Item -LiteralPath "installed-$InstallMode.png").Length -eq 0) { throw "Screenshot is empty" }
            foreach ($tool in @("copperline-ctl.exe", "copperline-import-uae.exe")) {
                & (Join-Path $installDir $tool) --help | Out-Null
                if ($LASTEXITCODE -ne 0) { throw "$tool --help exited with $LASTEXITCODE" }
            }
        } finally {
            Pop-Location
        }

        Write-Host "==> [$InstallMode] Uninstalling and checking removal and preserved user files"
        Invoke-Installer $layout.Uninstaller ($silent + "/LOG=`"$WorkDir\$InstallMode-uninstall.log`"")
        foreach ($file in $files) {
            $relative = [IO.Path]::GetRelativePath($payload, $file.FullName)
            if (Test-Path -LiteralPath (Join-Path $installDir $relative)) { throw "Uninstall left $relative" }
        }
        foreach ($path in @($layout.UninstallKey, $layout.StartShortcut, $layout.DesktopShortcut, $layout.Uninstaller)) {
            if (Test-Path -LiteralPath $path) { throw "Uninstall left $path" }
        }
        if ((Get-FileHash -LiteralPath $config).Hash -ne $configHash -or
            (Get-FileHash -LiteralPath $save).Hash -ne $saveHash) {
            throw "Uninstall changed user files"
        }
        Write-Host "==> [$InstallMode] Installer lifecycle smoke passed"
    } finally {
        # Also remove a partially tested install on failure, keeping the logs,
        # screenshot and preserved user files available for diagnosis.
        # A wrong-mode install found by Assert-Registration is removed too.
        foreach ($leftover in $layouts.Keys) {
            $uninstaller = $layouts[$leftover].Uninstaller
            if (Test-Path -LiteralPath $uninstaller) {
                Invoke-Installer $uninstaller ($silent + "/LOG=`"$WorkDir\$InstallMode-cleanup-$leftover.log`"")
            }
        }
    }
}

foreach ($installMode in $Mode) { Test-InstallMode $installMode }
