#!/usr/bin/env bash
# Build a .deb with dpkg-deb. No cargo-deb dependency; works on any host with
# dpkg-deb installed. Driven by the Makefile (NAME/VERSION/BIN in env).
set -euo pipefail

NAME=${NAME:-unlocr}
VERSION=${VERSION:?set VERSION}
BIN=${BIN:?set BIN (path to release binary)}
MAINTAINER=${MAINTAINER:-"unlocr maintainers <noreply@example.com>"}
OUT=${OUT:-dist}

[ -x "$BIN" ] || { echo "binary not found/executable: $BIN" >&2; exit 1; }
command -v dpkg-deb >/dev/null || { echo "dpkg-deb not installed (apt-get install dpkg-dev)" >&2; exit 1; }

# Debian arch name (amd64/arm64), not uname's x86_64/aarch64.
ARCH=$(dpkg --print-architecture)
ROOT=$(mktemp -d)
trap 'rm -rf "$ROOT"' EXIT

install -Dm0755 "$BIN" "$ROOT/usr/bin/$NAME"

mkdir -p "$ROOT/DEBIAN"
cat > "$ROOT/DEBIAN/control" <<EOF
Package: $NAME
Version: $VERSION
Section: utils
Priority: optional
Architecture: $ARCH
Depends: poppler-utils
Maintainer: $MAINTAINER
Description: OCR PDFs to markdown via Unlimited-OCR (DeepSeek-OCR) + llama.cpp
 Thin Rust wrapper that rasterizes PDF pages with pdftoppm (poppler-utils) and
 runs a persistent llama.cpp llama-server to convert each page to markdown.
 .
 Requires llama.cpp's llama-server (build >= b8530) on PATH. llama.cpp is not
 packaged in apt; install it separately. See the project README.
EOF

# llama-server is a runtime dep we cannot declare (not in apt). Warn, don't fail.
cat > "$ROOT/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if ! command -v llama-server >/dev/null 2>&1; then
  echo "unlocr: llama-server not found on PATH."
  echo "      Install llama.cpp (build >= b8530): https://github.com/ggml-org/llama.cpp"
fi
exit 0
EOF
chmod 0755 "$ROOT/DEBIAN/postinst"

mkdir -p "$OUT"
DEB="$OUT/${NAME}_${VERSION}_${ARCH}.deb"
dpkg-deb --build --root-owner-group "$ROOT" "$DEB"
echo "wrote $DEB"
