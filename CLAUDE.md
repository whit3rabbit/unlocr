# unlocr

Rust CLI: OCR PDFs to markdown via the Unlimited-OCR (DeepSeek-OCR) model + llama.cpp.
Thin wrapper. Full usage/benchmarks in README.md.

## Layout
- Cargo workspace lives in repo root. Source: `src/`.
  Modules / File Tree:
  - `lib.rs`: library entry point, exports `model`, `pdf`, `preflight`, `server`, `tools`.
  - `main.rs`: CLI binary entry point, declares `ocr`, `cli_args`, `inputs`.
  - `cli_args.rs`: CLI argument parsing / clap definition.
  - `inputs.rs`: CLI input parsing and expansion.
  - `ocr.rs`: bin-only OCR orchestrator (`run_pdf`).
  - `model/`: folder containing model cache logic (HF download/cache).
    - `mod.rs`: module entry, check_presence, list_cached_quants, etc.
    - `download.rs`: HTTP streaming download & cache population.
    - `tests.rs`: tests.
  - `server/`: folder containing llama-server and remote endpoint interaction.
    - `mod.rs`: module entry, traits, shared logic.
    - `local.rs`: `Server` manager for spawning llama-server.
    - `remote.rs`: `RemoteEndpoint` wrapper for SGLang/vLLM/OpenAI compatible backends.
    - `tests.rs`: tests.
  - `pdf.rs`: pdftoppm runner and page rasterizer.
  - `preflight.rs`: preflight checks and doctor diagnostics.
  - `preflight/`:
    - `hints.rs`: platform-specific installation hints.
    - `system.rs`: environment checking (binary locate/permissions).
  - `tools/`: folder containing the on-demand tool downloader.
    - `mod.rs`: PINS, download, extract_zip.
    - `tests.rs`: tests.
  - `lib_tests.rs`: unit tests for library.
  - `main_tests.rs`: unit tests for main CLI.
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

## Releasing
- Before any release follow `docs/RELEASE.md`. Bump the version in ONE place only:
  `[workspace.package].version` in root `Cargo.toml`. CLI (`CARGO_PKG_VERSION`),
  gui crate (`version.workspace`), and the Tauri bundle (no `version` in
  tauri.conf.json -> falls back to the crate) all derive from it.
- Always run the gates first: `cargo fmt --all`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace`, `cargo doc --workspace
  --no-deps`, `cargo build --workspace`. Then `./release.sh` tags; after the release
  populates, run the `update-tap` workflow with the tag.

## Runtime deps (external, NOT bundled)
- `pdftoppm` <- poppler-utils. Declared in deb/rpm.
- `llama-server` <- llama.cpp, build >= b8530 (PR #17400). NOT in apt/dnf; cannot
  declare as a package dep. deb postinst / rpm %post warn if missing.

## Gotchas
- `src/tools/mod.rs`: on-demand tool downloader. `PINS` is per OS+arch (cfg-selected):
  Windows = pandoc/poppler/llama-server CPU (.zip); macOS = pandoc only (per-arch .zip;
  poppler has no standalone mac binary, llama ships .tar.gz so both stay on brew); Linux
  = none. Pins (url+sha256+exe) are version-locked; bump on upgrade. The GitHub release
  API `digest` field gives the sha256 (also for `src/model/mod.rs` DIGESTS). `extract_zip` sets the
  unix exec bit from the zip entry (mac binary won't run otherwise). `preflight::locate`
  also scans `<cache>/tools/` so a downloaded tool resolves for every caller. Needs `zip`.
- OS detection is compile-time `cfg!(target_os)` everywhere (per-platform builds), never
  runtime. Tests asserting OS-gating put the `cfg!` check INSIDE the test body (runs
  per-host on CI), not `#[cfg]` on the fn, so each OS verifies its own branch.
- `cargo clippy --workspace --all-targets -- -D warnings` is GREEN; the old
  pre-existing debt was cleared. It is a real release gate (docs/RELEASE.md), so
  keep it green: your diff must add no new lints.
- Public lib API (consumed by gui crate): `run_ocr_job` + `OcrOptions` + `Progress`
  + `render_pages`/`render_page` (cached PDF->PNG for previews; the GUI preview pane
  calls the singular `render_page` per page) + `resolve_output_path` +
  `write_markdown_output` (the shared single/pages/both write sink, used by BOTH
  `ocr::run_pdf` and the GUI `run_ocr`) + `duplicate_stems` (clap-free).
  Keep these stable; the GUI links via `path = "../.."`.
- Output path is resolved by the shared `resolve_output_path(out_dir, out_file, stem)`
  called from BOTH CLI `ocr::run_pdf` and GUI `run_ocr`. `-o`/`out_file` is a single
  output file, single-input only (both paths guard `inputs.len() > 1`). It appends `.md`
  only when no extension; a custom non-`.md` name writes fine but the GUI review pane
  (`read_text_file` is `.md`-only) cannot render it.
- Bare `cargo build`/`cargo test` build the root CLI ONLY (gui is a workspace member
  with no default-members). After changing the public lib API (`OcrOptions`,
  `Server::start`, `run_ocr_job`, ...) run `cargo build --manifest-path
  gui/src-tauri/Cargo.toml` (or `cargo build --workspace`) or gui breakage stays hidden.
- Tests favor a pure helper + assert over spawning/network: extract arg-vec builders
  (e.g. `server::local::server_args`) and stub HTTP servers for the OpenAI path (`src/server/tests.rs`).
