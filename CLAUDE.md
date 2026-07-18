# unlocr

Rust CLI: OCR PDFs to markdown via the Unlimited-OCR (DeepSeek-OCR) model + llama.cpp.
Thin wrapper. Full usage/benchmarks in README.md.

## Layout
- Cargo workspace lives in repo root. Source: `src/`.
  Modules / File Tree:
  - `lib.rs`: library entry point, exports `model`, `pdf`, `preflight`, `server`, `tools`.
    Also splits the clap-free core into private `options`/`output`/`preview`/`job` mods
    re-exported at the crate root (`OcrOptions`, `OutputMode`, `OcrOutput`, `Progress`,
    `run_ocr_job`, `ocr_pages`, `render_pages`/`render_page`, `resolve_output_path`,
    `write_markdown_output`, `duplicate_stems`); see the Public lib API gotcha below.
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
    - `system.rs`: environment checking (binary locate/permissions) + RAM/CPU/disk/GPU probes.
    - `sysreq.rs`: rates those probes against thresholds into `SystemInfo` for the GUI System Requirements panel.
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
- `llama-server` <- llama.cpp. The Unlimited-OCR model needs the R-SWA patch from the
  UNMERGED draft PR #24975 (branch `sf/unlimited-ocr-rswa`); no upstream/apt/dnf/brew
  build has it. So unlocr builds its OWN patched llama-server in CI
  (`.github/workflows/build-llama.yml`) and auto-downloads it (see the managed-llama
  resolver gotcha below). An external binary is used only as an unverified fallback.
  (The base DeepSeek-OCR arch also needs >= b8530 / PR #17400, kept as a secondary
  warning.) deb postinst / rpm %post still warn if missing.

## Gotchas
- CI `cargo audit` (`.github/workflows/audit.yml`) has no severity/warning filter, so
  ANY new advisory on the dep tree fails it (unmaintained/unsound warnings alone do not,
  only actual vulnerabilities). `.cargo/audit.toml` holds `[advisories].ignore` entries
  for unfixable transitive vulns (currently quick-xml via `tauri -> tauri-utils -> plist`,
  Info.plist bundle metadata, no fixed `plist` release exists). Only add an ignore after
  confirming: no upstream fix (`cargo update -p <crate> --dry-run`), and the vuln path
  isn't attacker-reachable at runtime. Re-check ignores on every `tauri`/dep bump; this is
  a release gate (docs/RELEASE.md) so don't let ignores silently accumulate.
- `cargo tree` defaults to the root package like bare `cargo build`/`cargo test` (see
  below): `cargo tree -i <crate>` misses gui-only deps (e.g. `quick-xml` via tauri).
  Use `cargo tree --workspace -i <crate>` to find true reverse-dependents.
