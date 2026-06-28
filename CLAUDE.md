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

## Tauri GUI run log (2026-06-28)

Goal: Finish the Tauri 2 desktop front end -- harden single-PDF run flow, complete
Workspace UX, fill stubbed views, fix packaging/CI. (46 iterations, board: `.eatahorse`)

### Completed (17 tasks)

- GUI-01: cross-platform path helper for Windows .md output paths
- GUI-02: `keep_images` wired end-to-end; `ocr://images-kept` event surfaced in UI
- GUI-03: `subscribeOcrEvents` awaited before `invoke('run_ocr')` (listeners-before-invoke proven by code at main.js:904/918)
- GUI-04: redundant preflight removed; single `preflight::check` per run
- GUI-05: real CSP set in tauri.conf.json; `read_text_file` restricted to .md files via allowlist (not just OS denylist)
- GUI-06: `validate_quant` added to `model::check_presence`
- GUI-07: `tauri-plugin-dialog` added; native OS file picker wired to field + file list
- GUI-08: streaming token transcript -- SSE parse in server.rs, `Progress::PartialText`, `ocr://partial-text` event, live append in main.js
- GUI-09: editable prompt row in index.html; defaults to `OcrOptions::default().prompt`; forwarded in run payload
- GUI-10: engine segmented control removed (only one engine exists)
- GUI-11: quality tiers map best/good/less to BF16/Q8_0/Q4_K_M
- GUI-13: batch runs -- multi-file list, per-file progress in main.js (lib.rs already looped)
- GUI-14: Library view -- `history.json` in Tauri app-config dir; `load_history`/`append_history` commands; past runs list + re-open in review pane
- GUI-15: Settings view -- `settings.json` in app-config dir; model-dir, llama-bin, quant/DPI/max-tokens, cache-clear wired
- GUI-16: Board view -- kanban in index.html (`data-view=board`); `makeBoard()` fed by `list_jobs`; `recordRunOutcome` fires on done/failed
- GUI-17: app icons verified present (32x32.png, icon.icns, icon.ico, etc.)
- GUI-18: root CI extended to cover gui crate; Cargo.toml overclaiming comment fixed

### Blocked

**EH-0011 -- GUI-12 PDF preview pane** (status: `blocked`, path: `.eatahorse/tasks/blocked/EH-0011-gui-12-pdf-preview-pane.md`)

Both implementation bites were completed (lib.rs `rasterize` command + main.js page/zoom
render), but the acceptance check ("Selected PDF first page renders and page/zoom controls
work") was never verified. The runner moved it to blocked without logging an explicit
reason; by analogy with EH-0003 and EH-0010 (same pattern in this session), the block is
a runtime verification gap: no live desktop session, no populated model cache, no sample
PDF, and no automated harness substitutes for visual confirmation.

Constraint for the next run: do NOT re-check acceptance speculatively. To close this task
you need either (a) a `cargo tauri dev` session with a real PDF that confirms the first
page image appears and page/zoom controls respond, or (b) a `#[tauri::command]` test
that calls the rasterize command and asserts a non-empty PNG is returned.
