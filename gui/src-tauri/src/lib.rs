// Tauri backend for the unlocr desktop GUI.
//
// Thin bridge over the repo-root `unlocr` crate (linked via the path dep in
// Cargo.toml). All OCR behavior lives in `unlocr`; this crate only:
//   - exposes typed commands the frontend can invoke,
//   - maps frontend params into `unlocr::OcrOptions`,
//   - forwards `unlocr::Progress` to the webview as Tauri events.
// No pipeline logic is forked in here (see gui/CLAUDE.md "thin shim").

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
// ocr_pages is clap-free and takes a progress sink + any ImageOcr backend, so the
// GUI can drive OCR against a held local server or a remote endpoint.
// ImageOcr is the trait bound on ocr_pages; the bound is satisfied by passing
// Server/RemoteEndpoint, so the trait need not be imported by name here.
use unlocr::server::{RemoteEndpoint, Server};
use unlocr::{OcrOptions, Progress};

// Persisted job store (EH-0006 bite 1). Purely additive: a new module exposing
// `list_jobs` / `record_job` commands. No existing command changes.
mod store;
// Persisted GUI settings (provider mode + engine defaults), same JSON-under-cache
// pattern as store.rs.
mod settings;
// Re-export the wire types so the command signatures read cleanly and the frontend
// contract (camelCase Job/JobOptions) is documented in one place.
pub use store::{Job, JobOptions};

/// A loaded inference backend: a long-lived local llama-server (held so its model
/// stays warm in RAM until unloaded) or a remote OpenAI-compatible endpoint.
/// `Backend` is the litellm-style "loaded model" the Run gate checks for.
enum Backend {
    Local(Server),
    Remote(RemoteEndpoint),
}

/// The currently loaded model plus a human label for the status badge.
struct LoadedModel {
    backend: Backend,
    /// "Unlimited-OCR Q8_0" for local, the base URL for remote.
    label: String,
    /// "local" | "remote", echoed to the frontend for the badge.
    mode: String,
}

/// App-wide state managed by Tauri. `model` is None until Load succeeds (Run is
/// gated on it); dropping the `Server` inside it kills llama-server and frees RAM.
/// `pdftoppm` is resolved at load time because rasterization is always local, even
/// when inference is remote.
#[derive(Default)]
struct AppState {
    // ponytail: one Mutex held across a whole run serializes runs. Fine for a
    // single-user desktop app; split into a server pool if concurrent runs matter.
    model: Mutex<Option<LoadedModel>>,
    pdftoppm: Mutex<Option<PathBuf>>,
}

/// Status payload for `model_status` / `load_model` returns. Drives the titlebar
/// Local/Remote badge and the Run OCR enable/disable gate.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelStatus {
    loaded: bool,
    /// "local" | "remote" | "" when nothing is loaded.
    mode: String,
    /// Display label ("Unlimited-OCR Q8_0" / the remote URL), empty when unloaded.
    label: String,
}

/// Serializable payload for the `ocr://page` event. The frontend listens for
/// this to render per-page progress. Kept flat + camelCase so the JS side reads
/// `event.payload.page` / `.total` directly.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PageProgress {
    page: usize,
    total: usize,
}

/// Payload for the `ocr://progress` event (model/projector download). `pct` is
/// 0..=100. The frontend uses this for an indeterminate->determinate bar switch.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    name: String,
    pct: u8,
}

/// Payload for the `ocr://server-ready` event. Emitted once llama-server is
/// healthy, before per-page OCR begins. The frontend can use `port` for a
/// "server up on :port" status line.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerReady {
    port: u16,
}

/// Payload for the terminal `ocr://done` event. `markdown` is the assembled
/// output for one input; emitted per input (batch callers get one per file).
/// Also returned from the command directly so a simple await() caller works
/// without subscribing to events.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OcrDone {
    markdown: String,
}

/// Payload for `ocr://images-kept`. Emitted per input only when `keep_images`
/// was set, carrying the directory the page PNGs were left in. Without this the
/// kept images are orphaned in a temp dir the user can never locate.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImagesKept {
    dir: String,
}

