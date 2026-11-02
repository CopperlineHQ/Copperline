#!/usr/bin/env pwsh
# Check the Authenticode signatures SignPath put on a Windows package: every
# .exe inside a portable ZIP, or a single installer .exe. The Windows
# workflow runs it on what each signing request returns, before the package
# replaces the unsigned one.
#
# release-signing must produce a timestamped signature Windows trusts (the
# SignPath Foundation certificate). test-signing signs with SignPath's test
# certificate, which no Windows root trusts, so for that policy any
# signature whose hash matches the file passes.
param(
    [Parameter(Mandatory)][string]$Path,
    [Parameter(Mandatory)][ValidateSet("test-signing", "release-signing")][string]$Policy
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$Path = (Resolve-Path -LiteralPath $Path).Path
$accepted = if ($Policy -eq "release-signing") { @("Valid") } else { @("Valid", "UnknownError", "NotTrusted") }
$scratch = $null
try {
    if ($Path -like "*.zip") {
        $scratch = Join-Path ([IO.Path]::GetTempPath()) "copperline-signatures-$([guid]::NewGuid())"
        Expand-Archive -LiteralPath $Path -DestinationPath $scratch
        $files = @(Get-ChildItem -LiteralPath $scratch -Recurse -File -Filter *.exe)
    } else {
        $files = @(Get-Item -LiteralPath $Path)
    }
    if ($files.Count -eq 0) { throw "no executables in $Path" }
    foreach ($file in $files) {
        $signature = Get-AuthenticodeSignature -LiteralPath $file.FullName
        if (-not $signature.SignerCertificate -or $signature.Status -notin $accepted) {
            throw "$($file.Name): signature status $($signature.Status): $($signature.StatusMessage)"
        }
        if ($Policy -eq "release-signing" -and -not $signature.TimeStamperCertificate) {
            throw "$($file.Name): the signature has no timestamp, so it would lapse with the certificate"
        }
        Write-Host "$($file.Name): $($signature.Status), signed by $($signature.SignerCertificate.Subject)"
    }
} finally {
    if ($scratch -and (Test-Path -LiteralPath $scratch)) {
        Remove-Item -LiteralPath $scratch -Recurse -Force
    }
}
