// Drive one PDF end to end by delegating the rasterize+OCR loop to the library
// (`unlocr::ocr_pages`). This module is CLI-only glue: it converts the clap
// `Args` into the clap-free `OcrOptions`, wires a progress closure that
// reproduces the original println/print output byte-for-byte, then writes the
// returned markdown to `{stem}.md` and reports the kept-images path.

use crate::Args;
use std::io::Write;
use std::path::Path;

// `Server`, `ocr_pages`, `OcrOptions`, `Progress` all come from the `unlocr`
// library crate (one compiled backend), so the `Server` main passes in is the
// same type `ocr_pages` expects. `Args` is the bin's own clap struct.
use unlocr::{ocr_pages, server::ImageOcr, OcrOptions, Progress, Res};

/// Generic over the OCR backend (`ImageOcr`): the local `Server` or a
/// `RemoteEndpoint`. `ocr_pages` is already backend-agnostic, so this is just the
/// CLI glue (Args -> OcrOptions, progress println, write `{stem}.md`).
pub fn run_pdf<S: ImageOcr>(backend: &S, pdftoppm: &Path, input: &Path, args: &Args) -> Res<()> {
    if !input.is_file() {
        return Err("not a file".into());
    }
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("bad input filename")?;

    // Derive the clap-free OcrOptions from the clap Args. Explicit --quant wins;
    // otherwise --quality maps to a quant tag. This is the only Args->OcrOptions
    // conversion in the codebase, so the CLI and GUI share one option shape.
    let quant = args
        .quant
        .clone()
        .unwrap_or_else(|| args.quality.quant().to_string());
    let opts = OcrOptions {
        quant,
        max_tokens: args.max_tokens,
        dpi: args.dpi,
        prompt: args.resolved_prompt(),
        port: args.port,
        model_dir: args.model_dir.clone(),
        keep_images: args.keep_images,
        image_max_tokens: args.image_max_tokens,
        chat_template: args.chat_template.clone(),
        repeat_penalty: args.repeat_penalty,
        pages: args.resolved_pages()?,
    };

    let input_display = input.display().to_string();
    let mut header_printed = false;
    // Progress closure reproduces the original CLI output exactly:
    //   "<input>: N page(s)\n" before the first page line, then
    //   "\r  page i/N" per page.
    let mut on_progress = |p: Progress| {
        if let Progress::Page { page, total } = p {
            if !header_printed {
                println!("{input_display}: {total} page(s)");
                header_printed = true;
            }
            print!("\r  page {page}/{total}");
            let _ = std::io::stdout().flush();
        }
    };

    let out = ocr_pages(backend, pdftoppm, input, &opts, &mut on_progress, &|| false)?;
    println!(); // newline after the last "\r  page N/N" line, matching the original

    // --output/-o names a single file; it is meaningless in `pages` mode (which
    // writes a per-page folder named after the input stem). Warn rather than
    // silently ignore so the user sees the flag had no effect on the folder name.
    let mode = args.output_mode.to_mode();
    if args.output.is_some() && matches!(mode, unlocr::OutputMode::Pages) {
        eprintln!("  warning: --output is ignored in pages mode; folder uses the input stem");
    }

    // Write via the shared lib helper (single/pages/both). It creates parent dirs,
    // so the explicit create_dir_all the old path did now lives inside the helper.
    let paths = unlocr::write_markdown_output(mode, &args.out, args.output.as_deref(), stem, &out)?;
    for p in &paths {
        println!("  wrote {}", p.display());
    }

    if let Some(kept_dir) = &out.kept_images {
        println!("  kept page images in {}", kept_dir.display());
    }
    Ok(())
}