/// Structured preflight result the frontend can render as a status panel, not a
/// blob of text. `camelCase` so JS reads `report.llamaServer`, `.buildNumber`,
/// `.modelPresent`, etc. directly. Mirrors the board's EH-0003 bite 1 spec:
/// resolved paths + build number + model-present booleans.
///
/// `ok = false` (with `error` set) means a required tool is missing; the
/// resolved paths and model flags are still best-effort populated so the UI can
/// show which item failed. `ok = true` means the environment is runnable.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreflightReport {
    ok: bool,
    /// None when preflight::check failed before resolving a path, or when the
    /// build number could not be parsed (older/unknown llama-server builds).
    build_number: Option<u64>,
    llama_server: Option<String>,
    pdftoppm: Option<String>,
    /// Model GGUF for the requested quant present in the cache.
    model_present: bool,
    /// Projector (mmproj) GGUF present in the cache.
    mmproj_present: bool,
    /// Quant the presence check ran against (echoed so the UI can label it).
    quant: String,
    /// Set only when `ok = false`. Either an install hint (tool missing) or a
    /// cache-dir resolution error, already stringified for display.
    error: Option<String>,
}

/// Read a UTF-8 text file off disk into a String. Used by the frontend to fetch
/// the `{stem}.md` written by `run_ocr` (the path is returned from that command)
/// so the result can be rendered in a dedicated read-only markdown pane (see
/// EH-0004 bite 2). Kept a thin, additive FS shim: no globbing; the frontend
/// passes the exact path the OCR command returned along with the output directory
/// used for the run so the backend can enforce an allowlist (EH-0005 bite 3).
/// Errors are stringified so the future stays Send and the UI can surface them inline.
///
/// `allowed_dir` is the directory that `run_ocr` wrote output into (i.e. the
/// parent directory of the PDF). When supplied, the canonicalized file path MUST
/// start with the canonicalized allowed dir; any path outside that directory is
/// rejected even if it passes the extension check. This is an allowlist, not a
/// denylist: it confines reads to the known output location at runtime.
#[tauri::command]
fn read_text_file(path: String, allowed_dir: Option<String>) -> Result<String, String> {
    // Harden against path-traversal and symlink attacks (EH-0005 bites 2+3).
    //
    // Threat: the webview (or a compromised script inside it) passes a crafted
    // path like `../../etc/passwd` or a symlink whose target is outside the
    // expected output location. Defenses in order:
    //
    // 1. Extension pre-check on the raw string (fast, catches the common case).
    // 2. `canonicalize` resolves `..` and follows all symlinks; the *resolved*
    //    path is then re-checked for the `.md` extension (catches `foo.md ->
    //    /etc/shadow` symlinks).
    //    Canonicalize also rejects non-existent paths, so a speculative probe
    //    against a path that does not yet exist fails closed.
    // 3. Allowlist: if `allowed_dir` is provided (always supplied by the frontend
    //    during a real run), the canonical file path must start with the canonical
    //    allowed_dir. This confines reads to the known output directory and
    //    eliminates the residual risk the old denylist left open (any .md file
    //    outside system dirs, e.g. /tmp/x/secret.md, was previously readable).

    // Step 1: raw extension check before touching the filesystem.
    if Path::new(&path).extension().and_then(|e| e.to_str()) != Some("md") {
        return Err(format!("refusing to read non-markdown path: {path}"));
    }

    // Step 2: canonicalize (resolves `..`, symlinks, and relative segments).
    // Fails if the file does not exist — intentional: we never read a speculative
    // path, only one that run_ocr already wrote.
    let canonical = std::fs::canonicalize(&path)
        .map_err(|e| format!("cannot resolve path {}: {e}", path))?;

    // Step 3: re-check extension on the canonical target (symlink defense).
    if canonical.extension().and_then(|e| e.to_str()) != Some("md") {
        return Err(format!(
            "refusing to read non-markdown path after resolution: {}",
            canonical.display()
        ));
    }

    // Step 4: allowlist — the canonical file path must start with the canonical
    // allowed_dir. Reject the read when allowed_dir is provided but cannot be
    // resolved (e.g. the dir was deleted between run_ocr writing the file and
    // read_text_file being called); fail closed rather than silently widening scope.
    if let Some(dir) = allowed_dir {
        if dir.is_empty() {
            return Err("allowed_dir is required but was empty".to_string());
        }
        let canonical_dir = std::fs::canonicalize(&dir)
            .map_err(|e| format!("cannot resolve allowed_dir {dir}: {e}"))?;
        // Use starts_with on PathBuf components so "/tmp/foo" does not match
        // "/tmp/foobar" (component boundary check, not a string prefix).
        if !canonical.starts_with(&canonical_dir) {
            return Err(format!(
                "refusing to read path outside allowed output dir {}: {}",
                canonical_dir.display(),
                canonical.display()
            ));
        }
    }

    std::fs::read_to_string(&canonical).map_err(|e| format!("failed to read {}: {e}", canonical.display()))
}

