# End-to-end check of a PUBLISHED Windows release of haven: run the installer,
# then assert the install is actually healthy. Usable anywhere Windows runs —
# the install-check workflow, an RDP session on a runner, a throwaway VM, or a
# real machine. Requires a release that carries the Windows asset (v0.1.6+).
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File packaging\verify-install.ps1
#   ... -Version v0.1.6            pin a release (default: latest)
#   ... -Installer .\packaging\install.ps1   use a local installer (default: fetch from main)

param(
    [string]$Version = '',
    [string]$Installer = ''
)

$ErrorActionPreference = 'Stop'

if ($Version) { $env:HAVEN_VERSION = $Version }
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

Write-Host 'verify-install: PASS'
