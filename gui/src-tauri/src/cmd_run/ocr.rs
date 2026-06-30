// OCR run command and related structures/helpers.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use unlocr::{OcrOptions, Progress};

use crate::state::{AppState, Backend};
use crate::store::{self, JobOptions};

/// Best-effort notify the webview that the job store changed so the Library + Board reload live.
fn emit_jobs_changed(app: &AppHandle) {
    let _ = app.emit("jobs://changed", ());
}

/// Serializable payload for the `ocr://page` event.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PageProgress {
    page: usize,
    total: usize,
}

/// Payload for the `ocr://partial-text` event.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PartialText {
    page: usize,
    chunk: String,
}

/// Payload for the terminal `ocr://done` event.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OcrDone {
    markdown: String,
}

/// Payload for `ocr://images-kept`.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImagesKept {
    dir: String,
}

/// Return value of `run_ocr`: every written file path (combined file first in
/// single/both; first page file in pages) plus the in-memory combined markdown.
/// `combined` lets the frontend render a preview in `pages` mode, where no
/// single combined file exists on disk.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunResult {
    paths: Vec<String>,
    combined: String,
}

/// Run OCR on one or more PDFs using the already-loaded model.
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
    quant: Option<String>,
    output_mode: Option<String>,
) -> Result<RunResult, String> {
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
    opts.repeat_penalty = repeat_penalty;

    if opts.dpi == 0 {
        return Err("dpi must be greater than 0".to_string());
    }
    if opts.max_tokens == 0 {
        return Err("max_tokens must be greater than 0".to_string());
    }
    if let Some(rp) = opts.repeat_penalty {
        if !rp.is_finite() || rp <= 0.0 {
            return Err("repeat_penalty must be a finite value greater than 0".to_string());
        }
    }
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

    if out_file.is_some() && inputs.len() > 1 {
        return Err("out_file names a single file; clear it for multiple inputs".to_string());
    }

    // Resolve the on-disk layout once (single/pages/both); Copy, so the move
    // closure captures it by value. Unknown string -> error before any spawn.
    let mode = unlocr::parse_output_mode(output_mode.as_deref().unwrap_or("single"))
        .map_err(|e| e.to_string())?;

    // Pages/Both name the output folder after the input stem; out_file is ignored
    // there. Warn (parity with the CLI's ocr::run_pdf) so a set filename that has
    // no effect is surfaced rather than silently dropped.
    if out_file.is_some() && matches!(mode, unlocr::OutputMode::Pages | unlocr::OutputMode::Both) {
        eprintln!(
            "[run_ocr] warning: out_file is ignored in pages/both mode; the folder uses the input stem"
        );
    }
    // Same-stem inputs collide on the shared out dir: the `{stem}.md` file or the
    // `{stem}/` pages folder. A later input silently overwrites an earlier one, so
    // warn before running.
    for stem in unlocr::duplicate_stems(&inputs.iter().map(PathBuf::from).collect::<Vec<_>>()) {
        eprintln!(
            "[run_ocr] warning: multiple inputs share the stem '{stem}'; their outputs overwrite each other"
        );
    }

    #[cfg(debug_assertions)]
    eprintln!(
        "[run_ocr] effective opts: dpi={} max_tokens={} keep_images={}",
        opts.dpi, opts.max_tokens, opts.keep_images
    );

    let out_dir = PathBuf::from(out_dir);

    {
        let state = app.state::<AppState>();
        state.cancel.store(false, Ordering::SeqCst);
        *state.last_used.lock().unwrap_or_else(|p| p.into_inner()) =
            Some(std::time::Instant::now());
    }

    let app_for_join = app.clone();
    let res = tauri::async_runtime::spawn_blocking(move || -> Result<RunResult, String> {
        let state = app.state::<AppState>();

        let pdftoppm = state
            .pdftoppm
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or("load a model first")?;

        let mut results = Vec::with_capacity(inputs.len());
        // Accumulates the combined markdown of the processed input(s) so the
        // frontend can render a preview in `pages` mode (no combined file there).
        let mut combined_acc = String::new();
        let mut errors = Vec::new();
        let mut stopped = false;

        let job_opts = JobOptions::from_opts(
            quant.as_deref().unwrap_or(&opts.quant),
            opts.max_tokens,
            opts.dpi,
            &opts.prompt,
            opts.keep_images,
        );

        let is_local;
        {
            let guard = state.model.lock().unwrap_or_else(|p| p.into_inner());
            let lm = guard.as_ref().ok_or("load a model first")?;
            is_local = matches!(&lm.backend, Backend::Local(_));

            for input in &inputs {
                let input_path = PathBuf::from(input);

                let job = match store::start_job(input, job_opts.clone()) {
                    Ok(j) => Some(j),
                    Err(e) => {
                        eprintln!("[run_ocr] start_job failed for {input}: {e}");
                        None
                    }
                };
                emit_jobs_changed(&app);

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
                let out = match outcome {
                    Ok(v) => v,
                    Err(e) if state.cancel.load(Ordering::SeqCst) => {
                        let _ = e;
                        if let Some(j) = &job {
                            let _ = store::finish_job(&j.id, "failed", "", "stopped by user");
                            emit_jobs_changed(&app);
                        }
                        stopped = true;
                        break;
                    }
                    Err(e) => {
                        let msg = format!("{}: {}", input_path.display(), e);
                        if let Some(j) = &job {
                            let _ = store::finish_job(&j.id, "failed", "", &msg);
                            emit_jobs_changed(&app);
                        }
                        errors.push(msg);
                        continue;
                    }
                };

                if let Some(dir) = out.kept_images.as_ref() {
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
                        markdown: out.combined.clone(),
                    },
                );

                let stem = input_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("output");

                let (job_status, job_out, job_err): (&str, String, String) =
                    if out_dir.as_os_str().is_empty() && out_file.is_none() {
                        // In-memory fallback (the frontend normally guarantees a
                        // non-empty out_dir): nothing written; carry the combined
                        // text so the review pane can still preview it.
                        combined_acc = out.combined.clone();
                        ("done", String::new(), String::new())
                    } else {
                        match unlocr::write_markdown_output(
                            mode,
                            &out_dir,
                            out_file.as_deref().map(Path::new),
                            stem,
                            &out,
                        ) {
                            Ok(paths) => {
                                // Allowlist every written file (single: 1, pages: N,
                                // both: N+1) so read_text_file can serve any of them.
                                // read_text_file canonicalizes its argument and checks
                                // exact membership, so store canonicalized abs paths.
                                let cwd = std::env::current_dir().ok();
                                let abs_strings: Vec<String> = paths
                                    .iter()
                                    .map(|p| {
                                        if p.is_absolute() {
                                            p.display().to_string()
                                        } else {
                                            cwd.clone()
                                                .map(|c| c.join(p))
                                                .unwrap_or_else(|| p.clone())
                                                .display()
                                                .to_string()
                                        }
                                    })
                                    .collect();
                                if let Ok(mut g) = state.read_allow.lock() {
                                    for s in &abs_strings {
                                        let canon = std::fs::canonicalize(s)
                                            .unwrap_or_else(|_| PathBuf::from(s));
                                        g.insert(canon);
                                    }
                                }
                                // Primary path for the job row: combined file first
                                // (single/both) or the first page file (pages).
                                let primary = abs_strings.first().cloned().unwrap_or_default();
                                results.extend(abs_strings);
                                combined_acc = out.combined.clone();
                                ("done", primary, String::new())
                            }
                            Err(e) => {
                                let msg = format!("failed to write output: {e}");
                                errors.push(msg.clone());
                                ("failed", String::new(), msg)
                            }
                        }
                    };

                if let Some(j) = &job {
                    let _ = store::finish_job(&j.id, job_status, &job_out, &job_err);
                    emit_jobs_changed(&app);
                }
            }
        }

        if stopped {
            *state.model.lock().unwrap_or_else(|p| p.into_inner()) = None;
            *state.server_pid.lock().unwrap_or_else(|p| p.into_inner()) = None;
            return Err("stopped".to_string());
        }
        let tail_stop = state.cancel.swap(false, Ordering::SeqCst);
        if tail_stop && is_local {
            *state.model.lock().unwrap_or_else(|p| p.into_inner()) = None;
            *state.server_pid.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }

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

        Ok(RunResult {
            paths: results,
            combined: combined_acc,
        })
    })
    .await;
    {
        let state = app_for_join.state::<AppState>();
        state.invalidate_job_outputs();
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