/// Rasterize a PDF to per-page PNGs for the preview pane, cached on disk by the
/// core lib so a repeat preview skips pdftoppm. Returns absolute PNG paths; the
/// frontend wraps each with `convertFileSrc` to load it through the asset
/// protocol (the previews dir is allow-listed in the asset scope at startup,
/// see `run()`). Runs on spawn_blocking (pdftoppm shell-out) so the webview never
/// freezes; `unlocr`'s `Box<dyn Error>` is stringified inside the closure so the
/// future stays Send.
#[tauri::command]
async fn render_pages(pdf_path: String, dpi: Option<u32>) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<String>, String> {
        let dpi = dpi.unwrap_or_else(|| OcrOptions::default().dpi);
        let cache = unlocr::model::cache_dir(None).map_err(|e| e.to_string())?;
        // Preview only needs pdftoppm. Resolve it directly instead of
        // preflight::check, which also requires llama-server and forks
        // `llama-server --version` on every call: a preview must work on a
        // poppler-only machine and must not spawn an unrelated process per render.
        let pdftoppm = unlocr::preflight::pdftoppm().map_err(|e| e.to_string())?;
        let pages = unlocr::render_pages(&pdftoppm, Path::new(&pdf_path), dpi, &cache)
            .map_err(|e| e.to_string())?;
        Ok(pages
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect())
    })
    .await
    .map_err(|e| format!("render worker join failed: {e}"))?
}

// --- job store commands (EH-0006 bite 1) -----------------------------------
//
// The store itself lives in `store.rs`; these thin commands are the frontend's
// only access. Two are required for this bite: `list_jobs` (Library/Board reads)
// and `record_job` (write a run's outcome after run_ocr returns/throws). A third,
// `jobs_store_path`, exposes the on-disk path so an acceptance check can `cat`
// the file and confirm one record per run. All three are additive; run_ocr is
// unchanged (the frontend decides when to record, keeping OCR and persistence
// decoupled so an OCR success is never rolled back by a store write failure).

/// Return every persisted job in insertion order. The frontend renders this into
/// the Library grid (all jobs) and the Board (grouped by `status`). An empty vec
/// on a first launch or a missing/corrupt store; never throws.
#[tauri::command]
fn list_jobs() -> Vec<Job> {
    store::load_jobs()
}

/// Absolute path of the `jobs.json` store under the model cache dir, as a string.
/// Surfaces the cache-dir resolution error (if any) so the UI/acceptance can tell
/// "no jobs yet" apart from "could not even locate the store". Used by the card's
/// "cat the file path and show record count" acceptance check.
#[tauri::command]
fn jobs_store_path() -> Result<String, String> {
    store::store_path().map(|p| p.display().to_string())
}

