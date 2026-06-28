// OCR run + file IO commands: render_pages (preview rasterization), read_text_file
// (fetch the written .md for the review pane), and run_ocr (the batch OCR loop).
// These drive the held backend in `AppState` (state.rs).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use unlocr::{OcrOptions, Progress};

use crate::state::{AppState, Backend};

/// Serializable payload for the `ocr://page` event. The frontend listens for
/// this to render per-page progress. Kept flat + camelCase so the JS side reads
/// `event.payload.page` / `.total` directly.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PageProgress {
    page: usize,
    total: usize,
}

/// Payload for the `ocr://partial-text` event: one streamed token chunk during
/// OCR of a page (`page` is 1-based). The frontend appends `chunk` to the live
/// transcript / run-popup log so the user sees output as it arrives.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PartialText {
    page: usize,
    chunk: String,
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

/// Read a UTF-8 text file off disk into a String. Used by the frontend to fetch
/// the `{stem}.md` written by `run_ocr` (the path is returned from that command)
/// so the result can be rendered in a dedicated read-only markdown pane (see
/// EH-0004 bite 2). Kept a thin, additive FS shim: no globbing.
///
/// Read scope is BACKEND-DERIVED, not renderer-supplied: the only files served are
/// ones the app itself produced. The allowlist is the union of the output paths
/// this session's runs wrote (`AppState::read_allow`) and the non-empty
/// `output_path`s recorded in the job store (so re-opening a past run from Library
/// works across restarts). The requested path must, after canonicalization, EXACTLY
/// equal one of those. This is strictly tighter than the old caller-supplied
/// `allowed_dir`, which let a compromised webview read any `.md` anywhere by
/// pointing `allowed_dir` at the target's parent.
#[tauri::command]
pub(crate) fn read_text_file(path: String, state: State<'_, AppState>) -> Result<String, String> {
    let allowed = allowed_output_paths(&state);
    let canonical = check_readable(&path, &allowed)?;
    std::fs::read_to_string(&canonical)
        .map_err(|e| format!("failed to read {}: {e}", canonical.display()))
}

/// Overwrite a `.md` the review-pane editor is editing. Write scope is the SAME
/// backend-derived allowlist as `read_text_file`: the renderer may only overwrite a
/// file the app itself produced (this session's runs or a job-store `output_path`),
/// never a path it chooses. Overwrite-only -> the target always pre-exists, so the read
/// guard (`check_readable`: `.md` ext + canonicalize + exact allowlist match) applies
/// verbatim; there is no new arg the renderer could use to widen scope.
#[tauri::command]
pub(crate) fn write_text_file(
    path: String,
    content: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let allowed = allowed_output_paths(&state);
    let canonical = check_readable(&path, &allowed)?;
    std::fs::write(&canonical, content)
        .map_err(|e| format!("failed to write {}: {e}", canonical.display()))
}

/// Map a frontend export format to the pandoc writer name and output file extension.
/// Restricting to this fixed set (not an arbitrary string) keeps the renderer from
/// passing pandoc an unexpected writer. `txt` uses pandoc's `plain` writer (strips
/// markdown syntax); the others share their name with the extension.
fn pandoc_target(format: &str) -> Option<(&'static str, &'static str)> {
    match format {
        "html" => Some(("html", "html")),
        "txt" => Some(("plain", "txt")),
        "docx" => Some(("docx", "docx")),
        "odt" => Some(("odt", "odt")),
        "rtf" => Some(("rtf", "rtf")),
        _ => None,
    }
}