- Batch input: positionals accept files, folders, globs; `--from-list FILE` +
  `--recursive`. `expand_inputs` (`src/inputs.rs`) dedups/sorts to a concrete PDF list.
- Binary searches PATH then Homebrew prefixes (/opt/homebrew/bin, /usr/local/bin).
  Install hints in `src/preflight/hints.rs` are cross-platform (supporting macOS, Windows, and Linux via various package managers).
- Model GGUFs download from HF on first run, cached at per-OS dir + `/unlocr`
  (`src/model/mod.rs`). Renaming the binary changed this path: old `uocr` caches are orphaned.
- TWO distinct model repos, do not conflate them:
  - Local llama.cpp (managed-local path): the quantized GGUF build
    `sahilchachra/Unlimited-OCR-GGUF` (`REPO` in `src/model/mod.rs`). Downloaded + cached;
    this is what `--quant`/the GUI quality tiers select.
  - Remote GPU (`--gpu` / GUI "gpu" preset -> vLLM/SGLang): the full-precision
    original `baidu/Unlimited-OCR` (`UNLIMITED_OCR_REPO` in main.rs), sent as the
    `--endpoint-model` / request-body served name. NOT downloaded by us; the user
    runs `vllm serve baidu/Unlimited-OCR` themselves.
  Both names are wired in the GUI too (model.js gpu preset = baidu; quality tiers =
  GGUF quants). Architecture-family comments may still say "DeepSeek-OCR" (the base
  arch) and are intentionally not renamed.
  - vLLM serving of baidu/Unlimited-OCR ships ONLY as a Docker image
    (`vllm/vllm-openai:unlimited-ocr`), NOT a pip wheel; needs `--trust-remote-code`
    + logits processor `vllm.model_executor.models.unlimited_ocr:NGramPerReqLogits-
    Processor` (module `unlimited_ocr`, NOT `deepseek_ocr`) + an `<image>`-prefixed
    prompt (ngram_size/window_size via extra_body) or output is empty. Colab has no
    Docker daemon, so `colab/` uses the llama.cpp managed-local path on GPU, NOT
    `--gpu`/vLLM.
- `server::local::server_args` passes NO `-ngl`, so the managed-local llama-server runs
  CPU-only. For GPU (e.g. Colab) set env `LLAMA_ARG_N_GPU_LAYERS=99` before unlocr
  spawns it (llama-server reads `LLAMA_ARG_*` env vars); no CLI flag, no Rust change.
- llama.cpp GGUF build for Unlimited-OCR (`colab/` notebook) = clone llama.cpp +
  `pull/24975` branch + `cmake -DGGML_CUDA=ON --target llama-server`; stock llama.cpp
  won't load the DeepSeek-OCR arch (cf. the b8530/PR #17400 runtime note above).
- Two independent "resolutions": `--dpi` is the PNG pixel size pdftoppm renders;
  `--image-max-tokens` is llama-server's vision-token budget (DeepSeek-OCR base/large
  detail). They stack. image-max-tokens + `--chat-template` are llama-server *startup*
  flags (set in `Server::start`, baked at load in the GUI); `--repeat-penalty` is a
  per-request body field (in `ocr_via`/`ocr_via_stream`). `--task` is a CLI-side prompt
  preset; `--prompt` overrides it. Upstream Python knobs (`base_size`/`crop_mode`/
  gundam tiling, `no_repeat_ngram_size`) are NOT reachable via the OpenAI endpoint.
- Numeric knobs need explicit range guards in BOTH places: CLI `run()` (clap does
  not bounds-check) AND the GUI `run_ocr`/`load_model` commands (a direct `invoke()`
  bypasses the HTML `min=` form clamp). Pattern: reject `0`/non-finite/`<=0` before
  spawn (dpi, image-max-tokens, repeat-penalty all do this).
  When both front ends route through one lib fn, put the guard there as a single
  shared sink instead (e.g. `model::require_file` validates `--model`/`--mmproj` and
  the GUI `model_file`/`mmproj_file` in one place).
- A per-request body knob must be added to BOTH `ocr_via` and `ocr_via_stream`
  (stream + non-stream paths). Route it through a shared helper (`apply_repeat_penalty`).
- rust-analyzer inline diagnostics can lag the source (saw repeated false
  "no such field" on `Progress::Download {done,total}` while cargo was green).
  Trust `cargo build`/`cargo test` over the editor diagnostics; re-check, don't chase.
- Child-cleanup is platform-split (`src/server/local.rs`): Linux sets
  `prctl(PR_SET_PDEATHSIG, SIGKILL)` and Windows assigns the child to a JobObject with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so llama-server dies with the parent (incl. a
  Ctrl-C/SIGINT exit that skips `Drop`). macOS has NEITHER, so a SIGINT there can still
  orphan llama-server. Abnormal exit (SIGKILL/segfault; also `panic=abort`) skips `Drop` on
  every platform, and on macOS there is no kernel kill-on-parent-death to backstop it, so a
  warm GUI model can be stranded. A parent-side watchdog cannot help (it dies with the parent),
  and llama-server is not ours to patch; recover with `pkill llama-server`. (These pull in
  `libc`/`windows-sys` as per-OS deps in Cargo.toml.)
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
- GUI-08: streaming token transcript -- SSE parse in `src/server/mod.rs`, `Progress::PartialText`, `ocr://partial-text` event, live append in `main.js`
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