/// Record one run's outcome to the store. The frontend calls this right after a
/// `run_ocr` invocation completes (status="done", output_path set) or fails
/// (status="failed", error set). Options are echoed as the same-shaped struct the
/// `run_ocr` command received, so the stored record reflects what the run used.
///
/// Returns the stored `Job` (with its generated id) so the caller can append it to
/// an in-memory list without a full reload. A store write failure is surfaced as
/// Err rather than swallowed, but the OCR result it accompanies has already been
/// delivered to the user, so this never rolls back a successful run.
#[tauri::command]
fn record_job(
    input_path: String,
    quant: Option<String>,
    max_tokens: Option<u32>,
    dpi: Option<u32>,
    prompt: Option<String>,
    keep_images: Option<bool>,
    status: Option<String>,
    output_path: Option<String>,
    error: Option<String>,
) -> Result<Job, String> {
    // Defaults mirror OcrOptions::default() so a record with no options sent
    // matches a no-args run (the same convention run_ocr uses).
    let def = OcrOptions::default();
    let options = JobOptions::from_opts(
        quant.as_deref().unwrap_or(&def.quant),
        max_tokens.unwrap_or(def.max_tokens),
        dpi.unwrap_or(def.dpi),
        prompt.as_deref().unwrap_or(&def.prompt),
        keep_images.unwrap_or(def.keep_images),
    );
    // Validate status against the known set the Board buckets on. An unknown value
    // would render unstyled and be bucketed into "queued", hiding a finished run
    // from the Done column. Reject it: the frontend always sends a known value, so
    // an unknown is a bug, and recordRunOutcome swallows the Err (best-effort).
    let status = status.as_deref().unwrap_or("done");
    if !matches!(status, "queued" | "running" | "done" | "failed") {
        return Err(format!(
            "invalid status {status:?}: expected one of queued|running|done|failed"
        ));
    }
    store::record_outcome(
        &input_path,
        options,
        status,
        output_path.as_deref().unwrap_or(""),
        error.as_deref().unwrap_or(""),
    )
}

/// Validate the runtime environment before a run: locate llama-server + pdftoppm,
/// read llama-server's build number, and check the model/projector GGUFs for the
/// given quant are present in the cache. Returns a structured report the frontend
/// renders; never throws (Ok is always a report, Err is reserved for cache-dir
/// failures that prevent even the presence check).
///
/// `llama_bin` optionally overrides the llama-server lookup (file-picker path).
/// `quant` optionally selects which model file's presence to report; empty/None
/// falls back to the lib default ("Q8_0") so the report matches a no-args run.
#[tauri::command]
fn preflight(
    llama_bin: Option<String>,
    quant: Option<String>,
) -> Result<PreflightReport, String> {
    let llama_override = llama_bin.map(PathBuf::from);
    let quant = quant
        .map(|q| if q.trim().is_empty() { OcrOptions::default().quant } else { q })
        .unwrap_or_else(|| OcrOptions::default().quant);

    // Validate the webview-supplied quant before it reaches check_presence:
    // PathBuf::join does not normalize, so a traversing quant would become an
    // is_file() probe outside the cache dir (a file-existence oracle exposed to
    // the frontend). Surface it as an ok:false report, same shape as below.
    if let Err(e) = unlocr::model::validate_quant(&quant) {
        return Ok(PreflightReport {
            ok: false,
            build_number: None,
            llama_server: None,
            pdftoppm: None,
            model_present: false,
            mmproj_present: false,
            quant,
            error: Some(e.to_string()),
        });
    }

    // Resolve the cache dir up front. On failure we still return a report so the
    // UI can show the error inline rather than surfacing a thrown string.
    let cache = match unlocr::model::cache_dir(None) {
        Ok(c) => c,
        Err(e) => {
            return Ok(PreflightReport {
                ok: false,
                build_number: None,
                llama_server: None,
                pdftoppm: None,
                model_present: false,
                mmproj_present: false,
                quant,
                error: Some(format!("could not resolve model cache dir: {e}")),
            });
        }
    };

    // Tools check. preflight::check returns Box<dyn Error> (not Send); we are in
    // a sync command so this is fine, and we map the error into the report so a
    // missing tool becomes a user-facing status, not an exception.
    let tools = match unlocr::preflight::check(llama_override.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            // Still report model presence so the UI can show "install llama-server,
            // your model is already downloaded" rather than just the error.
            let (_, model_present, _, mmproj_present) = unlocr::model::check_presence(&cache, &quant);
            return Ok(PreflightReport {
                ok: false,
                build_number: None,
                llama_server: llama_override.map(|p| p.display().to_string()),
                pdftoppm: None,
                model_present,
                mmproj_present,
                quant,
                error: Some(e.to_string()),
            });
        }
    };

    // Build number is best-effort: a None here just means "unknown", not failure.
    // build_number shells out to `llama-server --version`; safe to call on the
    // sync command thread.
    let build_number = unlocr::preflight::build_number(&tools.llama_server);

    let (_, model_present, _, mmproj_present) = unlocr::model::check_presence(&cache, &quant);

    Ok(PreflightReport {
        ok: true,
        build_number,
        llama_server: Some(tools.llama_server.display().to_string()),
        pdftoppm: Some(tools.pdftoppm.display().to_string()),
        model_present,
        mmproj_present,
        quant,
        error: None,
    })
}

