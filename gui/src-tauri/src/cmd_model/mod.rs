// Model lifecycle + environment commands.
// Handles load, unload, stop, status lifecycle events, and delegates cache/preflight to submodules.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use unlocr::server::{free_port, MlxServer, RemoteEndpoint, Server, MLX_DEFAULT_MODEL};
use unlocr::{OcrOptions, Progress};

use crate::state::{AppState, Backend, LoadedModel};

mod cache;
pub(crate) use cache::{
    clear_model_cache, get_cache_info, list_available_quants, list_cached_files, list_local_models,
    preflight, remove_cached_file,
};
mod sysreq;
pub(crate) use sysreq::system_requirements;

/// Status payload for `model_status` / `load_model` returns.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelStatus {
    pub loaded: bool,
    /// "local" | "mlx" | "remote" | "" when nothing is loaded.
    pub mode: String,
    /// Display label ("Unlimited-OCR Q8_0" / the remote URL), empty when unloaded.
    pub label: String,
}

/// Payload for the `ocr://progress` event.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    name: String,
    pct: u8,
    done: u64,
    total: u64,
}

/// Payload for the `ocr://server-ready` event.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerReady {
    port: u16,
}

/// Payload for the `ocr://status` event: a free-form one-line message surfacing
/// what a long, otherwise event-less phase is doing (model load into RAM,
/// rasterization) so the UI does not look frozen.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusMsg {
    message: String,
}

