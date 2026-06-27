# unlocr GUI (Tauri 2)

Desktop front end for `unlocr`. Wraps the core OCR pipeline; no OCR logic lives here.

## Layout
- `src/`            Frontend: vanilla HTML/JS/CSS, no bundler, no npm. Static.
  - `index.html`, `main.js` (calls `window.__TAURI__.core.invoke`), `styles.css`.
- `src-tauri/`      Rust backend (crate `gui`, lib name `gui_lib`).
  - `src/lib.rs`    Tauri commands + `run()` builder. `src/main.rs` calls `gui_lib::run()`.
  - `Cargo.toml`, `tauri.conf.json`, `capabilities/`, `icons/`.

## Library link (the important part)
- The core lives in the repo-root `unlocr` crate (`../../src/lib.rs`), pulled in via
  `unlocr = { path = "../.." }` in `src-tauri/Cargo.toml`.
- Commands build `unlocr::Options` and call `unlocr::run_ocr` / `unlocr::preflight::check`.
- Keep this crate a thin shim. New OCR behavior goes in `unlocr` (root `src/`), then
  gets exposed here as a `#[tauri::command]`. Do not fork pipeline logic into the GUI.

## Command conventions
- OCR is long-running (model download + per-page inference). Run it on
  `tauri::async_runtime::spawn_blocking` so the webview never freezes.
- `unlocr`'s error is `Box<dyn Error>` (not Send). Convert to `String` INSIDE the
  blocking closure before returning, so the future stays Send.
- Register every command in `generate_handler![...]` in `run()`.

## Frontend conventions
- `withGlobalTauri: true` (see tauri.conf.json), so JS uses `window.__TAURI__.core.invoke`,
  not an npm `@tauri-apps/api` import. No build step: edit JS, reload.
- `frontendDist` is `../src` (static files served as-is).
- File paths are typed in by hand. A native file picker needs the
  `tauri-plugin-dialog` dependency (not added; ask before adding deps).

## Build / run (from gui/)
- `cargo tauri dev`     # dev window, hot-reloads frontend on file change
- `cargo tauri build`   # bundle for the host OS
- `cargo build` in `src-tauri/` compiles the backend + linked `unlocr` (link check).
- Needs the Tauri CLI: `cargo install tauri-cli` (or `cargo tauri` if already present).

## Runtime deps (same as the CLI, NOT bundled)
- `pdftoppm` (poppler) and `llama-server` (llama.cpp >= b8530) must be on PATH /
  Homebrew prefixes. The `preflight` command surfaces missing ones to the UI.