/// Load a model so it stays warm in RAM (litellm-style), gating Run OCR until it
/// succeeds. Long-running for local (model download + llama-server health wait),
/// so it runs on spawn_blocking. Emits `ocr://progress` (download pct) and
/// `ocr://server-ready` (local only) so the UI can show load progress.
///
/// `mode`     "local" (spawn+hold a llama-server) or "remote" (hold an endpoint).
/// `quant`    local only: which GGUF to load (defaults to the lib default).
/// `base_url` remote only: OpenAI-compatible base URL.
/// `api_key`  remote only: optional bearer token.
/// `llama_bin` optional llama-server override (local only).
///
/// Replaces any currently-loaded model, dropping the old `Server` first so its
/// RAM is freed before the new one is started.
#[tauri::command]
async fn load_model(
    app: AppHandle,
    mode: String,
    quant: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    llama_bin: Option<String>,
) -> Result<ModelStatus, String> {
    let llama_override = llama_bin.filter(|s| !s.trim().is_empty()).map(PathBuf::from);

    tauri::async_runtime::spawn_blocking(move || -> Result<ModelStatus, String> {
        // Free any already-loaded model BEFORE starting a new one so two models
        // never sit in RAM at once (the whole point of explicit load/unload).
        {
            let state = app.state::<AppState>();
            // Bind the guard to a local (not an if-let tail) so it drops before
            // `state` at block end. Recover from a poisoned lock rather than skip.
            let mut g = state.model.lock().unwrap_or_else(|p| p.into_inner());
            *g = None; // drops old Server -> kills old llama-server
        }

        // pdftoppm is always needed (rasterization is local even for remote
        // inference). Each branch resolves its own tools: remote needs ONLY
        // pdftoppm, local needs llama-server too. Resolving both for remote would
        // wrongly block remote on a machine without llama.cpp.
        let (backend, label, mode_out, pdftoppm) = match mode.as_str() {
            "remote" => {
                let base = base_url.unwrap_or_default().trim().to_string();
                if base.is_empty() {
                    return Err("remote mode requires a base URL".into());
                }
                let pdftoppm = unlocr::preflight::pdftoppm().map_err(|e| e.to_string())?;
                let ep = RemoteEndpoint {
                    base_url: base.clone(),
                    api_key: api_key.filter(|k| !k.trim().is_empty()),
                };
                // warn-not-fail: some OpenAI-compatible servers omit /v1/models.
                if let Err(e) = ep.probe() {
                    eprintln!("[load_model] remote probe failed (continuing): {e}");
                }
                (Backend::Remote(ep), base, "remote".to_string(), pdftoppm)
            }
            _ => {
                let tools = unlocr::preflight::check(llama_override.as_deref())
                    .map_err(|e| e.to_string())?;
                let quant = quant
                    .filter(|q| !q.trim().is_empty())
                    .unwrap_or_else(|| OcrOptions::default().quant);
                unlocr::model::validate_quant(&quant).map_err(|e| e.to_string())?;
                let cache = unlocr::model::cache_dir(None).map_err(|e| e.to_string())?;
                let app_dl = app.clone();
                let mut on_progress = |p: Progress| {
                    if let Progress::Download { name, pct } = p {
                        let _ = app_dl.emit("ocr://progress", DownloadProgress { name, pct });
                    }
                };
                let files = unlocr::model::ensure_with_progress(&cache, &quant, &mut on_progress)
                    .map_err(|e| e.to_string())?;
                let port = unlocr::server::free_port().map_err(|e| e.to_string())?;
                let srv = Server::start(&tools.llama_server, &files.model, &files.mmproj, port)
                    .map_err(|e| e.to_string())?;
                let _ = app.emit("ocr://server-ready", ServerReady { port });
                (
                    Backend::Local(srv),
                    format!("Unlimited-OCR {quant}"),
                    "local".to_string(),
                    tools.pdftoppm,
                )
            }
        };

        // Install the new model + the resolved pdftoppm into managed state.
        let state = app.state::<AppState>();
        if let Ok(mut g) = state.model.lock() {
            *g = Some(LoadedModel {
                backend,
                label: label.clone(),
                mode: mode_out.clone(),
            });
        }
        if let Ok(mut p) = state.pdftoppm.lock() {
            *p = Some(pdftoppm);
        }
        Ok(ModelStatus {
            loaded: true,
            mode: mode_out,
            label,
        })
    })
    .await
    .map_err(|e| format!("load worker join failed: {e}"))?
}

