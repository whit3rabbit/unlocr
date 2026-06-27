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
SRC="$ROOT"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

command -v cargo >/dev/null 2>&1 || die "cargo not found. Install Rust: https://rustup.rs"

say "Building $NAME (release)..."
cargo build --release --locked --manifest-path "$SRC/Cargo.toml"
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

llama_hint() {
  has_brew=0; has_port=0; has_conda=0; has_nix=0
  command -v brew >/dev/null 2>&1 && has_brew=1
  command -v port >/dev/null 2>&1 && has_port=1
  (command -v conda >/dev/null 2>&1 || command -v mamba >/dev/null 2>&1 || command -v pixi >/dev/null 2>&1) && has_conda=1
  command -v nix >/dev/null 2>&1 && has_nix=1

  case "$(uname -s)" in
    Darwin)
      options=""
      [ $has_brew -eq 1 ] && options="$options\n    - Homebrew: brew install llama.cpp"
      [ $has_port -eq 1 ] && options="$options\n    - MacPorts: sudo port install llama.cpp"
      [ $has_conda -eq 1 ] && options="$options\n    - Conda-forge: conda install -c conda-forge llama-cpp"
      [ $has_nix -eq 1 ] && options="$options\n    - Nix: nix profile install nixpkgs#llama-cpp"
      if [ -z "$options" ]; then
        options="\n    - Homebrew (Recommended): brew install llama.cpp\n    - Conda-forge: conda install -c conda-forge llama-cpp\n    - MacPorts: sudo port install llama.cpp\n    - Nix: nix profile install nixpkgs#llama-cpp"
      fi
      printf "install llama.cpp >= b8530:%b" "$options"
      ;;
    Linux)
      options=""
      [ $has_brew -eq 1 ] && options="$options\n    - Homebrew: brew install llama.cpp"
      [ $has_conda -eq 1 ] && options="$options\n    - Conda-forge: conda install -c conda-forge llama-cpp"
      [ $has_nix -eq 1 ] && options="$options\n    - Nix: nix profile install nixpkgs#llama-cpp"
      if [ -z "$options" ]; then
        options="\n    - Homebrew: brew install llama.cpp\n    - Conda-forge: conda install -c conda-forge llama-cpp\n    - Nix: nix profile install nixpkgs#llama-cpp\n    - Build from source: see https://github.com/ggml-org/llama.cpp/blob/master/docs/install.md"
      fi
      printf "install llama.cpp >= b8530:%b" "$options"
      ;;
    *)
      printf "install llama.cpp >= b8530 (see https://github.com/ggml-org/llama.cpp/blob/master/docs/install.md)"
      ;;
  esac
}

say "Runtime dependencies:"
case "$(uname -s)" in
  Darwin) check pdftoppm "brew install poppler"
          check llama-server "$(llama_hint)" ;;
  Linux)  check pdftoppm "apt install poppler-utils  /  dnf install poppler-utils"
          check llama-server "$(llama_hint)" ;;
  *)      check pdftoppm "install poppler"
          check llama-server "$(llama_hint)" ;;
esac

case ":$PATH:" in
  *":$BINDIR:"*) ;;
  *) say "note: $BINDIR is not on PATH; add it to your shell profile." ;;
esac
say "Done. Run: $NAME --help"
