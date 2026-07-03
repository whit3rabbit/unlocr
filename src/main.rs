// unlocr: thin CLI wrapping llama.cpp's llama-server to OCR PDFs with the
// Unlimited-OCR (DeepSeek-OCR) model. PDF -> PNG (pdftoppm) -> per-page chat
// completion -> page-delimited markdown.

// The OCR backend (model/pdf/preflight/server) lives in the `unlocr` library
// crate (src/lib.rs). The bin crate is now CLI glue only: Args/clap parsing,
// input expansion, and the bin-only ocr::run_pdf delegator. Using the lib's
// modules keeps one compiled copy of the backend (so a `Server` passed from
// main is the same type the lib's ocr_pages expects) instead of two diverging
// copies compiled into bin and lib separately.
mod cli_args;
mod ocr;

pub use cli_args::*;

use clap::Parser;
use std::path::PathBuf;
use std::process::ExitCode;

use unlocr::inputs::expand_inputs;
use unlocr::{model, preflight, server, OcrOptions};

/// Result type alias with a dynamic error type.
pub type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Res<()> {
    let mut args = Args::parse();

    // --gpu is sugar over the remote-endpoint path: a local vLLM serving the full
    // DeepSeek-OCR model. From here on the normal remote path handles it; no
    // GPU-specific code below.
    args.apply_gpu_defaults();

    if let Some(cmd) = args.command {
        match cmd {
            Commands::Doctor {
                llama_bin,
                model_dir,
                quant,
            }
            | Commands::Preflight {
                llama_bin,
                model_dir,
                quant,
            } => {
                preflight::run_doctor(llama_bin.as_deref(), model_dir, &quant)?;
            }
        }
        return Ok(());
    }

    // Reject out-of-range numerics before any download/spawn, via the single
    // shared lib sink (the same `OcrOptions::validate` the GUI runs in
    // validation.rs, so the guard logic lives in one place). A throwaway
    // OcrOptions carries just the guarded fields; `resolved_pages()` resolves
    // the page range here so a bad --pages also fails fast, off the slow path.
    OcrOptions {
        dpi: args.dpi,
        max_tokens: args.max_tokens,
        image_max_tokens: args.image_max_tokens,
        repeat_penalty: args.repeat_penalty,
        dry_multiplier: args.dry_multiplier,
        dry_base: args.dry_base,
        pages: args.resolved_pages()?,
        ..OcrOptions::default()
    }
    .validate()?;

    // Expand folders, globs, and --from-list into a concrete, deduped PDF list.
    let inputs = expand_inputs(&args.inputs, args.from_list.as_deref(), args.recursive)?;

    // --output names one file; it is ambiguous across a batch. Reject before any
    // download/spawn. Covers both the local and remote paths (both share `inputs`).
    if args.output.is_some() && inputs.len() > 1 {
        return Err("--output names a single file; use --out <DIR> for multiple inputs".into());
    }

    // --model/--mmproj select a local GGUF to spawn llama-server with; remote mode
    // has no local model to load. Reject before the remote return rather than
    // silently ignoring them. Checked here so both the local and remote paths share it.
    if args.endpoint.is_some() && (args.model.is_some() || args.mmproj.is_some()) {
        return Err("--model/--mmproj are local-only; remove them when using --endpoint".into());
    }

    // Remote endpoint mode: rasterize locally, OCR against a remote
    // OpenAI-compatible server. No local llama-server spawn, no model download.
    if let Some(base_url) = args.endpoint.clone() {
        return run_remote(base_url, &inputs, &args);
    }

    // Local GGUF path only (remote returned above): default the repeat penalty to
    // 1.15 so the stock quants do not fall into infinite-loop output on dense
    // pages. An explicit --repeat-penalty wins; remote/--gpu (full-precision
    // vLLM) is left untouched since it does not exhibit the quant loop.
    args.repeat_penalty = args.repeat_penalty.or(Some(1.15));
    // Same gating for the DRY sampler: every local GGUF (any quant) gets 1.0 by
    // default because the loop-preventing ngram processor the upstream Python
    // wrapper relies on does not ship in the GGUF; DRY is llama.cpp's analog.
    // An explicit --dry-multiplier (including 0 = off) wins. --dry-base has no
    // injected default (opt-in only, server default 1.75 applies when unset).
    args.dry_multiplier = args.dry_multiplier.or(Some(1.0));

    // --mmproj alone is meaningless: it overrides the projector for a custom model,
    // but without --model the stock model + stock projector are the matched pair.
    // Checked before preflight so it fails fast without needing llama-server present.
    if args.mmproj.is_some() && args.model.is_none() {
        return Err("--mmproj requires --model".into());
    }

    // 1. Preflight: locate external binaries and validate the llama.cpp build.
    let tools = preflight::check(args.llama_bin.as_deref())?;

    // 2. Ensure model + projector are present (download from HF if missing).
    // Explicit --quant wins; otherwise --quality maps to a quant.
    let quant = args
        .quant
        .clone()
        .unwrap_or_else(|| args.quality.quant().to_string());
    let cache = model::cache_dir(args.model_dir.clone())?;
    // Custom-GGUF mode: route through ensure_with_overrides so override paths are
    // used verbatim (existence-checked in model.rs). The custom model is never
    // downloaded; at most the stock mmproj is fetched here. ensure_with_overrides
    // emits Progress::Download with a concrete pct, so print percent ticks (no
    // separate "downloading <name> ..." header line, the one cosmetic difference
    // from model::ensure's CLI output).
    let files = if args.model.is_some() {
        let mut on_progress = |p: unlocr::Progress| {
            if let unlocr::Progress::Download {
                pct, done, total, ..
            } = p
            {
                use std::io::Write;
                print!("\r  {pct:>3}%  ({} / {} MiB)", done >> 20, total >> 20);
                let _ = std::io::stdout().flush();
            }
        };
        model::ensure_with_overrides(
            &cache,
            &quant,
            args.model.as_deref(),
            args.mmproj.as_deref(),
            &mut on_progress,
        )?
    } else {
        model::ensure(&cache, &quant)?
    };

    std::fs::create_dir_all(&args.out)?;

    // 3. Start llama-server once; it stays up for every page of every PDF.
    // Pass the raw port (0 = auto) so Server::start owns free-port resolution and
    // the bind-race retry; read the actual bound port back from srv.port.
    let srv = server::Server::start(
        &tools.llama_server,
        &files.model,
        &files.mmproj,
        args.port,
        args.image_max_tokens,
        args.chat_template.as_deref(),
    )?;
    let port = srv.port;
    println!("llama-server ready on 127.0.0.1:{port}");

    // 4. OCR each PDF.
    for stem in unlocr::duplicate_stems(&inputs) {
        eprintln!(
            "warning: multiple inputs share the stem '{stem}'; their outputs overwrite \
             each other in {}",
            args.out.display()
        );
    }
    let mut failures = 0;
    for input in &inputs {
        if let Err(e) = ocr::run_pdf(&srv, &tools.pdftoppm, input, &args) {
            eprintln!("error: {}: {e}", input.display());
            failures += 1;
        }
    }

    drop(srv); // explicit: kill llama-server before returning
    if failures > 0 {
        return Err(format!("{failures} input(s) failed").into());
    }
    Ok(())
}

