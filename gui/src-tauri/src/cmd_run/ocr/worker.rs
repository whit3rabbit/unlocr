use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use tauri::{AppHandle, Emitter};
use unlocr::{OcrOptions, OutputMode, Progress};

use crate::state::{AppState, Backend, LoadedModel};
use crate::store::{self, JobOptions};

use super::types::{
    ImagesKept, OcrDone, PageProgress, PartialText, RasterizeProgress, StatusMsg, Truncated,
};

/// Best-effort notify the webview that the job store changed so the Library + Board reload live.
pub(crate) fn emit_jobs_changed(app: &AppHandle) {
    let _ = app.emit("jobs://changed", ());
}

/// Runs OCR on a single input PDF, updates the job store, and emits progress events.
/// Returns Ok(None) if the operation was stopped/cancelled by the user.
/// Returns Ok(Some(())) on success.
/// Returns Err(String) on failure.
#[allow(clippy::too_many_arguments)]
pub(crate) fn process_single_input(
    app: &AppHandle,
    state: &AppState,
    input: &str,
    opts: &OcrOptions,
    job_opts: &JobOptions,
    mode: OutputMode,
    out_dir: &Path,
    out_file: Option<&str>,
    lm: &LoadedModel,
    pdftoppm: &Path,
    results: &mut Vec<String>,
    combined_acc: &mut String,
) -> Result<Option<()>, String> {
    let input_path = PathBuf::from(input);

    let job = match store::start_job(input, job_opts.clone()) {
        Ok(j) => Some(j),
        Err(e) => {
            eprintln!("[run_ocr] start_job failed for {input}: {e}");
            None
        }
    };
    emit_jobs_changed(app);

    const FLUSH_CHARS: usize = 256;
    let app_for_progress = app.clone();
    let mut buf = String::new();
    let mut buf_page = 0usize;
    let mut on_progress = |p: Progress| match p {
        Progress::Rasterizing { page, total } => {
            let _ = app_for_progress.emit("ocr://rasterizing", RasterizeProgress { page, total });
        }
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
        Progress::Truncated { page } => {
            let is_local = matches!(lm.backend, Backend::Local(_));
            let _ = app_for_progress.emit("ocr://page-truncated", Truncated { page, is_local });
        }
        _ => {}
    };

    // ocr_pages rasterizes EVERY page with pdftoppm before the first
    // page event; on a big PDF that is a long silent gap. Tell the popup
    // so it does not look hung on "starting…".
    let _ = app.emit(
        "ocr://status",
        StatusMsg {
            message: "rasterizing pages…".to_string(),
        },
    );

    let should_cancel = || state.cancel.load(Ordering::SeqCst);
    let outcome = match &lm.backend {
        Backend::Local(srv) => unlocr::ocr_pages(
            srv,
            pdftoppm,
            &input_path,
            opts,
            &mut on_progress,
            &should_cancel,
        ),
        Backend::Remote(ep) => unlocr::ocr_pages(
            ep,
            pdftoppm,
            &input_path,
            opts,
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
                emit_jobs_changed(app);
            }
            return Ok(None);
        }
        Err(e) => {
            let msg = format!("{}: {}", input_path.display(), e);
            if let Some(j) = &job {
                let _ = store::finish_job(&j.id, "failed", "", &msg);
                emit_jobs_changed(app);
            }
            return Err(msg);
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

    let (job_status, job_out, job_err): (&str, String, String) = if out_dir.as_os_str().is_empty()
        && out_file.is_none()
    {
        // In-memory fallback (the frontend normally guarantees a
        // non-empty out_dir): nothing written; carry the combined
        // text so the review pane can still preview it.
        *combined_acc = out.combined.clone();
        ("done", String::new(), String::new())
    } else {
        match unlocr::write_markdown_output(mode, out_dir, out_file.map(Path::new), stem, &out) {
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
                        let canon = std::fs::canonicalize(s).unwrap_or_else(|_| PathBuf::from(s));
                        g.insert(canon);
                    }
                }
                // Primary path for the job row: combined file first
                // (single/both) or the first page file (pages).
                let primary = abs_strings.first().cloned().unwrap_or_default();
                results.extend(abs_strings);
                *combined_acc = out.combined.clone();
                ("done", primary, String::new())
            }
            Err(e) => {
                let msg = format!("failed to write output: {e}");
                ("failed", String::new(), msg)
            }
        }
    };

    if let Some(j) = &job {
        let _ = store::finish_job(&j.id, job_status, &job_out, &job_err);
        emit_jobs_changed(app);
    }

    if job_status == "failed" {
        return Err(job_err);
    }

    Ok(Some(()))
}
