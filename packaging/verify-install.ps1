# End-to-end check of a PUBLISHED Windows release of haven: run the installer,
# then assert the install is actually healthy. Usable anywhere Windows runs —
# the install-check workflow, an RDP session on a runner, a throwaway VM, or a
# real machine. Requires a release that carries the Windows asset (v0.1.6+).
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File packaging\verify-install.ps1
#   ... -Version v0.1.6            pin a release (default: latest)
#   ... -Installer .\packaging\install.ps1   use a local installer (default: fetch from main)
#   ... -UpdateFrom v0.1.6         install that OLDER release first, then `self update`
#                                  to latest and assert the swap really happened —
#                                  the full download-then-replace composition.

param(
    [string]$Version = '',
    [string]$Installer = '',
    [string]$UpdateFrom = ''
)

$ErrorActionPreference = 'Stop'

if ($UpdateFrom) { $env:HAVEN_VERSION = $UpdateFrom }
elseif ($Version) { $env:HAVEN_VERSION = $Version }
if ($Installer) {
    & $Installer
} else {
    Invoke-RestMethod -UseBasicParsing 'https://raw.githubusercontent.com/nibbletech-labs/haven/main/packaging/install.ps1' | Invoke-Expression
}

$binDir = if ($env:HAVEN_BIN_DIR) { $env:HAVEN_BIN_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\haven\bin' }
$bin = Join-Path $binDir 'haven.exe'
if (-not (Test-Path $bin)) { throw "haven.exe not found at $bin" }

& $bin --version
if ($LASTEXITCODE -ne 0) { throw 'haven --version failed' }

$doctor = & $bin doctor | ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or $doctor.ok -ne $true) {
    throw "haven doctor not ok: $($doctor | ConvertTo-Json -Depth 5)"
}

# The installed location must be recognised, so auto-update runs rather than
# falling through to the Unknown install-method arm.
& $bin self update --check
if ($LASTEXITCODE -ne 0) { throw 'haven self update --check failed' }

if ($UpdateFrom) {
    # The composition test: download the newer release and swap it in place —
    # only meaningful once a release newer than $UpdateFrom carries the
    # Windows asset.
    $before = (& $bin --version | Out-String).Trim()
    & $bin self update
    if ($LASTEXITCODE -ne 0) { throw 'haven self update failed' }
    $after = (& $bin --version | Out-String).Trim()
    Write-Host "self update: '$before' -> '$after'"
    if ($after -eq $before) { throw "self update did not change the binary (still $before)" }
}

Write-Host 'verify-install: PASS'
