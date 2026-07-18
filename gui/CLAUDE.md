# unlocr GUI (Tauri 2)

Desktop front end for `unlocr`. Wraps the core OCR pipeline; no OCR logic lives here.

## Layout
- `src/`            Frontend: vanilla HTML/CSS + native ES modules, no bundler, no npm. Static.
  - `index.html`, `styles.css`; JS split into ES modules booted from `main.js`:
    - `tauri.js`: window.__TAURI__ core bridge wrapper.
    - `paths.js`: path parsing and date formatting helpers.
    - `model.js`: model loading and preset selectors.
    - `options.js`: form-options parser.
    - `run.js` & `run_ocr.js`: run button bindings, sequential batch execution, drag-drop import, and `run_ocr` event orchestration.
    - `ocr-events.js`: Tauri event listeners and preflight checks on load.
    - `panes.js` & `./markdown_pane.js`, `./preview_pane.js`, `./file_rail.js`: markdown editor, PDF preview, and workspace file rail controllers.
    - `ui.js`: global UI transitions, status messages, and streaming text buffering.
    - `jobs.js` & `./library.js`, `./board.js`, `./job_card.js`, `./rail.js`: library database grid, kanban board, and jobs view handlers.
    - `settings.js`: settings configuration pane.
    - `toasts.js`: notifications/toasts management.
    - `assets/i18n.js` + `locales/{en,zh,ja,ko}.json`: i18n runtime + translation strings (see Internationalization below).
- `src-tauri/`      Rust backend (crate `unlocr-gui`, lib name `gui_lib`).
  - `src/lib.rs`    `run()` builder + `generate_handler!`. `src/main.rs` calls `gui_lib::run()`.
  - `src/cmd_model/` directory: model loading/management Tauri commands (`mod.rs`, `cache.rs`, `sysreq.rs`).
  - `src/cmd_run/` directory: running, preflight, tools, and safe filesystem Tauri commands (`mod.rs`, `fs.rs`, `render.rs`, `tools.rs`).
  - `src/cmd_store.rs` Tauri commands wrapper for the jobs store.
  - `src/db.rs`     SQLite (`rusqlite`) backing for all persisted stores: one `unlocr.db`
                    in the app-DATA dir, one warm `Connection` behind a `Mutex` (`with_db`).
                    Replaced the old per-store JSON files under the cache dir.
  - `src/store/`    directory: typed jobs accessors over `db.rs` (`mod.rs`, `db.rs`, `types.rs`, `helpers.rs`, `tests.rs`).
  - `src/settings.rs`/`notifications.rs`  Typed accessors over `db.rs` (settings; notifications).
  - `src/state.rs`  `AppState` (warm model handle + read allowlist).
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
- `read_text_file(path)` enforces a `.md`-only, BACKEND-DERIVED allowlist: it serves only
  files the app itself produced (`AppState.read_allow`, the paths `run_ocr` wrote this session,
  plus non-empty `output_path`s in the job store). The match is exact after canonicalize, not a
  dir prefix. No `allowedDir` arg: the renderer cannot widen the read scope. `check_readable`
  in `src/cmd_run/fs.rs` is the pure, unit-tested core.
