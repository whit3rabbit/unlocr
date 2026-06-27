#!/usr/bin/env sh
# Uninstall unlocr (macOS / Linux): remove the binary AND the model cache.
# Does NOT remove llama.cpp or poppler (installed separately).
#
#   ./uninstall.sh                 # remove from /usr/local/bin + delete cache
#   PREFIX=$HOME/.local ./uninstall.sh
set -eu

NAME=unlocr
PREFIX=${PREFIX:-/usr/local}
BINDIR="$PREFIX/bin"

say() { printf '%s\n' "$*"; }

# Cache dir resolution MUST match unlocr/src/model.rs::base_cache_dir.
cache_dir() {
  if [ "${XDG_CACHE_HOME:-}" != "" ]; then
    printf '%s/%s\n' "$XDG_CACHE_HOME" "$NAME"; return
  fi
  case "$(uname -s)" in
    Darwin) printf '%s/Library/Caches/%s\n' "$HOME" "$NAME" ;;
    *)      printf '%s/.cache/%s\n' "$HOME" "$NAME" ;;
  esac
}

# Remove binary (sudo only if target dir isn't writable, mirroring install.sh).
BIN="$BINDIR/$NAME"
if [ -e "$BIN" ]; then
  SUDO=""
  if [ ! -w "$BINDIR" ] && [ "$(id -u)" -ne 0 ]; then
    command -v sudo >/dev/null 2>&1 && SUDO=sudo
  fi
  $SUDO rm -f "$BIN"
  say "Removed $BIN"
else
  say "No binary at $BIN (skipping)"
fi

# Remove model cache.
CACHE=$(cache_dir)
if [ -n "$CACHE" ] && [ "$CACHE" != "/" ] && [ -d "$CACHE" ]; then
  SIZE=$(du -sh "$CACHE" 2>/dev/null | cut -f1)
  rm -rf "$CACHE"
  say "Removed model cache $CACHE (${SIZE:-?} freed)"
else
  say "No cache at $CACHE (skipping)"
fi

say "Done. (llama.cpp and poppler were not touched.)"