/// Export the loaded review-pane markdown to another document format via pandoc
/// (docx / odt / rtf / html / plain-txt). `src_path` must be an app-produced `.md`
/// (same allowlist as read/write). The output is a SIBLING file (same dir + stem, new
/// extension), so the write target is BACKEND-DERIVED from the allowlisted source, not
/// renderer-chosen. Requires pandoc on PATH (resolved with the same lookup as the CLI's
/// other tools); a clear install hint is returned when missing. Shells out on
/// spawn_blocking (no shell: args are passed to Command directly, paths are not
/// interpreted). Returns the written path.
#[tauri::command]
pub(crate) async fn export_markdown(
    app: AppHandle,
    src_path: String,
    format: String,
) -> Result<String, String> {
    let (writer, ext) =
        pandoc_target(&format).ok_or_else(|| format!("unsupported export format: {format}"))?;
    let pandoc = unlocr::preflight::locate("pandoc").ok_or_else(|| {
        "pandoc not found on PATH. Install it (e.g. `brew install pandoc`) to export.".to_string()
    })?;
    tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        // Validate the source against the app-produced allowlist (same guard as read),
        // then derive the output path from the canonical source so it cannot escape.
        let state = app.state::<AppState>();
        let allowed = allowed_output_paths(&state);
        let canonical = check_readable(&src_path, &allowed)?;
        let out = canonical.with_extension(ext);
        // `-s` (standalone) so html/rtf get a complete document, not a fragment; it is
        // a no-op for the always-standalone docx/odt writers.
        let output = std::process::Command::new(&pandoc)
            .arg(&canonical)
            .args(["-f", "markdown", "-t", writer, "-s", "-o"])
            .arg(&out)
            .output()
            .map_err(|e| format!("failed to run pandoc: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "pandoc failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(out.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("export worker join failed: {e}"))?
}

/// Canonicalized set of files `read_text_file` may serve: the paths written this
/// session plus the non-empty `output_path`s persisted in the job store. Paths that
/// no longer resolve (deleted output) are simply absent, so a stale store entry
/// fails closed rather than widening scope.
fn allowed_output_paths(state: &AppState) -> HashSet<PathBuf> {
    let mut set: HashSet<PathBuf> = state
        .read_allow
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    for job in crate::store::load_jobs() {
        if job.output_path.is_empty() {
            continue;
        }
        if let Ok(c) = std::fs::canonicalize(&job.output_path) {
            set.insert(c);
        }
    }
    set
}

/// Pure-of-Tauri core of `read_text_file`: validate `path` and return the canonical
/// target iff it is allowed. Split out so it is unit-testable with an explicit
/// `allowed` set (no live Tauri `State`). Defenses, in order:
/// 1. `.md` extension pre-check on the raw string (fast reject).
/// 2. `canonicalize` resolves `..` + symlinks and rejects non-existent paths
///    (fail closed); re-check the extension on the resolved target to defeat a
///    `foo.md -> /etc/shadow` symlink.
/// 3. Exact match against the backend-derived `allowed` set (not a dir prefix).
fn check_readable(path: &str, allowed: &HashSet<PathBuf>) -> Result<PathBuf, String> {
    if Path::new(path).extension().and_then(|e| e.to_str()) != Some("md") {
        return Err(format!("refusing to read non-markdown path: {path}"));
    }
    let canonical =
        std::fs::canonicalize(path).map_err(|e| format!("cannot resolve path {path}: {e}"))?;
    if canonical.extension().and_then(|e| e.to_str()) != Some("md") {
        return Err(format!(
            "refusing to read non-markdown path after resolution: {}",
            canonical.display()
        ));
    }
    if !allowed.contains(&canonical) {
        return Err(format!(
            "refusing to read a file the app did not produce: {}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

/// Rasterize a PDF to per-page PNGs for the preview pane, cached on disk by the
/// core lib so a repeat preview skips pdftoppm. Returns absolute PNG paths; the
/// frontend wraps each with `convertFileSrc` to load it through the asset
/// protocol (the previews dir is allow-listed in the asset scope at startup,
/// see `run()`). Runs on spawn_blocking (pdftoppm shell-out) so the webview never
/// freezes; `unlocr`'s `Box<dyn Error>` is stringified inside the closure so the
/// future stays Send.
#[tauri::command]
pub(crate) async fn render_pages(
    pdf_path: String,
    dpi: Option<u32>,
) -> Result<Vec<String>, String> {
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

/// Render ONE page (1-based) of a PDF for the preview pane, returning its PNG path.
/// Backs the preview's lazy per-page load: importing a large PDF no longer rasterizes
/// every page up front, only the page being viewed (and the next, on navigation).
/// Shares the same on-disk cache as `render_pages`. Returns Err for an out-of-range
/// page, which the frontend uses as the "past the last page" signal (no separate
/// page-count probe). Same poppler-only resolution + spawn_blocking as `render_pages`.
#[tauri::command]
pub(crate) async fn render_page(
    pdf_path: String,
    page: u32,
    dpi: Option<u32>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let dpi = dpi.unwrap_or_else(|| OcrOptions::default().dpi);
        let cache = unlocr::model::cache_dir(None).map_err(|e| e.to_string())?;
        let pdftoppm = unlocr::preflight::pdftoppm().map_err(|e| e.to_string())?;
        let path = unlocr::render_page(&pdftoppm, Path::new(&pdf_path), dpi, &cache, page)
            .map_err(|e| e.to_string())?;
        Ok(path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("render worker join failed: {e}"))?
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
///           named after the stem). Empty string + no `out_file` = in-memory only.
/// `out_file` optional explicit output filename/path for the single-input case
///           (relative -> under `out_dir`, absolute -> verbatim, `.md` appended when
///           missing). Rejected with multiple inputs.
/// Remaining params map onto the per-run `OcrOptions` fields (quant is fixed at
/// load time and not accepted here).
// Tauri commands take one fn arg per invoke field; the count is the JS contract,
// not a refactor smell. A params struct would just move the fields, not remove them.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(crate) async fn run_ocr(
    app: AppHandle,
    inputs: Vec<String>,
    out_dir: String,
    out_file: Option<String>,
    max_tokens: Option<u32>,
    dpi: Option<u32>,
    prompt: Option<String>,
    keep_images: Option<bool>,
    repeat_penalty: Option<f32>,
    first_page: Option<u32>,
    last_page: Option<u32>,
) -> Result<Vec<String>, String> {
    // Per-run options from defaults + the fields the frontend sent. `quant`/`port`
    // are irrelevant here (the model is already loaded/held). image_max_tokens /
    // chat_template are startup-only and were baked into the loaded Server.
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
    // Per-request sampling knob; None leaves the server default in place.
    opts.repeat_penalty = repeat_penalty;

    // Reject out-of-range numerics before they reach pdftoppm/inference. The JS
    // form guards v > 0, but a direct invoke bypasses it: dpi=0 makes pdftoppm
    // "produce no pages" and max_tokens=0 yields silently-empty page content.
    if opts.dpi == 0 {
        return Err("dpi must be greater than 0".to_string());
    }
    if opts.max_tokens == 0 {
        return Err("max_tokens must be greater than 0".to_string());
    }
    // A repeat penalty <= 0 (or non-finite) drives llama.cpp's sampler into
    // degenerate output. The JS form clamps to >= 1, but a direct invoke does not.
    if let Some(rp) = opts.repeat_penalty {
        if !rp.is_finite() || rp <= 0.0 {
            return Err("repeat_penalty must be a finite value greater than 0".to_string());
        }
    }
    // Page selection: both None = all pages (default). Otherwise build a 1-based
    // range, defaulting a missing first to 1. A missing last is an OPEN upper bound
    // (first..end of document), preserved as None so the UI's "pages N-end" actually
    // OCRs to EOF rather than collapsing to a single page. Validate here too: a
    // direct invoke bypasses the HTML form's min= clamp.
    opts.pages = match (first_page, last_page) {
        (None, None) => None,
        (f, l) => {
            let first = f.unwrap_or(1);
            if first == 0 {
                return Err("first_page is 1-based; 0 is not valid".to_string());
            }
            if let Some(last) = l {
                if last < first {
                    return Err(format!("page range is reversed: {first}-{last}"));
                }
            }
            Some((first, l))
        }
    };

    // A custom out_file names one file; ambiguous across a batch (mirror the CLI).
    if out_file.is_some() && inputs.len() > 1 {
        return Err("out_file names a single file; clear it for multiple inputs".to_string());
    }

    #[cfg(debug_assertions)]
    eprintln!(
        "[run_ocr] effective opts: dpi={} max_tokens={} keep_images={}",
        opts.dpi, opts.max_tokens, opts.keep_images
    );

    let out_dir = PathBuf::from(out_dir);

    // Stamp the idle clock at run START, not only at run-END (below). Otherwise the
    // idle-unload watcher (lib.rs) can unload an idle-past-threshold model in the
    // window between this invoke and the worker acquiring the model lock, failing the
    // run with "load a model first" on a model the user just clicked Run on. Stamping
    // here (synchronously, before dispatch) shrinks that window to IPC transit.
    {
        let state = app.state::<AppState>();
        *state.last_used.lock().unwrap_or_else(|p| p.into_inner()) =
            Some(std::time::Instant::now());
    }

    let app_for_join = app.clone();
    let res = tauri::async_runtime::spawn_blocking(move || -> Result<Vec<String>, String> {
        let state = app.state::<AppState>();

        // pdftoppm was resolved at load time (rasterization is always local).
        let pdftoppm = state
            .pdftoppm
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or("load a model first")?;

        let mut results = Vec::with_capacity(inputs.len());
        let mut errors = Vec::new();
        // Set true if stop_ocr fired mid-run; handled after the model guard drops.
        let mut stopped = false;

        // Hold the model lock for the whole batch (inner scope so the guard drops
        // before we may need to re-lock to clear a stopped run's dead model). The
        // local Server cannot be cloned, and serializing runs is fine for one user.
        // Only a local backend has a server stop_ocr can kill; captured inside the
        // guard scope but used after it drops, so declare it out here.
        let is_local;
        {
            let guard = state.model.lock().unwrap_or_else(|p| p.into_inner());
            let lm = guard.as_ref().ok_or("load a model first")?;
            is_local = matches!(&lm.backend, Backend::Local(_));

            // NOTE: do NOT reset `cancel` here. A Stop clicked in the brief window
            // between the JS invoke and this point would be silently overwritten.
            // `cancel` is false on entry by invariant: load_model clears it, and a
            // clean run clears it at the end (below); a stopped run drops the model,
            // so the next run can't start until a reload re-clears it.

            for input in &inputs {
                let input_path = PathBuf::from(input);

                // Per-input progress sink: forward Page (per-page bar) and PartialText
                // (live token stream) to the webview. Best-effort emit (a failed IPC
                // must never abort the run).
                //
                // Coalesce streamed tokens: ocr_pages emits one PartialText per token,
                // and one Tauri IPC dispatch per token floods the webview event loop on
                // a repetition-heavy page (each dispatch runs a JS handler), starving
                // clicks like Stop (see gui/CLAUDE.md gotcha). Buffer per page and emit
                // one event per ~FLUSH_CHARS or per newline instead. The previous page's
                // tail is flushed when the bar advances (Page event); the final page's
                // sub-threshold tail is rendered by ocr://done (assembled markdown), so
                // it needs no separate flush.
                const FLUSH_CHARS: usize = 256;
                let app_for_progress = app.clone();
                let mut buf = String::new();
                let mut buf_page = 0usize;
                let mut on_progress = |p: Progress| match p {
                    Progress::Page { page, total } => {
                        if !buf.is_empty() {
                            let _ = app_for_progress.emit(
                                "ocr://partial-text",
                                PartialText {
                                    page: buf_page,
                                    chunk: std::mem::take(&mut buf),
                                },
                            );
                        }
                        let _ = app_for_progress.emit("ocr://page", PageProgress { page, total });
                    }
                    Progress::PartialText { page, chunk } => {
                        buf_page = page;
                        let had_newline = chunk.contains('\n');
                        buf.push_str(&chunk);
                        if buf.len() >= FLUSH_CHARS || had_newline {
                            let _ = app_for_progress.emit(
                                "ocr://partial-text",
                                PartialText {
                                    page: buf_page,
                                    chunk: std::mem::take(&mut buf),
                                },
                            );
                        }
                    }
                    _ => {}
                };

                // Dispatch to the held backend. ocr_pages is generic over ImageOcr so
                // both the local Server and the RemoteEndpoint drive the same loop.
                // Stop sets state.cancel; ocr_pages checks it at each page boundary so a
                // remote run (no pid to kill) aborts at the next page. The local backend
                // is also killed by pid in stop_ocr, so its in-flight stream errors out.
                let should_cancel = || state.cancel.load(Ordering::SeqCst);
                let outcome = match &lm.backend {
                    Backend::Local(srv) => unlocr::ocr_pages(
                        srv,
                        &pdftoppm,
                        &input_path,
                        &opts,
                        &mut on_progress,
                        &should_cancel,
                    ),
                    Backend::Remote(ep) => unlocr::ocr_pages(
                        ep,
                        &pdftoppm,
                        &input_path,
                        &opts,
                        &mut on_progress,
                        &should_cancel,
                    ),
                };
                // stop_ocr kills the local server, so the in-flight stream read fails
                // here. Remap that error to a clean "stopped" so the UI shows intent,
                // not a raw connection error. (Remote backend has no pid to kill, so
                // stop cannot abort an in-flight remote run; it finishes normally.)
                let (md, kept) = match outcome {
                    Ok(v) => v,
                    Err(e) if state.cancel.load(Ordering::SeqCst) => {
                        let _ = e;
                        stopped = true;
                        break;
                    }
                    Err(e) => {
                        errors.push(format!("{}: {}", input_path.display(), e));
                        continue;
                    }
                };

                if let Some(dir) = kept {
                    let _ = app.emit(
                        "ocr://images-kept",
                        ImagesKept {
                            dir: dir.display().to_string(),
                        },
                    );
                }
                let _ = app.emit(
                    "ocr://done",
                    OcrDone {
                        markdown: md.clone(),
                    },
                );

                // Write to disk when a folder OR an explicit filename was given; only an
                // empty folder AND no filename keeps the result in memory (results = md).
                if out_dir.as_os_str().is_empty() && out_file.is_none() {
                    results.push(md);
                } else {
                    let stem = input_path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("output");
                    // Shared resolver (with the CLI): out_file wins, else {stem}.md under
                    // out_dir; .md appended when missing, absolute out_file used verbatim.
                    let out_path = unlocr::resolve_output_path(
                        &out_dir,
                        out_file.as_deref().map(Path::new),
                        stem,
                    );
                    // Create the parent first (a custom/absolute out_file may target a
                    // not-yet-created dir), matching the CLI's create_dir_all.
                    let mut write_failed = false;
                    if let Some(parent) = out_path.parent() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            errors.push(format!("failed to create {}: {e}", parent.display()));
                            write_failed = true;
                        }
                    }
                    if !write_failed {
                        if let Err(e) = std::fs::write(&out_path, &md) {
                            errors.push(format!("failed to write {}: {e}", out_path.display()));
                            write_failed = true;
                        }
                    }
                    if !write_failed {
                        let abs_path = if out_path.is_absolute() {
                            out_path
                        } else {
                            match std::env::current_dir() {
                                Ok(cwd) => cwd.join(&out_path),
                                Err(_) => out_path,
                            }
                        };
                        // Authorize the review pane to read THIS file back. Canonicalize
                        // so it matches read_text_file's canonical comparison (the file
                        // exists now); fall back to the absolute path if that fails.
                        let canon =
                            std::fs::canonicalize(&abs_path).unwrap_or_else(|_| abs_path.clone());
                        if let Ok(mut g) = state.read_allow.lock() {
                            g.insert(canon);
                        }
                        results.push(abs_path.display().to_string());
                    }
                }
            }
        } // model guard dropped here

        if stopped {
            // stop_ocr killed the local server, so the held model is now dead.
            // Drop it (and the stale pid) so the UI gate flips to "load a model
            // first" instead of letting the next Run hit a dead socket. Remote
            // runs have no pid to kill and don't reach here.
            *state.model.lock().unwrap_or_else(|p| p.into_inner()) = None;
            *state.server_pid.lock().unwrap_or_else(|p| p.into_inner()) = None;
            return Err("stopped".to_string());
        }
        // Clean batch: clear cancel so the next run starts from a known state (we no
        // longer reset at run entry, to avoid racing a launch-window Stop). If a Stop
        // landed in the TAIL window (after the last page finished but before this
        // reset) on a local backend, stop_ocr may have just killed the server, so drop
        // the now-dead model -> the next Run gates to "load a model first" instead of
        // hitting a dead socket. This run still completed, so its results are returned.
        let tail_stop = state.cancel.swap(false, Ordering::SeqCst);
        if tail_stop && is_local {
            *state.model.lock().unwrap_or_else(|p| p.into_inner()) = None;
            *state.server_pid.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }

        // Surface failures, but never discard the outputs that DID succeed. A
        // single-input run that fails has no successes (results empty) -> Err as
        // before; a partial batch keeps the written paths (Ok) and logs the rest, so
        // a caller passing multiple inputs does not lose every good file to one bad one.
        if !errors.is_empty() {
            if results.is_empty() {
                return Err(errors.join("; "));
            }
            eprintln!(
                "[run_ocr] {} input(s) failed: {}",
                errors.len(),
                errors.join("; ")
            );
        }

        Ok(results)
    })
    .await;
    // Stamp run-end for the idle-unload watcher (lib.rs), whatever the outcome. The
    // model lock protects against unloading DURING a run; stamping here starts the
    // idle clock from when the run finished, not when it started.
    {
        let state = app_for_join.state::<AppState>();
        *state.last_used.lock().unwrap_or_else(|p| p.into_inner()) =
            Some(std::time::Instant::now());
    }
    match res {
        Ok(val) => val,
        Err(e) => {
            let state = app_for_join.state::<AppState>();
            *state.model.lock().unwrap_or_else(|p| p.into_inner()) = None;
            *state.server_pid.lock().unwrap_or_else(|p| p.into_inner()) = None;
            Err(format!("ocr worker join failed: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::check_readable;
    use std::collections::HashSet;

    /// A path in the allowed set is served; an existing-but-unlisted .md is rejected
    /// (the core of the renderer-cannot-widen-scope guarantee).
    #[test]
    fn check_readable_exact_match_only() {
        let tmp = tempfile::tempdir().unwrap();
        let allow = tmp.path().join("allowed.md");
        let other = tmp.path().join("other.md");
        std::fs::write(&allow, b"# ok").unwrap();
        std::fs::write(&other, b"# secret").unwrap();

        // canonicalize so the set matches what check_readable computes internally.
        let mut set = HashSet::new();
        set.insert(std::fs::canonicalize(&allow).unwrap());

        let got = check_readable(allow.to_str().unwrap(), &set).unwrap();
        assert_eq!(got, std::fs::canonicalize(&allow).unwrap());

        // An existing .md that the app did not produce is refused.
        let err = check_readable(other.to_str().unwrap(), &set).unwrap_err();
        assert!(err.contains("did not produce"), "unexpected error: {err}");
    }

    /// The write path shares `check_readable`'s guard: an allowed `.md` overwrites,
    /// an existing-but-unlisted `.md` is refused (cannot widen scope to write).
    #[test]
    fn write_guard_overwrites_only_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let allow = tmp.path().join("allowed.md");
        let other = tmp.path().join("other.md");
        std::fs::write(&allow, b"# old").unwrap();
        std::fs::write(&other, b"# secret").unwrap();

        let mut set = HashSet::new();
        set.insert(std::fs::canonicalize(&allow).unwrap());

        // Allowed: resolve then overwrite.
        let canonical = check_readable(allow.to_str().unwrap(), &set).unwrap();
        std::fs::write(&canonical, b"# new").unwrap();
        assert_eq!(std::fs::read_to_string(&allow).unwrap(), "# new");

        // Unlisted: refused before any write; file is untouched.
        let err = check_readable(other.to_str().unwrap(), &set).unwrap_err();
        assert!(err.contains("did not produce"), "unexpected error: {err}");
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "# secret");
    }

    /// Export format mapping: known formats resolve to (writer, ext); unknown is None
    /// (rejected before pandoc runs, so the renderer can't pass an arbitrary writer).
    #[test]
    fn pandoc_target_maps_known_formats_only() {
        use super::pandoc_target;
        assert_eq!(pandoc_target("html"), Some(("html", "html")));
        assert_eq!(pandoc_target("txt"), Some(("plain", "txt")));
        assert_eq!(pandoc_target("docx"), Some(("docx", "docx")));
        assert_eq!(pandoc_target("odt"), Some(("odt", "odt")));
        assert_eq!(pandoc_target("rtf"), Some(("rtf", "rtf")));
        assert_eq!(pandoc_target("pdf"), None);
        assert_eq!(pandoc_target(""), None);
    }

    /// Non-.md paths are rejected before any filesystem access.
    #[test]
    fn check_readable_rejects_non_markdown() {
        let set = HashSet::new();
        let err = check_readable("/etc/passwd", &set).unwrap_err();
        assert!(err.contains("non-markdown"), "unexpected error: {err}");
    }
}
