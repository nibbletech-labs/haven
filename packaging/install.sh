#!/usr/bin/env sh
# Haven installer — download a prebuilt `haven` binary (no Rust toolchain),
# install it, and wire it up. Falls back to building from source when no prebuilt
# asset matches the platform (or when asked).
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/nibbletech-labs/haven/main/packaging/install.sh | sh
# or, from a checkout:
#   ./packaging/install.sh
#
# Env:
#   HAVEN_VERSION=v0.1.5        install a specific release tag (default: latest)
#   HAVEN_BIN_DIR=/path         install dir (else /usr/local/bin, then ~/.local/bin)
#   HAVEN_BUILD_FROM_SOURCE=1   skip the prebuilt path and compile (needs cargo)
# Flags:
#   --from-source               same as HAVEN_BUILD_FROM_SOURCE=1
set -eu

REPO="https://github.com/nibbletech-labs/haven"
API="https://api.github.com/repos/nibbletech-labs/haven"
log() { printf '\033[1;34m==>\033[0m %s\n' "$1"; }
warn() { printf '\033[1;33mwarn:\033[0m %s\n' "$1" >&2; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

FROM_SOURCE="${HAVEN_BUILD_FROM_SOURCE:-}"
for arg in "$@"; do
    case "$arg" in
        --from-source) FROM_SOURCE=1 ;;
        *) die "unknown argument: $arg" ;;
    esac
done

# Download a URL to a file, or to stdout — curl preferred, wget as fallback.
download() { # url dest
    if have curl; then curl -fsSL "$1" -o "$2"
    elif have wget; then wget -qO "$2" "$1"
    else return 1; fi
}
download_stdout() { # url
    if have curl; then curl -fsSL "$1"
    elif have wget; then wget -qO - "$1"
    else return 1; fi
}

# True when a sha256 tool is available to verify a download.
have_sha_tool() { have sha256sum || have shasum; }

# Verify a "<hex>  <file>" sidecar against its file in the current directory.
# Fails closed: with no checksum tool this returns non-zero (the prebuilt path
# is gated on have_sha_tool, so a verified install is never silently skipped).
sha_check() { # sidecar
    if have sha256sum; then sha256sum -c "$1" >/dev/null 2>&1
    elif have shasum; then shasum -a 256 -c "$1" >/dev/null 2>&1
    else return 1; fi
}

# Map this host to a published target triple (mirrors the release build matrix).
detect_target() {
    case "$(uname -s)" in
        Darwin) os_part="apple-darwin" ;;
        Linux) os_part="unknown-linux-musl" ;;
        *) return 1 ;;
    esac
    case "$(uname -m)" in
        arm64 | aarch64) arch_part="aarch64" ;;
        x86_64 | amd64) arch_part="x86_64" ;;
        *) return 1 ;;
    esac
    printf '%s-%s' "$arch_part" "$os_part"
}

# The release tag to install: $HAVEN_VERSION (with or without leading v), else
# the latest published release from the GitHub API.
resolve_tag() {
    if [ -n "${HAVEN_VERSION:-}" ]; then
        case "$HAVEN_VERSION" in
            v*) printf '%s' "$HAVEN_VERSION" ;;
            *) printf 'v%s' "$HAVEN_VERSION" ;;
        esac
        return 0
    fi
    download_stdout "$API/releases/latest" 2>/dev/null |
        grep -m1 '"tag_name"' |
        sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/'
}

# Try the prebuilt path. Sets $BIN and $CLEANUP on success; returns non-zero
# (→ source fallback) when the platform/release has no matching asset. A download
# that succeeds but fails verification or extraction is fatal, not a fallback.
install_prebuilt() {
    target="$(detect_target)" || return 1
    # Fail closed: without a checksum tool we can't verify a download, so fall
    # back to the (verified-by-construction) source build rather than install
    # unverified bytes.
    have_sha_tool || { warn "no sha256 tool (sha256sum/shasum) found — can't verify a prebuilt download; using source"; return 1; }
    tag="$(resolve_tag)"
    [ -n "$tag" ] || return 1
    version="${tag#v}"
    asset="haven-${version}-${target}.tar.gz"
    url="$REPO/releases/download/${tag}/${asset}"

    tmp="$(mktemp -d)"
    log "Downloading $asset ($tag)"
    # A missing asset means "no prebuilt for this platform/release" → fall back to
    # source. But once the asset exists, its sidecar must too (the release
    # workflow attaches both): a missing sidecar is a broken release, not a
    # reason to silently install unverified — so that case is fatal.
    if ! download "$url" "$tmp/$asset"; then
        rm -rf "$tmp"
        return 1
    fi
    if ! download "$url.sha256" "$tmp/$asset.sha256"; then
        rm -rf "$tmp"
        die "release asset $asset has no .sha256 sidecar — refusing to install unverified."
    fi
    ( cd "$tmp" && sha_check "$asset.sha256" ) || { rm -rf "$tmp"; die "checksum verification failed for $asset"; }
    ( cd "$tmp" && tar -xzf "$asset" ) || { rm -rf "$tmp"; die "failed to extract $asset"; }
    [ -f "$tmp/haven" ] || { rm -rf "$tmp"; die "no haven binary inside $asset"; }
    BIN="$tmp/haven"
    CLEANUP="$tmp"
}

# The from-source path (needs a Rust toolchain). Sets $BIN and $CLEANUP.
build_from_source() {
    have cargo || die "cargo not found — install Rust from https://rustup.rs and re-run (or use a platform with a prebuilt binary)."
    if [ -f "Cargo.toml" ] && grep -q '"crates/haven-cli"' Cargo.toml 2>/dev/null; then
        SRC="$(pwd)"
        CLONED=""
    else
        have git || die "git not found — needed to fetch the source."
        SRC="$(mktemp -d)"
        CLONED="$SRC"
        log "Cloning $REPO"
        git clone --depth 1 "$REPO" "$SRC" >/dev/null 2>&1 || die "clone failed."
    fi
    log "Building haven (release)"
    ( cd "$SRC" && cargo build --release --bin haven ) || die "build failed."
    BIN="$SRC/target/release/haven"
    CLEANUP="$CLONED"
}

BIN=""
CLEANUP=""
if [ -n "$FROM_SOURCE" ]; then
    log "Building from source (requested)"
    build_from_source
elif install_prebuilt; then
    :
else
    warn "no prebuilt binary for this platform (or release unavailable) — building from source"
    build_from_source
fi

# Pick an install dir: explicit override, else the first writable standard dir.
for d in "${HAVEN_BIN_DIR:-}" /usr/local/bin "$HOME/.local/bin"; do
    [ -n "$d" ] || continue
    if mkdir -p "$d" 2>/dev/null && [ -w "$d" ]; then DEST="$d"; break; fi
done
[ -n "${DEST:-}" ] || die "no writable install dir — set \$HAVEN_BIN_DIR and re-run."

log "Installing to $DEST/haven"
install -m 0755 "$BIN" "$DEST/haven"
[ -n "$CLEANUP" ] && rm -rf "$CLEANUP"

# Wire MCP + skill (idempotent; never fatal).
log "Running haven setup"
"$DEST/haven" setup || log "setup skipped (run \`haven setup\` manually)."

case ":$PATH:" in
    *":$DEST:"*) ;;
    *) log "Note: $DEST is not on your \$PATH — add it to use \`haven\` directly." ;;
esac
log "Done. Try: haven item add \"First item\" && haven doctor"