- `write_text_file`/`export_markdown` reuse `read_text_file`'s `check_readable` allowlist
  (read==write scope). Export writes a SIBLING of the allowlisted source (backend-derived
  path, renderer can't choose it); `export_markdown` shells pandoc. Dep-downloader commands
  in `src/cmd_run/tools.rs`: `list_tools` (status), `download_tool` (direct fetch), `host_os`,
  `brew_available`, `brew_install` (allowlisted formulae only). UI: direct Download where
  `downloadable` (Win all; mac pandoc); else on mac an "Install with Homebrew" button when
  `brew_available`, else copyable Homebrew guidance.
- Job-store commands (`list_jobs`, `jobs_store_path`, `record_job`) wrap `src/store/mod.rs`;
  `record_job` fires after each run_ocr completes/fails. Defaults mirror `OcrOptions::default()`.
- Native file pickers feeding a backend read open the dialog SERVER-SIDE
  (`app.dialog().file().blocking_pick_file()`; see `pick_password_file` in
  `cmd_run/render.rs`) so the renderer never supplies an arbitrary path -- same
  "renderer can't widen the read scope" invariant as `read_text_file`. Backend
  `DialogExt` calls bypass the capability ACL (no `dialog:` permission needed).

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
- Vendored UMD libs (`EasyMDE`, `DOMPurify`, `hotkeys`) live in `src/assets/`, loaded as classic
  `<script>` in `index.html <head>` BEFORE the deferred `main.js` module so the globals
  exist when `makeMarkdownPane()` runs. New frontend dep = drop the file + add the tag
  (no npm/bundler); ask first.
- **Accessibility & Screen Readers**:
  - Global hotkeys mapping (`Alt+W/L/B/R/S`, `Alt+I`, `Ctrl+Enter`, `Alt+M`, `Alt+N`) are registered via `hotkeys-js` at the end of `DOMContentLoaded` inside `main.js` to allow fast keyboard navigation.
  - `hotkeys.filter` (set in `main.js`): inside an input/textarea/contenteditable it
    returns `ctrlKey || metaKey` ONLY. That lets the Run shortcut (`ctrl+enter` /
    `cmd+enter`) fire from a text field, INCLUDING macOS Cmd, while `Alt+Enter` is left
    as a normal key (newline / IME), not a hijacked Run trigger. `Alt+*` nav shortcuts
    still work when focus is NOT in a text field (the filter returns true). Do NOT add
    `altKey` to the editable-branch filter: it hijacks text entry.
  - The Markdown review editor is configured with `codeMirrorOptions: { inputStyle: "contenteditable" }` in `markdown_pane.js`. This exposes a native `contenteditable` container to screen readers and OS accessibility trees (mimicking CodeMirror 6's accessibility behavior within our CodeMirror 5-based EasyMDE instance) to fix virtual cursors and selection announcements.
- EasyMDE's default toolbar icons are FontAwesome auto-fetched from a CDN -> blocked by
  our CSP (`style-src`/`font-src 'self'`) and broken offline. Use a text-label toolbar
  over EasyMDE's static actions + `autoDownloadFontAwesome:false` in `src/markdown_pane.js`.
- OS is compile-time only (the GUI ships per-platform, release-gui.yml). The frontend
  reads it via the `host_os` command; the Windows dep-download UI shows only when
  `list_tools` reports `downloadable`. Do NOT add runtime OS detection.
- Engine backend mode + remote URL/key/model are read ONLY at `load_model` time (model
  is held warm); `run_ocr` uses the loaded server and does NOT re-read engine fields.
  Backend picker is `#enginePreset` (llamacpp=managed local, vllm/sglang/custom=remote);
  `applyPreset()` in model.js drives field visibility + URL prefill. The remote
  URL/key/model + custom GGUF/projector fields live in `#engineDialog` (native
  `<dialog>`, opened by the Modify button via `wireEngineDialog`), NOT inline in
  `.model-bar`; their ids are unchanged so `load_model` reads them the same.
  (mlx = local mlxcel, Apple Silicon only; picks its model repo via `#optQuant`, below.)
- `#optQuant` (+ `#setQuant`/`#qsQuant`) is SHARED by both LOCAL engines: `applyPreset`
  fills it with GGUF quants (`list_available_quants`) for llamacpp, static `MLX_QUANTS`
  repo ids for mlx; hidden only for a true remote endpoint. `populateQuantSelects` MUST
  guard `activeEngineMode()==='mlx'` (re-render MLX_QUANTS, don't fall through) or a GGUF
  refetch / locale switch clobbers the MLX options. mlx load reads the model repo from
  `#optQuant` (sent as `model`), NOT `#remoteModel`. `MLX_QUANTS`/`MLX_DEFAULT_MODEL` in
  model.js are hand-mirrored from `src/server/mlx.rs` `recommend_model`/`DEFAULT_MODEL` --
  keep in sync (like `TASK_PROMPTS`).
- `frontendDist` is `../src` (static files served as-is).
- Native file picker: `tauri-plugin-dialog` (added). Init'd in `run()`,
  permission `dialog:default` in `capabilities/default.json`. The Import button
  calls `window.__TAURI__.dialog.open(...)` (exposed by the plugin's init IIFE
  under `withGlobalTauri`, no npm). Single-select seeds `#pdfPath`; batch import
  stays on drag-drop. Plugins added: `opener`, `dialog`. Ask before adding more.
- `TASK_PROMPTS` (options.js, exported) hardcodes the same prompt strings as Rust
  `Task::prompt()` (src/cli_args.rs); no shared source. Edit both. The Prompt box is an
  optional override: empty -> the selected Task preset is sent (`readRunOptions`), filled
  -> sent verbatim. Settings `default_prompt` defaults to "" (empty box); a non-empty
  value seeds the run box. Unlimited-OCR uses NO system prompt; do not add one.

## Internationalization (i18n)
- Runtime: `src/assets/i18n.js` is a classic `<script>` in `<head>` (loads before the
  deferred `main.js` module), exposing `window.unlocrI18n` (`t`, `apply`, `setLocale`,
  `onLocaleChange`, `ready`) plus a short `window.t`. Strings live in
  `src/locales/{en,zh,ja,ko}.json`: a flat `dotted.key` -> string map with
  `{placeholder}` substitution via `t(key, { placeholder })`. A missing key renders as
  the raw dotted key (visible, not blank), so the locale files MUST stay in KEY PARITY
  (every file has the same key set).
- Static text: HTML carries an English default plus a `data-i18n="key"` attr
  (textContent), `data-i18n-ph` (placeholder), or `data-i18n-aria` (aria-label).
  `applyText()` walks those on every locale load; the English default is the
  progressive-enhancement fallback before the (async) locale fetch resolves.
- Adding a string: add the `dotted.key` to ALL locale files (en + zh/ja/ko). Keep the
  files sorted alphabetically, 2-space indent, raw UTF-8 (no `\uXXXX` escapes) by
  re-dumping: `json.dumps(d, ensure_ascii=False, indent=2, sort_keys=True)` + trailing
  newline (the existing files round-trip cleanly through exactly this). Verify parity
  afterward: every locale must have the same key set (a one-line python key-set diff
  against en catches a forgotten file).
- Adding a locale: drop `locales/<tag>.json` with full key parity with en, add the tag
  to `AVAILABLE` in `i18n.js`, and add the `<option value="<tag>">` to `#localeSelect`
  in `index.html`. Locale resolution: exact tag, then primary subtag (zh-CN -> zh),
  then en.
- Persistence: the chosen locale is stored in `localStorage` (`unlocr.locale`), NOT
  the SQLite settings store. It is a frontend-only preference (the Rust backend never
  reads it), and localStorage is synchronous so `boot()` can restore it with no flash
  of the wrong language and no DB schema migration. `boot()` order: saved locale >
  `navigator.language`. The `#localeSelect` change handler calls `setLocale`, which
  loads + applies + persists.
- Dynamic text (set imperatively via `t()`, NOT a `data-i18n` node) does NOT
  auto-translate: register an `onLocaleChange(fn)` listener so it re-renders on a live
  switch AND on the initial load (listeners fire inside `useLocale` after the dict
  loads, so this also covers the brief pre-load window). Existing hooks: model bar
  (`refreshModelStatus` in main.js), quant labels (`markCachedQuants`), sysreq panel
  (`wireSystemRequirements`), Load-button label (`updateLoadLabel`), notify panel.
  NEVER infer UI state from a translated string: the old `updateLoadLabel` compared the
  button's `textContent` to `tr("model.reload")` and broke on locale switch (the button
  held the old-language text while `tr()` returned the new one). Track state in a
  flag/variable (`modelLoaded`) and re-derive the label.
- Gates after an i18n edit: `node --check src/assets/i18n.js` (and any edited module),
  and confirm every `locales/*.json` parses and is in key parity.

## Build / run (from gui/)
- `node --check src/main.js`  # cheapest gate after a JS edit (no bundler/test on the static frontend)
- `cargo tauri dev`     # dev window, hot-reloads frontend on file change
- `cargo tauri build`   # bundle for the host OS
- `cargo build` in `src-tauri/` compiles the backend + linked `unlocr` (link check).
- Needs the Tauri CLI: `cargo install tauri-cli` (or `cargo tauri` if already present).

## Runtime deps (same as the CLI, NOT bundled)
- `pdftoppm` (poppler) must be on PATH / Homebrew prefixes.
- `llama-server`: the Unlimited-OCR model needs unlocr's patched R-SWA build (PR #24975,
  unmerged upstream). `load_model` calls `unlocr::preflight::check`, which now AUTO-DOWNLOADS
  the managed build (progress via the existing `ocr://progress` / `Progress::Download` path,
  `name: "llama-server"`, shown on the model bar). The Settings > Dependencies panel also
  offers a Download button (llama-server is now `downloadable` on mac/linux/win); its
  `TOOL_INFO` entry has NO `brew` field because Homebrew's llama.cpp is stock and lacks
  R-SWA. The passive `preflight` status command (`cmd_model/cache.rs`) uses the
  NON-downloading `find_llama_server`, so opening the panel never pulls the binary. An
  external build (PATH/brew/`--llama-bin`) is an unverified fallback; silence its warning
  with `UNLOCR_ALLOW_EXTERNAL_LLAMA=1`.
- `pandoc` is an OPTIONAL, GUI-only runtime dep: used ONLY by the review-pane export
  (`export_markdown`, md -> docx/odt/rtf/html/txt). Declared as a WEAK dep in the GUI
  deb/rpm (`tauri.conf.json` bundle.linux deb.recommends / rpm.recommends) and a hard
  cask dep (`packaging/homebrew/unlocr-cask.rb`); NOT on the CLI (the CLI has no
  export). Missing pandoc disables export only (cross-platform install hint shown),
  never OCR. Resolved via `preflight::locate` like the other tools.

## Gotchas
- `spawn_blocking` keeps Rust off the UI thread, but a high-rate `emit` (e.g. the
  per-token `ocr://partial-text` stream) still freezes the WEBVIEW: each event runs
  its JS handler, and per-token DOM writes + forced reflow (`scrollTop`) starve the
  event loop so clicks (Stop) never run. Throttle DOM writes (buffer + flush per
  requestAnimationFrame) and cap rendered text. See ui.js `appendPartial`/`flushPartial`.
- macOS orphan: the warm model is held in `AppState::model` for the whole app lifetime,
  so a SIGKILL/segfault/`panic=abort` that skips `Drop` + the `RunEvent::Exit` cleanup
  strands a multi-GB `llama-server`. Linux/Windows backstop this with
  `PR_SET_PDEATHSIG`/Job Objects; macOS has neither, and a parent-side watchdog can't
  help (it dies with the app). If a user force-quits, tell them to `pkill llama-server`.
  See `src/server/local.rs` `Drop` + `Server::start`.
- System Requirements panel: the `system_requirements` command returns the lib's
  `unlocr::preflight::sysreq::SystemInfo` directly. Its `Status` enum serializes
  lowercase (`good`/`marginal`/`insufficient`/`unknown`) and the struct is camelCase,
  which is exactly what `settings.js` reads (`metrics[].status`, `verdict`,
  `verdictLabel`). Do NOT re-declare a parallel `Metric`/report DTO + `status_str` in
  the GUI; keep the one wire type in the lib. The static probes are memoized in the lib
  (`OnceLock`), so startup + Recheck do not re-shell `system_profiler`/`lspci` each
  time (disk free is re-probed live). Metric labels + the verdict are localized via
  the `sysreq.label.*` / `sysreq.verdict.*` locale keys; the per-metric recommendation
  HINTS still come from the backend as English (localizing them needs a Rust change).
- Settings persistence has 3 writers sharing one singleton DB row (Settings pane
  Save, Quick Settings popup, Workspace auto-save `wireAutoSaveEngineOptions`).
  All 3 MUST go through `patchSettings()` (settings.js), which refetches the row
  fresh before spreading overrides -- never cache a "baseline" row across saves
  (a stale closure-cached baseline silently reverted other surfaces' saves).
  Never call `applySettingsToControls` (full-restore) after a narrow/partial
  save -- it force-writes every Workspace field from the DB and clobbers live
  uncommitted edits elsewhere; sync only the fields that save actually owns.
  Add a new Advanced-panel knob to `SYNCED_FIELDS` in settings.js (single list
  driving both restore and the auto-save trigger ids), not two hand-synced lists.
- `db.rs` schema migrations: wrap multi-statement `ALTER TABLE` batches in
  `conn.unchecked_transaction()` (works on `&Connection`, no `&mut` needed).
  Bare `execute_batch` runs each statement autocommit -- a mid-batch failure
  leaves partial columns added but `user_version` un-bumped, so the next
  launch retries and hits "duplicate column name" forever.

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
- EH-0006 Job store (`src/store/`), Library grid, Board columns, drag-drop import. All 4 bites done.
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
