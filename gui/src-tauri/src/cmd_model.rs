// Model lifecycle + environment commands: preflight, load/unload, stop, status,
// local-model listing, and model-cache info/clear. The held backend lives in
// `AppState` (state.rs); these commands install into it and read it back.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use unlocr::server::{free_port, RemoteEndpoint, Server};
use unlocr::{OcrOptions, Progress};

use crate::state::{AppState, Backend, LoadedModel};

/// Status payload for `model_status` / `load_model` returns. Drives the titlebar
/// Local/Remote badge and the Run OCR enable/disable gate.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelStatus {
    loaded: bool,
    /// "local" | "remote" | "" when nothing is loaded.
    mode: String,
    /// Display label ("Unlimited-OCR Q8_0" / the remote URL), empty when unloaded.
    label: String,
}

/// Payload for the `ocr://progress` event (model/projector download). `pct` is
/// 0..=100. The frontend uses this for an indeterminate->determinate bar switch.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    name: String,
    pct: u8,
    /// Bytes transferred so far and the full file size (0 if the server omitted
    /// Content-Length). The frontend shows size and derives MB/s from successive
    /// events; `pct` stays for the simple bar.
    done: u64,
    total: u64,
}

/// Payload for the `ocr://server-ready` event. Emitted once llama-server is
/// healthy, before per-page OCR begins. The frontend can use `port` for a
/// "server up on :port" status line.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerReady {
    port: u16,
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
pub(crate) struct PreflightReport {
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
pub(crate) fn preflight(
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
            let (_, model_present, _, mmproj_present) =
                unlocr::model::check_presence(&cache, &quant).unwrap_or((Default::default(), false, Default::default(), false));
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

    let (_, model_present, _, mmproj_present) =
        unlocr::model::check_presence(&cache, &quant).unwrap_or((Default::default(), false, Default::default(), false));

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
pub(crate) async fn load_model(
    app: AppHandle,
    mode: String,
    quant: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    llama_bin: Option<String>,
    image_max_tokens: Option<u32>,
    chat_template: Option<String>,
    model_file: Option<String>,
    mmproj_file: Option<String>,
) -> Result<ModelStatus, String> {
    let llama_override = llama_bin.filter(|s| !s.trim().is_empty()).map(PathBuf::from);
    // Custom-GGUF mode (local only): when set, the model file is used directly,
    // skipping the HF download and the quant naming convention. mmproj_file is an
    // optional projector override; omit it to use the stock cached/downloaded one.
    // Empty string -> None (same trim pattern as the other path fields).
    let model_override = model_file.filter(|s| !s.trim().is_empty()).map(PathBuf::from);
    let mmproj_override = mmproj_file.filter(|s| !s.trim().is_empty()).map(PathBuf::from);
    // Overrides apply to the local spawn only; the remote arm ignores them. Skip the
    // pairing guard in remote mode so a stale projector pick (picker hidden but its
    // dataset persists after switching local->remote) can't spuriously fail a remote load.
    if mode != "remote" && mmproj_override.is_some() && model_override.is_none() {
        return Err("mmproj_file requires model_file".to_string());
    }
    // Startup-only knobs (local mode): they parameterize the llama-server spawn,
    // so they belong here at load time, not per-run. Empty string = unset.
    let chat_template = chat_template.filter(|s| !s.trim().is_empty());
    // The JS form clamps to >= 1, but a direct invoke does not. llama-server
    // rejects --image-max-tokens 0 at spawn, so fail early with a clear message.
    if image_max_tokens == Some(0) {
        return Err("image_max_tokens must be greater than 0".to_string());
    }

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
        // Set in the local branch; stays None for remote (nothing local to kill).
        let mut pid: Option<u32> = None;
        let (backend, label, mode_out, pdftoppm) = match mode.as_str() {
            "remote" => {
                let base = base_url.unwrap_or_default().trim().to_string();
                if base.is_empty() {
                    return Err("remote mode requires a base URL".into());
                }
                let pdftoppm = unlocr::preflight::pdftoppm().map_err(|e| e.to_string())?;
                // Same caveat the CLI prints: these OCR models only run on a few
                // servers. Surfaced in the UI too (Settings/model-bar notes).
                eprintln!(
                    "[load_model] remote mode: Unlimited-OCR / DeepSeek-OCR is only known to run \
                     on llama.cpp (PR #17400), vLLM, and SGLang. Ollama / LM Studio do not support \
                     these OCR models; gateways (litellm/vLLM) need a model name set."
                );
                let ep = RemoteEndpoint {
                    base_url: base.clone(),
                    api_key: api_key.filter(|k| !k.trim().is_empty()),
                    model: model.filter(|m| !m.trim().is_empty()),
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
                    if let Progress::Download { name, pct, done, total } = p {
                        let _ = app_dl.emit(
                            "ocr://progress",
                            DownloadProgress { name, pct, done, total },
                        );
                    }
                };
                // Custom-GGUF mode routes through ensure_with_overrides (override
                // paths used verbatim, existence-checked in model.rs); else the
                // stock cache + download path. Both yield ModelFiles for Server::start.
                let files = unlocr::model::ensure_with_overrides(
                    &cache,
                    &quant,
                    model_override.as_deref(),
                    mmproj_override.as_deref(),
                    &mut on_progress,
                )
                .map_err(|e| e.to_string())?;
                let port = free_port().map_err(|e| e.to_string())?;
                let srv = Server::start(
                    &tools.llama_server,
                    &files.model,
                    &files.mmproj,
                    port,
                    image_max_tokens,
                    chat_template.as_deref(),
                )
                .map_err(|e| e.to_string())?;
                let _ = app.emit("ocr://server-ready", ServerReady { port });
                // Capture the pid before `srv` moves into Backend so `stop_ocr`
                // can kill it without the model lock (see AppState::server_pid).
                pid = Some(srv.pid());
                // Label: the model file stem for a custom GGUF, else the quant tag.
                let label = match model_override.as_deref() {
                    Some(p) => p
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "custom model".to_string()),
                    None => format!("Unlimited-OCR {quant}"),
                };
                (Backend::Local(srv), label, "local".to_string(), tools.pdftoppm)
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
        // Record the local server pid for stop_ocr; clear any stale cancel from a
        // prior stopped run so the fresh model can run.
        if let Ok(mut sp) = state.server_pid.lock() {
            *sp = pid;
        }
        state.cancel.store(false, Ordering::SeqCst);
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
pub(crate) fn unload_model(state: State<'_, AppState>) -> ModelStatus {
    if let Ok(mut g) = state.model.lock() {
        *g = None;
    }
    ModelStatus {
        loaded: false,
        mode: String::new(),
        label: String::new(),
    }
}

/// Stop an in-flight run. Sets the cancel flag and kills the held local
/// llama-server by pid so the in-flight stream read aborts immediately (run_ocr
/// remaps the resulting error to "stopped"). The killed server cannot serve
/// again, so the user must reload the model before the next run — that is the
/// intended Stop-only tradeoff (no pause/resume; llama-server can't pause).
/// Remote backend has no pid stashed, so stop cannot abort an in-flight remote
/// run; it only sets the flag.
#[tauri::command]
pub(crate) fn stop_ocr(state: State<'_, AppState>) {
    state.cancel.store(true, Ordering::SeqCst);
    let pid = state
        .server_pid
        .lock()
        .ok()
        .and_then(|g| *g);
    if let Some(pid) = pid {
        // ponytail: shell-kill by pid; the child handle lives inside the Server
        // behind the held model lock, which stop_ocr cannot take mid-run.
        #[cfg(unix)]
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status();
        #[cfg(windows)]
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status();
        // The server is dead; clear the pid so a stale value can't be re-killed.
        if let Ok(mut g) = state.server_pid.lock() {
            *g = None;
        }
    }
}

/// Current load state for the titlebar badge + the Run OCR gate.
#[tauri::command]
pub(crate) fn model_status(state: State<'_, AppState>) -> ModelStatus {
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
pub(crate) fn list_local_models() -> Vec<String> {
    match unlocr::model::cache_dir(None) {
        Ok(cache) => unlocr::model::list_cached_quants(&cache),
        Err(_) => Vec::new(),
    }
}

/// Return the model cache directory path and the total size of its GGUF files in
/// bytes. Used by the Settings view to show how much disk the cached models use
/// and to let the user locate the directory. Errors are stringified so the
/// future stays Send. Size is best-effort: unreadable entries are skipped.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CacheInfo {
    /// Absolute path of the model cache directory (for display).
    path: String,
    /// Total size of all .gguf files in the cache, in bytes.
    size_bytes: u64,
}

#[tauri::command]
pub(crate) fn get_cache_info() -> Result<CacheInfo, String> {
    let cache = unlocr::model::cache_dir(None).map_err(|e| e.to_string())?;
    let path = cache.display().to_string();
    // Sum the size of every .gguf file (model + mmproj). Non-.gguf files (logs,
    // settings.json, jobs.json, previews/) are excluded — they are not model data.
    let size_bytes = std::fs::read_dir(&cache)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .and_then(|x| x.to_str())
                        .map(|x| x == "gguf")
                        .unwrap_or(false)
                })
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0);
    Ok(CacheInfo { path, size_bytes })
}

/// Delete all .gguf files from the model cache (the model and projector GGUFs
/// for every quant). Non-model files (settings.json, jobs.json, previews/) are
/// left intact. Any currently-loaded model is NOT unloaded first: the caller is
/// responsible for unloading before clearing if that matters. Errors are returned
/// as a string so the frontend can surface them inline.
#[tauri::command]
pub(crate) fn clear_model_cache() -> Result<(), String> {
    let cache = unlocr::model::cache_dir(None).map_err(|e| e.to_string())?;
    let entries = std::fs::read_dir(&cache).map_err(|e| e.to_string())?;
    let mut errors: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) == Some("gguf") {
            if let Err(e) = std::fs::remove_file(&path) {
                errors.push(format!("{}: {e}", path.display()));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("some files could not be removed: {}", errors.join("; ")))
    }
}