/// Load a model so it stays warm in RAM (litellm-style), gating Run OCR until it
/// succeeds.
#[allow(clippy::too_many_arguments)]
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
    let llama_override = llama_bin
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from);
    let model_override = model_file
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from);
    let mmproj_override = mmproj_file
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from);
    if mode != "remote" && mmproj_override.is_some() && model_override.is_none() {
        return Err("mmproj_file requires model_file".to_string());
    }
    let chat_template = chat_template.filter(|s| !s.trim().is_empty());
    if image_max_tokens == Some(0) {
        return Err("image_max_tokens must be greater than 0".to_string());
    }

    // In-flight guard: refuse a second concurrent load instead of spawning another
    // server on top of the one still starting (the accumulation source). CAS after
    // the cheap validations so an early Err above never leaves the flag stuck set.
    {
        let state = app.state::<AppState>();
        if state
            .loading
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(
                "a model load is already in progress; wait for it to finish or Unload first"
                    .to_string(),
            );
        }
    }
    // Cleared unconditionally after the load future resolves (below), so a failed
    // or panicking load never wedges the flag. Cloned because `app` moves into the
    // blocking closure.
    let app_flag = app.clone();

    let res = tauri::async_runtime::spawn_blocking(move || -> Result<ModelStatus, String> {
        {
            let state = app.state::<AppState>();
            let mut g = state.model.lock().unwrap_or_else(|p| p.into_inner());
            *g = None;
            *state.server_pid.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }

        let mut pid: Option<u32> = None;
        let (backend, label, mode_out, pdftoppm) = match mode.as_str() {
            "remote" => {
                let base = base_url.unwrap_or_default().trim().to_string();
                if base.is_empty() {
                    return Err("remote mode requires a base URL".into());
                }
                let pdftoppm = unlocr::preflight::pdftoppm().map_err(|e| e.to_string())?;
                eprintln!(
                    "[load_model] remote mode: Unlimited-OCR / DeepSeek-OCR is only known to run \
                     on llama.cpp (PR #17400), vLLM, and SGLang. Ollama / LM Studio do not support \
                     these OCR models; gateways (litellm/vLLM) need a model name set."
                );
                let mut real_key = api_key.filter(|k| !k.trim().is_empty());
                if let Some(ref k) = real_key {
                    if k == "••••••••" || k == "__saved__" {
                        if let Ok(entry) = keyring::Entry::new("unlocr", "remote_api_key") {
                            if let Ok(key) = entry.get_password() {
                                real_key = Some(key);
                            } else {
                                real_key = None;
                            }
                        } else {
                            real_key = None;
                        }
                    }
                }
                let ep = RemoteEndpoint {
                    base_url: base.clone(),
                    api_key: real_key,
                    model: model.filter(|m| !m.trim().is_empty()),
                };
                if let Err(e) = ep.probe() {
                    eprintln!("[load_model] remote probe failed (continuing): {e}");
                }
                (Backend::Remote(ep), base, "remote".to_string(), pdftoppm)
            }
            "mlx" => {
                // Reap any orphaned mlxcel-server from a prior hung/abandoned load
                // before spawning a new one (see kill_stale_mlxcel). The warm model
                // was dropped at the top of this worker, so anything still running
                // under that name is a leak, not the current backend.
                kill_stale_mlxcel();
                let pdftoppm = unlocr::preflight::pdftoppm().map_err(|e| e.to_string())?;
                let model_repo = model
                    .filter(|m| !m.trim().is_empty())
                    .unwrap_or_else(|| MLX_DEFAULT_MODEL.to_string());

                let app_dl = app.clone();
                let mut on_progress = |p: Progress| {
                    if let Progress::Download {
                        name,
                        pct,
                        done,
                        total,
                    } = p
                    {
                        let _ = app_dl.emit(
                            "ocr://progress",
                            DownloadProgress {
                                name,
                                pct,
                                done,
                                total,
                            },
                        );
                    }
                };
                let bin = unlocr::preflight::resolve_mlxcel_server(None, None, &mut on_progress)
                    .map_err(|e| e.to_string())?;

                let port = free_port().map_err(|e| e.to_string())?;
                let _ = app.emit(
                    "ocr://status",
                    StatusMsg {
                        message: "starting mlxcel-server (first run downloads the model)…"
                            .to_string(),
                    },
                );
                // mlxcel-server's own model download happens INSIDE MlxServer::start,
                // before it answers /health, and can take minutes on a slow
                // connection; on_tick fires every few seconds so the model bar
                // keeps updating instead of sitting frozen on the message above.
                let app_tick = app.clone();
                let mut on_tick = |elapsed: std::time::Duration, stderr_tail: &str| {
                    let last_line = stderr_tail
                        .lines()
                        .next_back()
                        .map(str::trim)
                        .filter(|l| !l.is_empty());
                    let message = match last_line {
                        Some(line) => {
                            format!(
                                "starting mlxcel-server… {}s elapsed — {line}",
                                elapsed.as_secs()
                            )
                        }
                        None => format!("starting mlxcel-server… {}s elapsed", elapsed.as_secs()),
                    };
                    let _ = app_tick.emit("ocr://status", StatusMsg { message });
                };
                let srv = MlxServer::start(&bin, &model_repo, port, &mut on_tick)
                    .map_err(|e| e.to_string())?;

                let _ = app.emit("ocr://server-ready", ServerReady { port });
                pid = Some(srv.pid());
                let label = format!("Unlimited-OCR MLX ({model_repo})");
                (Backend::Mlx(srv), label, "mlx".to_string(), pdftoppm)
            }
            _ => {
                // Build the download-progress sink FIRST: preflight::check now
                // auto-downloads unlocr's managed llama-server and emits
                // Progress::Download for it, so the same sink surfaces both the
                // llama-server and the model download to the model bar.
                let app_dl = app.clone();
                let mut on_progress = |p: Progress| {
                    if let Progress::Download {
                        name,
                        pct,
                        done,
                        total,
                    } = p
                    {
                        let _ = app_dl.emit(
                            "ocr://progress",
                            DownloadProgress {
                                name,
                                pct,
                                done,
                                total,
                            },
                        );
                    }
                };

                let tools = unlocr::preflight::check(llama_override.as_deref(), &mut on_progress)
                    .map_err(|e| e.to_string())?;
                let quant = quant
                    .filter(|q| !q.trim().is_empty())
                    .unwrap_or_else(|| OcrOptions::default().quant);
                unlocr::model::validate_quant(&quant).map_err(|e| e.to_string())?;
                let cache = unlocr::model::cache_dir(None).map_err(|e| e.to_string())?;

                let files = unlocr::model::ensure_with_overrides(
                    &cache,
                    &quant,
                    model_override.as_deref(),
                    mmproj_override.as_deref(),
                    &mut on_progress,
                )
                .map_err(|e| e.to_string())?;

                let port = free_port().map_err(|e| e.to_string())?;
                // Server::start blocks in await_health while llama-server loads the
                // multi-GB GGUF into RAM, emitting nothing. Surface a status so the
                // model bar does not sit frozen on "downloading 100%"/"loading…".
                let _ = app.emit(
                    "ocr://status",
                    StatusMsg {
                        message: "loading model into memory (can take a minute)…".to_string(),
                    },
                );
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
                pid = Some(srv.pid());
                let label = match model_override.as_deref() {
                    Some(p) => p
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "custom model".to_string()),
                    None => format!("Unlimited-OCR {quant}"),
                };
                (
                    Backend::Local(srv),
                    label,
                    "local".to_string(),
                    tools.pdftoppm,
                )
            }
        };

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
        if let Some(p) = pid {
            *state.server_pid.lock().unwrap_or_else(|p| p.into_inner()) = Some(p);
        }
        // Stamp last_used INSIDE the worker, right after the model is installed, so
        // the 60s idle-unload watcher can never observe a stale (pre-load)
        // timestamp in the window between install and the stamp and unload a model
        // that was just loaded.
        *state.last_used.lock().unwrap_or_else(|p| p.into_inner()) =
            Some(std::time::Instant::now());

        Ok(ModelStatus {
            loaded: true,
            mode: mode_out,
            label,
        })
    })
    .await;
    // Release the in-flight guard no matter how the load ended (Ok / Err / panic-
    // as-JoinError), so the next Load is never permanently blocked.
    app_flag
        .state::<AppState>()
        .loading
        .store(false, Ordering::SeqCst);
    res.map_err(|e| format!("load worker join failed: {e}"))?
}

