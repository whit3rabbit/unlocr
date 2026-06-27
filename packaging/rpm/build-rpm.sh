#!/usr/bin/env bash
# Build an .rpm from the prebuilt binary via rpmbuild. Driven by the Makefile.
set -euo pipefail

NAME=${NAME:-unlocr}
VERSION=${VERSION:?set VERSION}
BIN=${BIN:?set BIN (path to release binary)}
OUT=${OUT:-dist}
SPEC="$(cd "$(dirname "$0")" && pwd)/unlocr.spec"

[ -x "$BIN" ] || { echo "binary not found/executable: $BIN" >&2; exit 1; }
command -v rpmbuild >/dev/null || { echo "rpmbuild not installed (dnf install rpm-build)" >&2; exit 1; }

TOP=$(mktemp -d)
trap 'rm -rf "$TOP"' EXIT
mkdir -p "$TOP/SOURCES" "$TOP/RPMS"
cp "$BIN" "$TOP/SOURCES/unlocr"

rpmbuild -bb \
  --define "_topdir $TOP" \
  --define "_sourcedir $TOP/SOURCES" \
  --define "version $VERSION" \
  "$SPEC"

mkdir -p "$OUT"
find "$TOP/RPMS" -name '*.rpm' -exec cp {} "$OUT/" \;
echo "wrote:"; ls -1 "$OUT"/*.rpm
