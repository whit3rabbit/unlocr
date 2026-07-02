// OCR run command and related structures/helpers.

pub(crate) mod types;
pub(crate) mod validation;
pub(crate) mod worker;

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use tauri::{AppHandle, Manager};

use crate::state::{AppState, Backend};
use crate::store::JobOptions;

// Re-export RunResult so main mod.rs continues to work.
pub(crate) use types::RunResult;

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
    dry_multiplier: Option<f32>,
    dry_base: Option<f32>,
    first_page: Option<u32>,
    last_page: Option<u32>,
    quant: Option<String>,
    output_mode: Option<String>,
) -> Result<RunResult, String> {
    let (mut opts, mode) = validation::validate_and_prepare_options(
        &inputs,
        out_file.as_deref(),
        max_tokens,
        dpi,
        prompt,
        keep_images,
        repeat_penalty,
        dry_multiplier,
        dry_base,
        first_page,
        last_page,
        output_mode.as_deref(),
    )?;

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

            // Local GGUF quants fall into infinite-loop output on dense pages; a
            // repeat penalty escapes it. Default to 1.15 when the user left the
            // field blank (None). An explicit value wins; remote (full-precision
            // vLLM) is left alone, it does not exhibit the quant loop.
            if is_local && opts.repeat_penalty.is_none() {
                opts.repeat_penalty = Some(1.15);
            }
            // Same gating for the DRY sampler (any local GGUF quant): it stands in
            // for the loop-preventing ngram processor that does not ship in the
            // GGUF. Explicit value (including 0 = off) wins; remote is left alone
            // (llama.cpp-only field). dry_base has no injected default (opt-in
            // only, server default 1.75 applies when unset).
            if is_local && opts.dry_multiplier.is_none() {
                opts.dry_multiplier = Some(1.0);
            }

            for input in &inputs {
                match worker::process_single_input(
                    &app,
                    &state,
                    input,
                    &opts,
                    &job_opts,
                    mode,
                    &out_dir,
                    out_file.as_deref(),
                    lm,
                    &pdftoppm,
                    &mut results,
                    &mut combined_acc,
                ) {
                    Ok(Some(())) => {}
                    Ok(None) => {
                        stopped = true;
                        break;
                    }
                    Err(e) => {
                        errors.push(e);
                        continue;
                    }
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