/// Unload the currently-held model.
#[tauri::command]
pub(crate) fn unload_model(state: State<'_, AppState>) -> ModelStatus {
    // A load in flight (flag set) means a worker is blocked in await_health holding
    // a child we can't reach through `model` (it isn't stored until the load
    // succeeds). This is the escape hatch for a hung mlxcel load (never answers
    // /health): reap it by name so the blocked worker's try_wait sees the exit,
    // unwinds, and clears the flag. Gated on `loading`, so a HEALTHY loaded model
    // (flag already false by the time load returns) is dropped normally below, not
    // pkilled out from under an in-flight run.
    if state.loading.load(Ordering::SeqCst) {
        kill_stale_mlxcel();
    }
    // Dropping the Server/MlxServer handle IS the kill: `Drop` kills+waits the
    // owned Child (identity-safe). We deliberately do NOT also kill by pid here.
    // The pid guard (`pid_is_managed_server`) checks only the comm name, not
    // process identity, so on the rare path where the managed server already
    // died and the OS recycled its pid to another llama-server/mlxcel-server, a
    // pid kill would terminate that unrelated process. `stop_ocr` still uses the
    // pid kill because a run
    // holds the model lock for the whole batch and it cannot drop the Server;
    // unload takes the lock and drops. The frontend flips the badge off + shows
    // "stopping server…" around this call, so the unload is visually honest.
    let mut g = state.model.lock().unwrap_or_else(|p| p.into_inner());
    *g = None;
    *state.server_pid.lock().unwrap_or_else(|p| p.into_inner()) = None;
    // Clear last_used so a later load is not judged against a stale timestamp by
    // the idle-unload watcher (the watcher clears it on its own unload; an
    // explicit unload must too).
    *state.last_used.lock().unwrap_or_else(|p| p.into_inner()) = None;
    ModelStatus {
        loaded: false,
        mode: String::new(),
        label: String::new(),
    }
}