- `src/tools/mod.rs`: on-demand tool downloader. `PINS` is per OS+arch (cfg-selected):
  Windows = pandoc/poppler/llama-server (.zip); macOS = pandoc + llama-server (per-arch;
  poppler has no standalone mac binary, stays on brew); Linux x86_64 = llama-server only
  (poppler stays a deb/rpm/apt/dnf dep); other = none. The llama-server pins point at OUR
  `whit3rabbit/unlocr` `llama-rswa-<date>` release (patched R-SWA build), NOT upstream
  ggml-org. Pins (url+sha256+exe) are version-locked; bump on upgrade. The GitHub release
  API `digest` field gives the sha256 (also for `src/model/mod.rs` DIGESTS). `extract_zip` sets the
  unix exec bit from the zip entry (mac binary won't run otherwise). `preflight::locate`
  also scans `<cache>/tools/` so a downloaded tool resolves for every caller. Needs `zip`.
  The llama-server pins currently carry 64-hex-zero placeholders (`TODO(rswa)`): fill them
  from the first `build-llama.yml` run BEFORE any release (runtime sha256 verify rejects
  placeholders). The `pins_are_well_formed`/`downloadable_matches_pins` tests run under
  `#[cfg(test)]` against a pandoc-only test pin, so they do NOT validate the real per-OS
  pins; `cargo check --workspace` is the only compile gate for those.
- Managed llama-server resolver (`src/preflight.rs`): the R-SWA patch (PR #24975) is
  unmerged, so a build NUMBER can't prove compatibility. `resolve_llama_server` (the RUN
  path, called by `check()`) prefers unlocr's cached managed build under
  `<cache>/tools/llama-server/`, else AUTO-DOWNLOADS it via `ensure_tool` (like the GGUF
  model), else falls back to PATH/brew (`Provenance::External`) with a soft warning.
  `--llama-bin` is always respected but flagged External. Silence the external warning with
  `UNLOCR_ALLOW_EXTERNAL_LLAMA=1`. `find_llama_server` is the NON-downloading variant for
  diagnostics (CLI `doctor`, the GUI status/preflight command in `cmd_model/cache.rs`) so a
  passive status check never triggers the download. `check()` now takes an `on_progress`
  sink (all callers updated: `main.rs`, `job.rs`, GUI `load_model`). The hard gate is still
  the real model load in `server::local::await_health`.
- PIN-bump / re-verify checklist for the patched llama-server (a release gate; the draft PR
  moves): (1) pick the latest good commit SHA on `sf/unlimited-ocr-rswa`; (2) run
  `build-llama.yml` (workflow_dispatch) with that SHA + a fresh `llama-rswa-<date>` tag;
  (3) copy the four printed sha256s + asset URLs into the PINS blocks in `src/tools/mod.rs`;
  (4) smoke-test each platform (managed build downloads, loads Unlimited-OCR, produces
  output, does NOT die in `await_health`); (5) `cargo check --workspace`, then release.
  Irreversible: once users cache a `llama-rswa-*` asset, changing its bytes under the same
  tag breaks sha256 verify. Always cut a NEW dated tag for a rebuild; never mutate a
  published asset.
- OS detection is compile-time `cfg!(target_os)` everywhere (per-platform builds), never
  runtime. Tests asserting OS-gating put the `cfg!` check INSIDE the test body (runs
  per-host on CI), not `#[cfg]` on the fn, so each OS verifies its own branch.
- `cargo clippy --workspace --all-targets -- -D warnings` is GREEN; the old
  pre-existing debt was cleared. It is a real release gate (docs/RELEASE.md), so
  keep it green: your diff must add no new lints.
- PDF password probes (`src/pdf.rs`): `select_password`/`can_open`/`needs_user_password`
  are the fallback-aware openers (bare-name pdftoppm with no sibling `pdfinfo` -> probe by
  rendering page 1). Any "can this PDF open / does it need a password" check MUST route
  through these, NOT `pdf::info` directly: `info` has no fallback, so it Errs on bare-name
  pdftoppm and misclassifies EVERY PDF as "needs password". Poppler gets the password as
  `-upw` argv (visible in `ps`) regardless of source (`--password`/env/`--password-file`);
  only shell history + unlocr's own argv are protected, so don't claim more. Encrypted-PDF
  tests need `qpdf` (poppler can't create one) and skip when it/pdftoppm/pdfinfo are absent.
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
  (`read_text_file` is `.md`-only) cannot render it. GUI-ONLY EXCEPTION: for a default
  (no `out_file`) path the GUI versions the stem via `next_free_stem` (worker.rs) so
  re-OCR'ing the same PDF writes `foo.md`, `foo-2.md`, ... (or `foo/`, `foo-2/` in pages
  mode) instead of overwriting. The CLI and any explicit `-o` still overwrite. Each GUI
  run now points at its OWN file, so deleting one run's `.md` no longer strands the others
  (jobs schema is v5: `page_count`/`duration_ms`/`backend`/`output_mode` recorded per run).
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
- Distinct model repos (GGUF / remote-GPU / MLX), do not conflate them:
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
  - THIRD source (MLX, `--mlx`): `sahilchachra/unlimited-ocr-*-mlx` quants, served +
    cached by mlxcel in its OWN `~/.cache/mlxcel`, not our cache. `DEFAULT_MODEL` /
    `recommend_model` live in `src/server/mlx.rs`.
- MLX backend (`--mlx`, Apple Silicon only): `src/server/mlx.rs` spawns lablup/mlxcel
  `mlxcel-server` (own `ToolPin`, auto-downloaded). mlxcel resolves + downloads the HF
  model ITSELF at startup (unlocr never sees the files, no managed GGUF download), so it
  uses `MLX_HEALTH_TIMEOUT` (30 min) NOT the 180s `HEALTH_TIMEOUT` -- the first-run
  multi-GB download happens inside the health window. The anti-loop sampling defaults
  (repeat_penalty 1.3 / dry_multiplier 1.0) in `main.rs::run` are scoped to the
  llama-server GGUF path only; `run_mlx` returns BEFORE them and mlxcel is not known to
  accept llama.cpp DRY fields, so do NOT inject them blind. `--mlx-model` is
  `requires = "mlx"`; `--mlx` vs `--endpoint/--gpu/--model/--mmproj` rejected in `main.rs`.
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
  BUT local llama-server accepts llama.cpp-native sampling fields in the chat-
  completions body: `--dry-multiplier` (default 1.0 on local, + hardcoded
  `dry_allowed_length: 4`) is the DRY-sampler analog of upstream's no-repeat-ngram
  logits processor and is the loop-killer; `--repeat-penalty` defaults to 1.3 on
  local GGUF (bumped from 1.15; still "not always enough" per upstream repetition-
  loop reports, see the README "Repetition loops" limitation bullet). The model
  also natively emits `label [x, y, x, y]` layout annotations
  (coords 0-999) with EVERY prompt; upstream cleans them in Python, we port that as
  `strip_layout_annotations`/`AnnotationStripper` (src/output.rs), applied in
  `ocr_pages` to final text AND the PartialText stream unless the prompt contains
  `<|grounding|>` (the opt-out marker carried by the grounding task preset).
- Numeric knobs need explicit range guards: clap does not bounds-check the CLI, and
  a direct `invoke()` bypasses the GUI's HTML `min=` form clamp. The single shared
  sink is `OcrOptions::validate()` in `src/options.rs` (rejects dpi==0 /
  max_tokens==0 / image_max_tokens==Some(0) / non-finite or <=0 repeat_penalty /
  page first==0 or last<first). CLI `main.rs::run` and GUI
  `cmd_run/ocr/validation.rs` BOTH call it, so add a new numeric knob's guard THERE,
  not in two duplicated front-end checks. (`image_max_tokens` is a load-time flag, so
  GUI `load_model` also guards it before `Server::start`.) `model::require_file` is the
  analogous single sink for `--model`/`--mmproj`.
- A per-request body knob must be added to BOTH `ocr_via` and `ocr_via_stream`
  (stream + non-stream paths). Route it through a shared helper (`apply_sampling`).
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
- `src/preflight/system.rs` parses Linux `/proc/cpuinfo` with the literal prefix
  `"core id\t\t: "` (TWO tabs) and `"physical id\t: "` (one tab). This is CORRECT, not
  a bug: the x86 kernel (`arch/x86/kernel/cpu/proc.c`) prints
  `seq_printf("core id\t\t: %d\n", ...)` with two tabs to align the colon. Do not
  "fix" it to one tab (twice flagged in review, twice refuted against the kernel
  source). ARM `/proc/cpuinfo` has no `core id`/`physical id` fields, so the fn
  returns `None` there by design.
- `Progress::Page { page, total }`: `total` is the number of pages processed THIS run
  (the rendered `--pages` subset size), NOT the document's page count. A ranged run
  (`--pages 5-15`) emits `total` = 11; the GUI progress bar reflects the run span,
  not the whole PDF. The field doc says so explicitly now.
- `preflight::sysreq` rates the `system.rs` probes into a `SystemInfo` the GUI renders.
  The static probes (RAM/CPU/model/GPU) are memoized per-process via a `OnceLock`
  (`detect_gpu` shells `system_profiler` ~1-2s on macOS / `lspci`+`nvidia-smi` on
  Linux); only disk free is re-probed per call (it changes as models download).
- Documentation Mirroring: Any changes or updates made to `README.md` (English) must be accurately translated and mirrored in `README_zh.md` (Chinese) to keep both versions in sync.

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
