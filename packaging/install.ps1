# Haven installer for native Windows — download a prebuilt haven.exe (no Rust
# toolchain), verify its checksum, install it, and wire it up. No elevation, no
# WSL, no Unix shell.
#
# Usage (PowerShell):
#   irm https://raw.githubusercontent.com/nibbletech-labs/haven/main/packaging/install.ps1 | iex
# or, from a checkout:
#   powershell -ExecutionPolicy Bypass -File packaging\install.ps1
#
# Env:
#   HAVEN_VERSION = v0.1.5   install a specific release tag (default: latest)
#   HAVEN_BIN_DIR = C:\path  install dir (default: %LOCALAPPDATA%\Programs\haven\bin)
#
# The installer places the binary and nothing else. It deliberately does NOT
# compute the data directory: `haven setup` resolves ~/.haven itself via the
# Windows known folder, which ignores env-var overrides that could make the
# two disagree.

$ErrorActionPreference = 'Stop'

$Repo = 'https://github.com/nibbletech-labs/haven'
$Api = 'https://api.github.com/repos/nibbletech-labs/haven'

function Write-Step($msg) { Write-Host "==> $msg" -ForegroundColor Blue }
function Fail($msg) { Write-Host "error: $msg" -ForegroundColor Red; exit 1 }

# --- Platform gate -----------------------------------------------------------
# Only x86_64-pc-windows-msvc is published. PowerShell on Windows-on-ARM runs
# as a NATIVE ARM64 process, so PROCESSOR_ARCHITECTURE reliably reads ARM64
# there — fail with a clear message instead of requesting an asset that does
# not exist.
switch ($env:PROCESSOR_ARCHITECTURE) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc' }
    'ARM64' { Fail 'Windows on ARM (ARM64) has no prebuilt haven binary yet — only x64 is published. (x64 emulation would work, but this shell is native ARM64.)' }
    default { Fail "unsupported architecture '$($env:PROCESSOR_ARCHITECTURE)' — only x64 (AMD64) is published." }
}

if (-not (Get-Command tar -ErrorAction SilentlyContinue)) {
    Fail 'tar.exe not found — it ships with Windows 10 1803+ / Windows 11. On older Windows, extract the release tarball manually.'
}

# --- Resolve the release tag -------------------------------------------------
$tag = $env:HAVEN_VERSION
if ($tag) {
    if ($tag -notmatch '^v') { $tag = "v$tag" }
} else {
    Write-Step 'Resolving latest release'
    $tag = (Invoke-RestMethod -UseBasicParsing "$Api/releases/latest").tag_name
    if (-not $tag) { Fail 'could not resolve the latest release tag.' }
}
$version = $tag -replace '^v', ''
$asset = "haven-$version-$target.tar.gz"
$url = "$Repo/releases/download/$tag/$asset"

# --- Download + verify -------------------------------------------------------
$stage = Join-Path ([System.IO.Path]::GetTempPath()) "haven-install-$([System.IO.Path]::GetRandomFileName())"
New-Item -ItemType Directory -Path $stage | Out-Null
try {
    Write-Step "Downloading $asset ($tag)"
    Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile (Join-Path $stage $asset)
    # The release workflow always attaches a sidecar; a missing one is a broken
    # release, not a reason to install unverified bytes.
    Invoke-WebRequest -UseBasicParsing -Uri "$url.sha256" -OutFile (Join-Path $stage "$asset.sha256")

    # Sidecar is "<hex>  <filename>" (shasum -c form); take the hash.
    $expected = ((Get-Content (Join-Path $stage "$asset.sha256") -Raw).Trim() -split '\s+')[0]
    $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $stage $asset)).Hash
    if ($actual -ne $expected) { Fail "checksum verification failed for $asset (expected $expected, got $actual)" }

    Write-Step "Extracting $asset"
    tar -xzf (Join-Path $stage $asset) -C $stage
    if ($LASTEXITCODE -ne 0) { Fail "failed to extract $asset" }
    $bin = Join-Path $stage 'haven.exe'
    if (-not (Test-Path $bin)) { Fail "no haven.exe inside $asset" }

    # --- Install ---------------------------------------------------------------
    $destDir = $env:HAVEN_BIN_DIR
    if (-not $destDir) { $destDir = Join-Path $env:LOCALAPPDATA 'Programs\haven\bin' }
    New-Item -ItemType Directory -Force -Path $destDir | Out-Null
    $dest = Join-Path $destDir 'haven.exe'
    Write-Step "Installing to $dest"
    # Move-Item can't replace a running haven.exe; keep parity with the CLI's
    # own swap: rename the live one aside, move the new one in.
    if (Test-Path $dest) {
        try {
            Move-Item -Force $bin $dest
        } catch {
            $aside = "$dest.$PID.old"
            Move-Item -Force $dest $aside
            try {
                Move-Item -Force $bin $dest
            } catch {
                # Put the original back so the machine is never left with no
                # haven.exe at all (mirrors the CLI's own swap fallback).
                Move-Item -Force $aside $dest
                throw
            }
            Remove-Item -Force $aside -ErrorAction SilentlyContinue
        }
    } else {
        Move-Item -Force $bin $dest
    }

    # --- PATH (user scope; no setx — it truncates at 1024 chars) ---------------
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not ($userPath -split ';' | Where-Object { $_ -eq $destDir })) {
        Write-Step "Adding $destDir to your user PATH"
        $newPath = if ($userPath) { "$userPath;$destDir" } else { $destDir }
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        Write-Host 'note: already-open terminals keep their old PATH; open a new one to use `haven` there.'
    }
    if (-not (($env:PATH -split ';') -contains $destDir)) { $env:PATH = "$destDir;$env:PATH" }

    # --- Wire up (idempotent; never fatal) --------------------------------------
    Write-Step 'Running haven setup'
    & $dest setup
    if ($LASTEXITCODE -ne 0) { Write-Host 'setup skipped (run `haven setup` manually).' }

    Write-Step 'Done. Try: haven item add "First item"; haven doctor'
} finally {
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
}
