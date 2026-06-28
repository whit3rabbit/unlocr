# Releasing unlocr

CLI + GUI ship from one `v*` tag. Follow this checklist; do not skip the gates.

## Version: one source of truth

Bump **only** `[workspace.package].version` in the root `Cargo.toml`:

```toml
[workspace.package]
version = "X.Y.Z"
```

Everything else derives from it. Do NOT edit a version anywhere else:

- **CLI** — clap `#[command(version)]` reads `CARGO_PKG_VERSION` (the root crate).
- **GUI crate** — `unlocr-gui` uses `version.workspace = true`.
- **GUI bundle** — `tauri.conf.json` has no `version`, so Tauri falls back to the gui
  crate's Cargo.toml version. This sets the dmg/msi filenames (`unlocr_X.Y.Z_<arch>.dmg`),
  which the Homebrew cask URLs depend on.
- `release.sh` and the `Makefile` read this line via `sed`.

After bumping, run a build so `Cargo.lock` updates, then commit the bump + lockfile.

## Pre-release gates (run from repo root, all must pass)

```bash
cargo fmt --all                                          # format
cargo clippy --workspace --all-targets -- -D warnings    # lint, warnings = errors
cargo test --workspace --locked                          # tests (CLI + gui crate)
cargo doc --workspace --no-deps                          # docs build clean
cargo build --workspace --locked                         # CLI + Tauri gui compile
cargo publish --dry-run --locked --manifest-path Cargo.toml   # crates.io metadata OK
node --check gui/src/main.js                             # cheap static-frontend gate
```

`cargo fmt --all -- --check` instead of `--all` if you want a no-write verification in CI.

## Tag + publish

```bash
./release.sh
```

`release.sh` enforces: on `main`, clean tree, local == remote, CI green, tag does not
exist. It reads the version from `Cargo.toml`, signs/annotates the tag, and pushes it.
The tag push fires three workflows:

- `release.yml` — CLI binaries (4 targets) + CLI `.deb`/`.rpm` (amd64).
- `release-gui.yml` — GUI bundles (dmg/msi/AppImage/deb/rpm) via tauri-action.
- `publish-crate.yml` — `cargo publish` of the CLI to crates.io.

## After the release is fully populated

The CLI + GUI assets land across many matrix jobs. Once the GitHub Release shows all
of them, run the tap updater (manual, by design, to avoid racing partial uploads):

- Actions → **update-tap** → Run workflow → `tag = vX.Y.Z`.

It renders the formula + cask from the release assets (computing sha256s) and pushes
them to `whit3rabbit/homebrew-tap`. Templates live in `packaging/homebrew/`.

## One-time setup (must exist before the first release)

- Repo secret `CARGO_REGISTRY_TOKEN` — crates.io publish token.
- Repo secret `HOMEBREW_TAP_TOKEN` — PAT with write access to `whit3rabbit/homebrew-tap`.
- The tap repo `whit3rabbit/homebrew-tap` (exists).

## Verify after install

```bash
cargo install unlocr && unlocr --version          # crates.io
brew install whit3rabbit/tap/unlocr               # CLI formula
brew install --cask whit3rabbit/tap/unlocr        # GUI cask
brew audit --formula whit3rabbit/tap/unlocr
brew audit --cask whit3rabbit/tap/unlocr
```

## Known limits

- CLI `.deb`/`.rpm` are **amd64 only** (no arm64; needs a cross toolchain / arm runner).
- macOS GUI is **unsigned/un-notarized**; the cask documents the Gatekeeper workaround.
- `tauri-action` is pinned to `@v0`, not a commit SHA (see the TODO in `release-gui.yml`).
