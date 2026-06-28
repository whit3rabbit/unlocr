// OCR run + file IO commands: render_pages (preview rasterization), read_text_file
// (fetch the written .md for the review pane), and run_ocr (the batch OCR loop).
// These drive the held backend in `AppState` (state.rs).

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
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
pub(crate) fn read_text_file(path: String, allowed_dir: Option<String>) -> Result<String, String> {
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
pub(crate) async fn render_pages(pdf_path: String, dpi: Option<u32>) -> Result<Vec<String>, String> {
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
pub(crate) async fn run_ocr(
    app: AppHandle,
    inputs: Vec<String>,
    out_dir: String,
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
    // inclusive range, defaulting a missing bound (first->1, last->first). Validate
    // here too: a direct invoke bypasses the HTML form's min= clamp.
    opts.pages = match (first_page, last_page) {
        (None, None) => None,
        (f, l) => {
            let first = f.unwrap_or(1);
            let last = l.unwrap_or(first);
            if first == 0 {
                return Err("first_page is 1-based; 0 is not valid".to_string());
            }
            if last < first {
                return Err(format!("page range is reversed: {first}-{last}"));
            }
            Some((first, last))
        }
    };

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

        let mut results = Vec::with_capacity(inputs.len());
        // Set true if stop_ocr fired mid-run; handled after the model guard drops.
        let mut stopped = false;

        // Hold the model lock for the whole batch (inner scope so the guard drops
        // before we may need to re-lock to clear a stopped run's dead model). The
        // local Server cannot be cloned, and serializing runs is fine for one user.
        {
        let guard = state.model.lock().map_err(|_| "model state poisoned")?;
        let lm = guard.as_ref().ok_or("load a model first")?;

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
            let app_for_progress = app.clone();
            let mut on_progress = |p: Progress| match p {
                Progress::Page { page, total } => {
                    let _ = app_for_progress.emit("ocr://page", PageProgress { page, total });
                }
                Progress::PartialText { page, chunk } => {
                    let _ = app_for_progress.emit("ocr://partial-text", PartialText { page, chunk });
                }
                _ => {}
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
                Err(e) => return Err(e.to_string()),
            };

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
        } // model guard dropped here

        if stopped {
            // stop_ocr killed the local server, so the held model is now dead.
            // Drop it (and the stale pid) so the UI gate flips to "load a model
            // first" instead of letting the next Run hit a dead socket. Remote
            // runs have no pid to kill and don't reach here.
            if let Ok(mut g) = state.model.lock() {
                *g = None;
            }
            if let Ok(mut sp) = state.server_pid.lock() {
                *sp = None;
            }
            return Err("stopped".to_string());
        }
        // Clean batch: clear cancel so the next run starts from a known state
        // (we no longer reset at run entry, to avoid racing a launch-window Stop).
        state.cancel.store(false, Ordering::SeqCst);
        Ok(results)
    })
    .await
    .map_err(|e| format!("ocr worker join failed: {e}"))?
}