/// OCR every input against a remote OpenAI-compatible endpoint. Pages are still
/// rasterized locally (pdftoppm), so this only skips the llama-server spawn and
/// the model download; --quant/--quality/--llama-bin/--port are inert here.
fn run_remote(base_url: String, inputs: &[PathBuf], args: &Args) -> Res<()> {
    // Only the rasterizer is needed locally; no llama-server, no GGUF.
    let pdftoppm = preflight::pdftoppm()?;

    // Key precedence: --endpoint-key, then UNLOCR_API_KEY. Prefer the env var so
    // the secret stays out of the process list / shell history.
    let api_key = args
        .endpoint_key
        .clone()
        .or_else(|| std::env::var("UNLOCR_API_KEY").ok());

    eprintln!(
        "warning: remote endpoint mode. Unlimited-OCR / DeepSeek-OCR is only known to run on \
         llama.cpp (PR #17400), vLLM, and SGLang. Ollama / LM Studio do not support these \
         OCR models; gateways (litellm/vLLM) need --endpoint-model set to the served name."
    );

    let endpoint = server::RemoteEndpoint {
        base_url,
        api_key,
        model: args.endpoint_model.clone(),
    };

    // Soft reachability check: some servers omit /v1/models, so warn but proceed.
    if let Err(e) = endpoint.probe() {
        eprintln!(
            "warning: could not reach {} (/v1/models): {e}. Proceeding anyway.",
            endpoint.base_url
        );
    }

    std::fs::create_dir_all(&args.out)?;
    println!("using remote endpoint {}", endpoint.base_url);

    for stem in unlocr::duplicate_stems(inputs) {
        eprintln!(
            "warning: multiple inputs share the stem '{stem}'; their outputs overwrite \
             each other in {}",
            args.out.display()
        );
    }
    let mut failures = 0;
    for input in inputs {
        if let Err(e) = ocr::run_pdf(&endpoint, &pdftoppm, input, args) {
            eprintln!("error: {}: {e}", input.display());
            failures += 1;
        }
    }
    if failures > 0 {
        return Err(format!("{failures} input(s) failed").into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