/// Stop an in-flight run.
#[tauri::command]
pub(crate) fn stop_ocr(state: State<'_, AppState>) {
    state.cancel.store(true, Ordering::SeqCst);
    let pid = *state.server_pid.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(pid) = pid {
        kill_local_server_pid(pid);
        *state.server_pid.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

/// Kill any lingering mlxcel-server processes before spawning a fresh one. A hung
/// MLX load (mlxcel-server starts, loads weights from cache, then never answers
/// `/health` -- observed with the current `sahilchachra/unlimited-ocr-*-mlx` quants
/// under mlxcel v0.4.1, whose quant-param inference misreads these checkpoints)
/// blocks in `await_health` for the full `MLX_HEALTH_TIMEOUT` (30 min) holding its
/// child, so each repeated Load click strands one more mlxcel-server. `load_model`
/// has already dropped the warm model above (its `Drop` kills the tracked child),
/// so any process still named mlxcel-server at this point is an orphan. Killing it
/// also unblocks a still-waiting prior worker (its `try_wait` then sees the exit).
/// MLX is Apple-Silicon-only; this is a mac `pkill` and a no-op on other targets.
///
/// Scoped to unlocr's OWN managed binary path (`<cache>/tools/mlxcel-server`), not
/// the bare process name, so it never reaps an mlxcel-server a user runs from
/// elsewhere or another app spawned. Falls back to the bare name only if the cache
/// dir can't be resolved. (Two unlocr instances share the managed path and would
/// still cross-reap, but both are "ours" -- an accepted single-user-desktop edge.)
fn kill_stale_mlxcel() {
    #[cfg(unix)]
    {
        let pattern = unlocr::model::cache_dir(None)
            .ok()
            .map(|c| {
                unlocr::tools::tools_dir(&c)
                    .join("mlxcel-server")
                    .display()
                    .to_string()
            })
            .unwrap_or_else(|| "mlxcel-server".to_string());
        let _ = std::process::Command::new("pkill")
            .args(["-f", &pattern])
            .status();
    }
}

/// True if `pid` is currently a managed local server process (llama-server or
/// mlxcel-server -- the two `Backend::Local`/`Backend::Mlx` process kinds).
fn pid_is_managed_server(pid: u32) -> bool {
    #[cfg(unix)]
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output();
    #[cfg(windows)]
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            pid_names_managed_server(&String::from_utf8_lossy(&o.stdout))
        }
        _ => false,
    }
}

/// Whether a `ps -o comm=` / `tasklist` line names llama-server or
/// mlxcel-server. Kept as one name-check (not a per-backend split) so
/// `stop_ocr` does not need to know which backend is loaded.
fn pid_names_managed_server(proc_listing: &str) -> bool {
    proc_listing.lines().any(|l| {
        let l = l.to_ascii_lowercase();
        l.contains("llama-server") || l.contains("mlxcel-server")
    })
}

/// Kill the local backend process (llama-server or mlxcel-server) by pid:
/// SIGKILL on Unix, `taskkill /F` on Windows. No-op if the pid is stale or no
/// longer a recognized managed-server process (`pid_is_managed_server`). Used
/// only by `stop_ocr`, which cancels an in-flight run without taking the model
/// lock (a run holds it for the whole batch), so it cannot drop the `Server`/
/// `MlxServer` and must kill out-of-band. `unload_model` drops the backend
/// instead, whose `Drop` is the identity-safe kill on the owned Child.
fn kill_local_server_pid(pid: u32) {
    if !pid_is_managed_server(pid) {
        return;
    }
    #[cfg(unix)]
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
    #[cfg(windows)]
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .status();
}

/// Current load state for the titlebar badge + the Run OCR gate.
#[tauri::command]
pub(crate) fn model_status(state: State<'_, AppState>) -> ModelStatus {
    let g = state.model.lock().unwrap_or_else(|p| p.into_inner());
    match g.as_ref().map(|lm| (lm.mode.clone(), lm.label.clone())) {
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

#[cfg(test)]
mod tests {
    use super::pid_names_managed_server;

    /// The kill guard must fire only on a real llama-server or mlxcel-server listing.
    #[test]
    fn pid_names_managed_server_matches_only_known_servers() {
        assert!(pid_names_managed_server("llama-server\n"));
        assert!(pid_names_managed_server("/opt/homebrew/bin/llama-server\n"));
        assert!(pid_names_managed_server("LLAMA-SERVER")); // case-insensitive
        assert!(pid_names_managed_server("mlxcel-server\n"));
        assert!(pid_names_managed_server(
            "/Users/x/Library/Caches/unlocr/tools/mlxcel-server/mlxcel-server\n"
        ));
        assert!(pid_names_managed_server("MLXCEL-SERVER")); // case-insensitive
        assert!(!pid_names_managed_server("")); // no such pid
        assert!(!pid_names_managed_server("bash\n"));
        assert!(!pid_names_managed_server("Terminal\n"));
    }
}