/// Unload the current model: drop the held `Server` (kills llama-server, frees
/// RAM) or forget the remote endpoint. Run OCR is gated off afterward.
#[tauri::command]
fn unload_model(state: State<'_, AppState>) -> ModelStatus {
    if let Ok(mut g) = state.model.lock() {
        *g = None;
    }
    ModelStatus {
        loaded: false,
        mode: String::new(),
        label: String::new(),
    }
}

/// Current load state for the titlebar badge + the Run OCR gate.
#[tauri::command]
fn model_status(state: State<'_, AppState>) -> ModelStatus {
    match state
        .model
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|lm| (lm.mode.clone(), lm.label.clone())))
    {
        Some((mode, label)) => ModelStatus {
            loaded: true,
            mode,
            label,
        },
        None => ModelStatus {
            loaded: false,
            mode: String::new(),
            label: String::new(),
        },
    }
}

/// Quant tags already downloaded to the model cache, for the model picker.
#[tauri::command]
fn list_local_models() -> Vec<String> {
    match unlocr::model::cache_dir(None) {
        Ok(cache) => unlocr::model::list_cached_quants(&cache),
        Err(_) => Vec::new(),
    }
}

/// Read persisted GUI settings (provider mode + engine defaults), falling back to
/// defaults on a missing/corrupt file.
#[tauri::command]
fn get_settings() -> settings::Settings {
    settings::load_settings()
}

/// Persist GUI settings. Param is `newSettings` on the wire (camelCase) to avoid
/// shadowing the `settings` module inside the body.
#[tauri::command]
fn save_settings(new_settings: settings::Settings) -> Result<(), String> {
    settings::save_settings(&new_settings)
}

