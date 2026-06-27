#!/usr/bin/env sh
# unlocr installer for macOS / Linux. Builds from source (cargo) and installs the
# binary, then checks runtime deps (pdftoppm, llama-server). POSIX sh.
#
#   ./install.sh                 # build + install to /usr/local/bin
#   PREFIX=$HOME/.local ./install.sh
set -eu

NAME=unlocr
PREFIX=${PREFIX:-/usr/local}
BINDIR="$PREFIX/bin"
ROOT=$(cd "$(dirname "$0")" && pwd)
SRC="$ROOT/unlocr"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

command -v cargo >/dev/null 2>&1 || die "cargo not found. Install Rust: https://rustup.rs"

say "Building $NAME (release)..."
cargo build --release --manifest-path "$SRC/Cargo.toml"
BIN="$SRC/target/release/$NAME"
[ -x "$BIN" ] || die "build produced no binary at $BIN"

# Need sudo if the target dir is not writable.
SUDO=""
if [ ! -w "$(dirname "$BINDIR")" ] && [ "$(id -u)" -ne 0 ]; then
  command -v sudo >/dev/null 2>&1 && SUDO=sudo
fi

say "Installing to $BINDIR (may prompt for sudo)..."
$SUDO install -d "$BINDIR"
$SUDO install -m 0755 "$BIN" "$BINDIR/$NAME"
say "Installed $BINDIR/$NAME"

# Runtime dep checks (warn only; both are external to this package).
check() {
  if command -v "$1" >/dev/null 2>&1; then
    say "  ok: $1 ($(command -v "$1"))"
  else
    say "  MISSING: $1 -- $2"
  fi
}
say "Runtime dependencies:"
case "$(uname -s)" in
  Darwin) check pdftoppm "brew install poppler"
          check llama-server "brew install llama.cpp" ;;
  Linux)  check pdftoppm "apt install poppler-utils  /  dnf install poppler-utils"
          check llama-server "build llama.cpp >= b8530: https://github.com/ggml-org/llama.cpp" ;;
  *)      check pdftoppm "install poppler"
          check llama-server "install llama.cpp >= b8530" ;;
esac

case ":$PATH:" in
  *":$BINDIR:"*) ;;
  *) say "note: $BINDIR is not on PATH; add it to your shell profile." ;;
esac
say "Done. Run: $NAME --help"
