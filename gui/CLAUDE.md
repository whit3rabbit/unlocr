# unlocr GUI (Tauri 2)

Desktop front end for `unlocr`. Wraps the core OCR pipeline; no OCR logic lives here.

## Layout
- `src/`            Frontend: vanilla HTML/JS/CSS, no bundler, no npm. Static.
  - `index.html`, `main.js` (calls `window.__TAURI__.core.invoke`), `styles.css`.
- `src-tauri/`      Rust backend (crate `unlocr-gui`, lib name `gui_lib`).
  - `src/lib.rs`    Tauri commands + `run()` builder. `src/main.rs` calls `gui_lib::run()`.
  - `src/store.rs`  Job persistence: appends each run to a JSON file under the
                    model cache dir (no extra dep). Powers Library/Board views.
  - `Cargo.toml`, `tauri.conf.json`, `capabilities/`, `icons/`.

## Library link (the important part)
- The core lives in the repo-root `unlocr` crate (`../../src/lib.rs`), pulled in via
  `unlocr = { path = "../.." }` in `src-tauri/Cargo.toml`.
- Commands build `unlocr::OcrOptions` and call `unlocr::run_ocr_job` (clap-free,
  takes a `Progress` sink) / `unlocr::preflight::check`. Note: the GUI's own Tauri
  command is also named `run_ocr` (different thing); the core fn is `run_ocr_job`.
- Keep this crate a thin shim. New OCR behavior goes in `unlocr` (root `src/`), then
  gets exposed here as a `#[tauri::command]`. Do not fork pipeline logic into the GUI.

## Command conventions
- OCR is long-running (model download + per-page inference). Run it on
  `tauri::async_runtime::spawn_blocking` so the webview never freezes.
- `unlocr`'s error is `Box<dyn Error>` (not Send). Convert to `String` INSIDE the
  blocking closure before returning, so the future stays Send.
- Register every command in `generate_handler![...]` in `run()`.
- Job-store commands (`list_jobs`, `jobs_store_path`, `record_job`) wrap `store.rs`;
  `record_job` fires after each run_ocr completes/fails. Defaults mirror `OcrOptions::default()`.

## PDF preview + page-image cache
- Core fn `unlocr::render_pages(pdftoppm, pdf, dpi, cache_root)` rasterizes to
  `<cache_root>/previews/<key>/page-N.png`, key = hash(canonical path + mtime +
  dpi). Repeat preview = cache hit, no pdftoppm. Separate from `ocr_pages` (which
  keeps its own tempdir), so the CLI path is unchanged. Unbounded cache (ponytail).
- GUI command `render_pages` returns PNG paths; the frontend loads them via the
  **asset protocol** (`window.__TAURI__.core.convertFileSrc(path)`), NOT file://.
- Asset protocol setup (3 coupled pieces, all required):
  1. `tauri` feature `protocol-asset` (Cargo.toml) — compiles in the protocol +
     `asset_protocol_scope()`.
  2. `security.assetProtocol.enable=true` + `img-src ... asset: http://asset.localhost`
     in `tauri.conf.json`.
  3. `run()`'s `.setup()` calls `app.asset_protocol_scope().allow_directory(previews, true)`
     — the cache dir is per-OS/runtime, so the scope is empty in config and extended here.

## Frontend conventions
- `withGlobalTauri: true` (see tauri.conf.json), so JS uses `window.__TAURI__.core.invoke`,
  not an npm `@tauri-apps/api` import. No build step: edit JS, reload.
- `frontendDist` is `../src` (static files served as-is).
- Native file picker: `tauri-plugin-dialog` (added). Init'd in `run()`,
  permission `dialog:default` in `capabilities/default.json`. The Import button
  calls `window.__TAURI__.dialog.open(...)` (exposed by the plugin's init IIFE
  under `withGlobalTauri`, no npm). Single-select seeds `#pdfPath`; batch import
  stays on drag-drop. Plugins added: `opener`, `dialog`. Ask before adding more.

