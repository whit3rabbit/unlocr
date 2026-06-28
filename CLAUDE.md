# unlocr

Rust CLI: OCR PDFs to markdown via the Unlimited-OCR (DeepSeek-OCR) model + llama.cpp.
Thin wrapper. Full usage/benchmarks in README.md.

## Layout
- Cargo workspace lives in repo root. Source: `src/`.
  Modules: `model` (HF download/cache), `pdf` (pdftoppm), `server` (llama-server
  spawn), `ocr`, `preflight`, `lib` (public API), `main` (clap CLI).
- Packaging (deb/rpm/installers) at repo root + `packaging/`.
- Repo, product, binary, and crate are all named `unlocr`.

## Commands (from repo root)
- `make build` / `make test`      # cargo, targets Cargo.toml
- `make install`                  # to $PREFIX/bin (default /usr/local; honors DESTDIR)
- `make deb`                      # dist/*.deb   (needs dpkg-deb)
- `make rpm`                      # dist/*.rpm   (needs rpmbuild)
- `make dist`                     # tarball
- `./install.sh`                  # macOS/Linux build+install+depcheck
- `packaging/windows/install.ps1` # Windows

## Runtime deps (external, NOT bundled)
- `pdftoppm` <- poppler-utils. Declared in deb/rpm.
- `llama-server` <- llama.cpp, build >= b8530 (PR #17400). NOT in apt/dnf; cannot
  declare as a package dep. deb postinst / rpm %post warn if missing.

## Gotchas
- Public lib API (consumed by gui crate): `run_ocr_job` + `OcrOptions` + `Progress`
  + `render_pages` (cached PDF->PNG for previews) (clap-free). Keep these stable;
  the GUI links via `path = "../.."`.
- Batch input: positionals accept files, folders, globs; `--from-list FILE` +
  `--recursive`. `expand_inputs` (main.rs) dedups/sorts to a concrete PDF list.
- Binary searches PATH then Homebrew prefixes (/opt/homebrew/bin, /usr/local/bin).
  Install hints in preflight.rs are macOS-only.
- Model GGUFs download from HF on first run, cached at per-OS dir + `/unlocr`
  (model.rs). Renaming the binary changed this path: old `uocr` caches are orphaned.
- Ctrl-C does not clean up; may orphan llama-server.
- Release profile tuned for size (opt-level=z, lto, panic=abort).
- BSD sed (macOS) has no `\b`; use plain patterns or `[[:<:]]`/`[[:>:]]`.
