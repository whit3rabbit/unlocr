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
use unlocr::{ocr_pages, server::Server, OcrOptions, Progress, Res};

pub fn run_pdf(srv: &Server, pdftoppm: &Path, input: &Path, args: &Args, _port: u16) -> Res<()> {
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
        prompt: args.prompt.clone(),
        port: args.port,
        model_dir: args.model_dir.clone(),
        keep_images: args.keep_images,
    };

    let input_display = input.display().to_string();
    let mut header_printed = false;
    // Progress closure reproduces the original CLI output exactly:
    //   "<input>: N page(s)\n" before the first page line, then
    //   "\r  page i/N" per page.
    let mut on_progress = |p: Progress| match p {
        Progress::Page { page, total } => {
            if !header_printed {
                println!("{input_display}: {total} page(s)");
                header_printed = true;
            }
            print!("\r  page {page}/{total}");
            let _ = std::io::stdout().flush();
        }
        _ => {}
    };

    let (md, kept) = ocr_pages(srv, pdftoppm, input, &opts, &mut on_progress)?;
    println!(); // newline after the last "\r  page N/N" line, matching the original

    let out_path = args.out.join(format!("{stem}.md"));
    std::fs::write(&out_path, md)?;
    println!("  wrote {}", out_path.display());

    if let Some(kept_dir) = kept {
        println!("  kept page images in {}", kept_dir.display());
    }
    Ok(())
}
