use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use tauri::{AppHandle, Emitter};
use unlocr::{OcrOptions, OutputMode, Progress};

use crate::state::{AppState, Backend, LoadedModel};
use crate::store::{self, JobMetrics, JobOptions};

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

    // Run metadata for the Library run-detail dialog: which engine ran it, the
    // output layout, and wall-clock duration (measured from just before OCR, so it
    // covers rasterize + inference). page_count is filled at finish from out.pages.
    let backend_label = match &lm.backend {
        Backend::Local(_) => "local",
        Backend::Mlx(_) => "mlx",
        Backend::Remote(_) => "remote",
    };
    let output_mode_label = match mode {
        OutputMode::Single => "single",
        OutputMode::Pages => "pages",
        OutputMode::Both => "both",
    };
    let started = std::time::Instant::now();
    let make_metrics = |page_count: Option<u32>| JobMetrics {
        page_count,
        duration_ms: Some(started.elapsed().as_millis() as u64),
        backend: backend_label.to_string(),
        output_mode: output_mode_label.to_string(),
    };

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
        Backend::Mlx(srv) => unlocr::ocr_pages(
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
                let _ =
                    store::finish_job(&j.id, "failed", "", "stopped by user", &make_metrics(None));
                emit_jobs_changed(app);
            }
            return Ok(None);
        }
        Err(e) => {
            let msg = format!("{}: {}", input_path.display(), e);
            if let Some(j) = &job {
                let _ = store::finish_job(&j.id, "failed", "", &msg, &make_metrics(None));
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

    let base_stem = input_path
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
        // Version the stem so re-OCR'ing the same PDF never overwrites a prior
        // run's output (the CLI keeps deterministic overwrite; this is GUI-only).
        // Only the default `{stem}.md` path is versioned; an explicit `out_file`
        // is the name the user chose, so it is left to overwrite as they asked.
        let versioned_stem;
        let stem: &str = if out_file.is_none() {
            versioned_stem = next_free_stem(out_dir, base_stem, mode);
            &versioned_stem
        } else {
            base_stem
        };
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
        let page_count = Some(out.pages.len() as u32);
        let _ = store::finish_job(
            &j.id,
            job_status,
            &job_out,
            &job_err,
            &make_metrics(page_count),
        );
        emit_jobs_changed(app);
    }

    if job_status == "failed" {
        return Err(job_err);
    }

    Ok(Some(()))
}

/// Pick an output stem that does not collide with an existing run's output in
/// `out_dir`, so re-OCR'ing the same PDF versions (`foo`, `foo-2`, `foo-3`, ...)
/// instead of overwriting. The collision target depends on `mode`: Single/Both
/// write `{stem}.md`, Pages (and Both) write a `{stem}/` folder, so a free stem
/// must clear whichever the mode uses. Returns the base stem when nothing exists.
///
// ponytail: TOCTOU-racy scan (a stem free at check time could be taken before the
// write), but GUI runs are serialized (one warm model, sequential batch), so two
// runs never race the same out_dir. Per-run-id naming would be the fix if that
// changes.
fn next_free_stem(out_dir: &Path, base: &str, mode: OutputMode) -> String {
    let wants_file = matches!(mode, OutputMode::Single | OutputMode::Both);
    let wants_folder = matches!(mode, OutputMode::Pages | OutputMode::Both);
    let is_free = |stem: &str| {
        let file_ok = !wants_file || !out_dir.join(format!("{stem}.md")).exists();
        let folder_ok = !wants_folder || !out_dir.join(stem).exists();
        file_ok && folder_ok
    };
    if is_free(base) {
        return base.to_string();
    }
    // Start at -2 (the base is "-1"); cap the scan defensively so a pathological
    // dir can never spin forever.
    for n in 2..100_000u32 {
        let candidate = format!("{base}-{n}");
        if is_free(&candidate) {
            return candidate;
        }
    }
    // Fallback: overwrite the base rather than loop (unreachable in practice).
    base.to_string()
}

#[cfg(test)]
mod tests {
    use super::next_free_stem;
    use unlocr::OutputMode;

    #[test]
    fn next_free_stem_empty_dir_returns_base() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(next_free_stem(dir.path(), "foo", OutputMode::Single), "foo");
    }

    #[test]
    fn next_free_stem_single_bumps_on_existing_md() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("foo.md"), b"x").unwrap();
        assert_eq!(
            next_free_stem(dir.path(), "foo", OutputMode::Single),
            "foo-2"
        );
        std::fs::write(dir.path().join("foo-2.md"), b"x").unwrap();
        assert_eq!(
            next_free_stem(dir.path(), "foo", OutputMode::Single),
            "foo-3"
        );
    }

    #[test]
    fn next_free_stem_pages_bumps_on_existing_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("foo")).unwrap();
        // Pages mode collides on the {stem}/ folder, not a .md file.
        assert_eq!(
            next_free_stem(dir.path(), "foo", OutputMode::Pages),
            "foo-2"
        );
        // A stray foo.md alone does not block Pages mode (it wants the folder).
        let dir2 = tempfile::tempdir().unwrap();
        std::fs::write(dir2.path().join("foo.md"), b"x").unwrap();
        assert_eq!(next_free_stem(dir2.path(), "foo", OutputMode::Pages), "foo");
    }

    #[test]
    fn next_free_stem_both_clears_file_and_folder() {
        let dir = tempfile::tempdir().unwrap();
        // Both mode needs BOTH the .md and the folder free; a stray folder bumps it.
        std::fs::create_dir(dir.path().join("foo")).unwrap();
        assert_eq!(next_free_stem(dir.path(), "foo", OutputMode::Both), "foo-2");
    }
}
