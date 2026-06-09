#!/usr/bin/env sh
# Haven installer — build from source, install the `haven` binary, wire it up.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/nibbletech-labs/haven/main/packaging/install.sh | sh
# or, from a checkout:
#   ./packaging/install.sh
#
# Requires a Rust toolchain (https://rustup.rs). Installs to the first writable
# of $HAVEN_BIN_DIR, /usr/local/bin, ~/.local/bin, then runs `haven setup`.
set -eu

REPO="https://github.com/nibbletech-labs/haven"
log() { printf '\033[1;34m==>\033[0m %s\n' "$1"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

command -v cargo >/dev/null 2>&1 || die "cargo not found — install Rust from https://rustup.rs and re-run."

# Resolve a source tree: a local checkout if we're in one, else a fresh clone.
if [ -f "Cargo.toml" ] && grep -q '"crates/haven-cli"' Cargo.toml 2>/dev/null; then
    SRC="$(pwd)"
    CLONED=""
else
    command -v git >/dev/null 2>&1 || die "git not found — needed to fetch the source."
    SRC="$(mktemp -d)"
    CLONED="$SRC"
    log "Cloning $REPO"
    git clone --depth 1 "$REPO" "$SRC" >/dev/null 2>&1 || die "clone failed."
fi

log "Building haven (release)"
( cd "$SRC" && cargo build --release --bin haven ) || die "build failed."

# Pick an install dir: explicit override, else the first writable standard dir.
BIN="$SRC/target/release/haven"
for d in "${HAVEN_BIN_DIR:-}" /usr/local/bin "$HOME/.local/bin"; do
    [ -n "$d" ] || continue
    if mkdir -p "$d" 2>/dev/null && [ -w "$d" ]; then DEST="$d"; break; fi
done
[ -n "${DEST:-}" ] || die "no writable install dir — set \$HAVEN_BIN_DIR and re-run."

log "Installing to $DEST/haven"
install -m 0755 "$BIN" "$DEST/haven"
[ -n "$CLONED" ] && rm -rf "$CLONED"

# Wire MCP + skill (idempotent; never fatal).
log "Running haven setup"
"$DEST/haven" setup >/dev/null 2>&1 || log "setup skipped (run \`haven setup\` manually)."

case ":$PATH:" in
    *":$DEST:"*) ;;
    *) log "Note: $DEST is not on your \$PATH — add it to use \`haven\` directly." ;;
esac
log "Done. Try: haven doctor"