/// Run OCR on one or more PDFs using the already-loaded model. Emits:
///   - `ocr://page`         per page rasterized+OCR'd (1-based page, total)
///   - `ocr://done`         terminal, per input, payload carries the markdown
///   - `ocr://images-kept`  per input when `keep_images` was set
/// Download + server-ready now happen in `load_model`; this command requires a
/// loaded model (else `Err("load a model first")`) and reuses it via `ocr_pages`.
/// Long-running (per-page inference), so it runs on spawn_blocking.
///
/// `inputs`  absolute or relative PDF paths (batch supported).
/// `out_dir` directory the assembled markdown is written to (one `.md` per input,
///           named after the stem). Empty string = in-memory only.
/// Remaining params map onto the per-run `OcrOptions` fields (quant is fixed at
/// load time and not accepted here).
#[tauri::command]
async fn run_ocr(
    app: AppHandle,
    inputs: Vec<String>,
    out_dir: String,
    max_tokens: Option<u32>,
    dpi: Option<u32>,
    prompt: Option<String>,
    keep_images: Option<bool>,
) -> Result<Vec<String>, String> {
    // Per-run options from defaults + the fields the frontend sent. `quant`/`port`
    // are irrelevant here (the model is already loaded/held).
    let mut opts = OcrOptions::default();
    if let Some(t) = max_tokens {
        opts.max_tokens = t;
    }
    if let Some(d) = dpi {
        opts.dpi = d;
    }
    if let Some(p) = prompt {
        opts.prompt = p;
    }
    if let Some(k) = keep_images {
        opts.keep_images = k;
    }

    // Reject out-of-range numerics before they reach pdftoppm/inference. The JS
    // form guards v > 0, but a direct invoke bypasses it: dpi=0 makes pdftoppm
    // "produce no pages" and max_tokens=0 yields silently-empty page content.
    if opts.dpi == 0 {
        return Err("dpi must be greater than 0".to_string());
    }
    if opts.max_tokens == 0 {
        return Err("max_tokens must be greater than 0".to_string());
    }

    #[cfg(debug_assertions)]
    eprintln!(
        "[run_ocr] effective opts: dpi={} max_tokens={} keep_images={}",
        opts.dpi, opts.max_tokens, opts.keep_images
    );

    let out_dir = PathBuf::from(out_dir);

    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<String>, String> {
        let state = app.state::<AppState>();

        // pdftoppm was resolved at load time (rasterization is always local).
        let pdftoppm = state
            .pdftoppm
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .ok_or("load a model first")?;

        // Hold the model lock for the whole batch: the local Server cannot be
        // cloned, and serializing runs is acceptable for a single-user app.
        let guard = state.model.lock().map_err(|_| "model state poisoned")?;
        let lm = guard.as_ref().ok_or("load a model first")?;

        let mut results = Vec::with_capacity(inputs.len());
        for input in &inputs {
            let input_path = PathBuf::from(input);

            // Per-input progress sink: ocr_pages emits only Page events; forward
            // them. Best-effort emit (a failed IPC must never abort the run).
            let app_for_progress = app.clone();
            let mut on_progress = |p: Progress| {
                if let Progress::Page { page, total } = p {
                    let _ = app_for_progress.emit("ocr://page", PageProgress { page, total });
                }
            };

            // Dispatch to the held backend. ocr_pages is generic over ImageOcr so
            // both the local Server and the RemoteEndpoint drive the same loop.
            let outcome = match &lm.backend {
                Backend::Local(srv) => {
                    unlocr::ocr_pages(srv, &pdftoppm, &input_path, &opts, &mut on_progress)
                }
                Backend::Remote(ep) => {
                    unlocr::ocr_pages(ep, &pdftoppm, &input_path, &opts, &mut on_progress)
                }
            };
            let (md, kept) = outcome.map_err(|e| e.to_string())?;

            if let Some(dir) = kept {
                let _ = app.emit(
                    "ocr://images-kept",
                    ImagesKept { dir: dir.display().to_string() },
                );
            }
            let _ = app.emit("ocr://done", OcrDone { markdown: md.clone() });

            if out_dir.as_os_str().is_empty() {
                results.push(md);
            } else {
                let stem = input_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("output");
                // Create the output dir first, matching the CLI (main.rs creates
                // args.out before writing). Without this a non-existent out_dir
                // fails the write with NotFound instead of being created.
                if let Err(e) = std::fs::create_dir_all(&out_dir) {
                    return Err(format!("failed to create {}: {e}", out_dir.display()));
                }
                let out_file = out_dir.join(format!("{stem}.md"));
                if let Err(e) = std::fs::write(&out_file, &md) {
                    return Err(format!("failed to write {}: {e}", out_file.display()));
                }
                results.push(out_file.display().to_string());
            }
        }
        Ok(results)
    })
    .await
    .map_err(|e| format!("ocr worker join failed: {e}"))?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Allow the asset protocol to read cached preview PNGs. The cache dir
            // is resolved per-OS at runtime, so the scope is extended here rather
            // than hardcoded in tauri.conf.json. Best-effort: a failure just means
            // previews fail to load (logged), not an app crash.
            if let Ok(cache) = unlocr::model::cache_dir(None) {
                let previews = cache.join("previews");
                let _ = std::fs::create_dir_all(&previews);
                if let Err(e) = app.asset_protocol_scope().allow_directory(&previews, true) {
                    eprintln!("[setup] asset scope allow_directory failed: {e}");
                }
            }
            Ok(())
        })
        // Managed state holding the warm model + resolved pdftoppm across commands.
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            preflight,
            run_ocr,
            render_pages,
            read_text_file,
            list_jobs,
            jobs_store_path,
            record_job,
            load_model,
            unload_model,
            model_status,
            list_local_models,
            get_settings,
            save_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