## Build / run (from gui/)
- `cargo tauri dev`     # dev window, hot-reloads frontend on file change
- `cargo tauri build`   # bundle for the host OS
- `cargo build` in `src-tauri/` compiles the backend + linked `unlocr` (link check).
- Needs the Tauri CLI: `cargo install tauri-cli` (or `cargo tauri` if already present).

## Runtime deps (same as the CLI, NOT bundled)
- `pdftoppm` (poppler) and `llama-server` (llama.cpp >= b8530) must be on PATH /
  Homebrew prefixes. The `preflight` command surfaces missing ones to the UI.

## Eatahorse run log (2026-06-27/28)

Board: `.eatahorse-goal-build-the-ferrum-ocr-desktop-gui-fr` at repo root
(goal: build the Ferrum OCR desktop GUI from the Tauri PDF OCR app HTML mockup,
wired into the `src/` Rust OCR backend). 41 iterations; cleared via "board cleared".

### Done
- EH-0001 Scaffold the Tauri app shell in `gui/` (header + sidebar frame, workspace member).
- EH-0002 Extract `unlocr` lib from `src/` (additive: `src/lib.rs`, `OcrOptions`,
  `run_ocr_job`, `Progress` sink). CLI behavior byte-identical, 21 tests pass.

### Remaining (code complete, verification blocked)
- EH-0003 Tauri commands `preflight` + `run_ocr` with `ocr://` progress events. All 3 bites done.
- EH-0004 Port the mockup UI into `gui/src/` (Workspace panes, options form, event subs). All 5 bites done.
- EH-0006 Job store (`store.rs`), Library grid, Board columns, drag-drop import. All 4 bites done.
- EH-0007 Review view (rendered markdown + diff between two runs). Backlog, untouched.
- EH-0008 Settings view (persist engine/provider/privacy/routing). Backlog, untouched.

### Why every blocked card is blocked (treat as constraints, do not re-attempt headless)
The implementation for EH-0003/0004/0006 is finished (all bites closed, `cargo build
--workspace` green, forbidden paths untouched). The ONLY open items are acceptance
checks that require a live desktop session a headless subagent cannot produce. A rerun
will hit the same wall unless one of these is provided first:
1. A running `cargo tauri dev` window with devtools open (needed to capture
   `invoke('preflight')` JSON, the ordered `ocr://` event log, the options-vs-invoke
   cross-check, and screenshots).
2. A populated model cache (GGUF under `~/Library/Caches/unlocr`), or a cached-path
   override; the cache is empty and the real download is multi-GB.
3. A sample one-page PDF somewhere in or near the repo; none exists.
4. For EH-0003 acceptance 4 specifically: `pgrep -f llama-server` must return nothing
   after a successful run (no orphan) on the success path.
Do NOT re-run these cards headless. Instead either (a) run them in an interactive
session with the above three preconditions, or (b) add a `#[test]` harness that calls
the command handlers directly (e.g. `preflight()` returning a `PreflightReport`,
`run_ocr` against a fixture PDF with a stubbed server) to satisfy the runtime-gated
acceptance without a GUI.

### Board invariants this run honored (keep honoring on a rerun)
- `cargo build --workspace` at repo root stays green after every card; the completion
  gate runs it before accepting a card as done.
- The existing `unlocr` CLI keeps its current behavior: the backend bridge is additive
  (`src/lib.rs`), not a rewrite of the CLI path. `--help` and `doctor` stay byte-identical.
- Scope: edits land only in `gui/` (the Tauri app dir) and the `src/` Rust bridge.
  Do NOT modify `packaging/`, `install.sh`, make targets, the `Tauri PDF OCR app/`
  mockup directory, or Cargo release profiles. (EH-0001 was once reopened for violating
  exactly this; forbidden-path edits were reverted via `git checkout HEAD`.)
