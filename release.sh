#!/usr/bin/env sh
# Cut a release: tag v<version> from Cargo.toml and push it. The push
# triggers .github/workflows/release.yml, which builds per-OS binaries and
# attaches them to the GitHub Release. This script does no building itself.
#
#   ./release.sh         # tag + push current Cargo.toml version
set -eu

ROOT=$(cd "$(dirname "$0")" && pwd)
cd "$ROOT"

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

# Must be on main with a clean tree so the tag points at reviewed, pushed code.
branch=$(git rev-parse --abbrev-ref HEAD)
[ "$branch" = "main" ] || die "on branch '$branch'; release from main."
[ -z "$(git status --porcelain)" ] || die "working tree dirty; commit or stash first."

# Verify local matches remote
printf "Fetching origin to verify state...\n"
git fetch origin
git diff --quiet origin/main HEAD || die "local main has drifted from origin/main. Pull or push first."

# Verify CI status if gh is available
if command -v gh >/dev/null 2>&1; then
  printf "Verifying CI status for origin/main...\n"
  conclusion=$(gh run list --branch main --limit 1 --json conclusion -q '.[0].conclusion' 2>/dev/null)
  if [ -n "$conclusion" ] && [ "$conclusion" != "success" ] && [ "$conclusion" != "skipped" ]; then
    die "Latest CI run on main was not successful (status: $conclusion). Wait for CI to pass before releasing."
  fi
fi

VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$VERSION" ] || die "could not read version from Cargo.toml"
TAG="v$VERSION"

if git rev-parse "$TAG" >/dev/null 2>&1; then
  die "tag $TAG already exists. Bump version in Cargo.toml, commit, push, then re-run."
fi

# Make sure local main is pushed; the workflow builds from the tagged commit.
git push origin main

# Try to sign the tag; fallback to annotated tag if signing key is not configured.
if git config --get user.signingkey >/dev/null 2>&1 || git config --get commit.gpgsign >/dev/null 2>&1; then
  printf "Signing tag with GPG...\n"
  git tag -s "$TAG" -m "unlocr $VERSION"
else
  printf "warning: GPG signing key not found. Creating unsigned annotated tag instead...\n"
  git tag -a "$TAG" -m "unlocr $VERSION"
fi
git push origin "$TAG"

remote=$(git remote get-url origin | sed -E 's#(git@github.com:|https://github.com/)##; s#\.git$##')
printf 'Tagged %s and pushed.\n' "$TAG"
printf 'Watch the build: https://github.com/%s/actions\n' "$remote"
printf 'Release will appear at: https://github.com/%s/releases/tag/%s\n' "$remote" "$TAG"
