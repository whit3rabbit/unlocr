use std::path::PathBuf;
use unlocr::{OcrOptions, OutputMode};

/// Validates the OCR arguments, sets up the default options, duplicate stem checks,
/// and determines the output mode.
#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_and_prepare_options(
    inputs: &[String],
    out_file: Option<&str>,
    max_tokens: Option<u32>,
    dpi: Option<u32>,
    prompt: Option<String>,
    keep_images: Option<bool>,
    repeat_penalty: Option<f32>,
    first_page: Option<u32>,
    last_page: Option<u32>,
    output_mode: Option<&str>,
) -> Result<(OcrOptions, OutputMode), String> {
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
    opts.pages = match (first_page, last_page) {
        (None, None) => None,
        (f, l) => Some((f.unwrap_or(1), l)),
    };

    // Single shared numeric/range guard: the same `OcrOptions::validate` the CLI
    // runs in main.rs, so the checks (and their wording) live in one place
    // rather than drifting between the two front ends. `image_max_tokens` is
    // None here (it is a load-time flag, validated in `load_model`); validate()
    // covers dpi / max_tokens / repeat_penalty and the page range.
    opts.validate().map_err(|e| e.to_string())?;

    if out_file.is_some() && inputs.len() > 1 {
        return Err("out_file names a single file; clear it for multiple inputs".to_string());
    }

    // Resolve the on-disk layout once (single/pages/both); Copy, so the move
    // closure captures it by value. Unknown string -> error before any spawn.
    let mode =
        unlocr::parse_output_mode(output_mode.unwrap_or("single")).map_err(|e| e.to_string())?;

    // Pages/Both name the output folder after the input stem; out_file is ignored
    // there. Warn (parity with the CLI's ocr::run_pdf) so a set filename that has
    // no effect is surfaced rather than silently dropped.
    if out_file.is_some() && matches!(mode, OutputMode::Pages | OutputMode::Both) {
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

    Ok((opts, mode))
}
