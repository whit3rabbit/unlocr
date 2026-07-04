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
        dry_multiplier: args.dry_multiplier,
        dry_base: args.dry_base,
        temperature: args.temperature,
        pages: args.resolved_pages()?,
    };

    let input_display = input.display().to_string();
    let mut header_printed = false;
    // A PDF small/fast enough that no Rasterizing tick ever printed keeps the
    // original output byte-identical (no stray leading blank line).
    let mut rasterizing_printed = false;
    // Progress closure reproduces the original CLI output for the common case:
    //   "<input>: N page(s)\n" before the first page line, then
    //   "\r  page i/N" per page. A live "\r  rasterizing i[/N]" line (from
    //   pdftoppm's poll-based progress) may print before that, in which case a
    //   leading newline separates it from the header.
    let mut on_progress = |p: Progress| match p {
        Progress::Rasterizing { page, total } => {
            match total {
                Some(total) => print!("\r  rasterizing {page}/{total}"),
                None => print!("\r  rasterizing {page}"),
            }
            rasterizing_printed = true;
            let _ = std::io::stdout().flush();
        }
        Progress::Page { page, total } => {
            if !header_printed {
                if rasterizing_printed {
                    println!();
                }
                println!("{input_display}: {total} page(s)");
                header_printed = true;
            }
            print!("\r  page {page}/{total}");
            let _ = std::io::stdout().flush();
        }
        Progress::Truncated { page } => {
            // The GGUF-quant repetition-loop framing + remedies only apply to the
            // local backend; --endpoint/--gpu (remote, full-precision) does not
            // exhibit the quant loop (see main.rs's repeat_penalty comment), and
            // --dry-multiplier/--quality are meaningless/inert there, so a remote
            // run gets a neutral "hit the token limit" notice instead.
            if args.endpoint.is_none() {
                eprintln!(
                    "\nwarning: page {page} hit max_tokens without a natural stop (likely a \
                     repetition loop, a known Unlimited-OCR/DeepSeek-OCR failure mode on blank/\
                     ruled/low-content input); consider raising --repeat-penalty/--dry-multiplier \
                     or a higher-precision --quality"
                );
            } else {
                eprintln!(
                    "\nwarning: page {page} hit max_tokens without a natural stop; the page's \
                     output may be incomplete. If this page is legitimately dense, consider \
                     raising --max-tokens"
                );
            }
        }
        _ => {}
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
