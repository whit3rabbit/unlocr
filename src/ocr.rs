// Drive one PDF end to end: rasterize, OCR each page sequentially, write the
// assembled markdown.

use crate::server::Server;
use crate::{pdf, Args, Res};
use base64::Engine;
use std::io::Write;
use std::path::Path;

pub fn run_pdf(srv: &Server, pdftoppm: &Path, input: &Path, args: &Args, _port: u16) -> Res<()> {
    if !input.is_file() {
        return Err("not a file".into());
    }
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("bad input filename")?;

    // Scratch dir for page PNGs. Dropped (deleted) at end of scope unless kept.
    let tmp = tempfile::tempdir()?;
    let pages = pdf::rasterize(pdftoppm, input, tmp.path(), args.dpi)?;
    let n = pages.len();
    println!("{}: {n} page(s)", input.display());

    let mut md = String::new();
    for (i, page) in pages.iter().enumerate() {
        print!("\r  page {}/{n}", i + 1);
        let _ = std::io::stdout().flush();

        let bytes = std::fs::read(page)?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let data_uri = format!("data:image/png;base64,{b64}");
        let text = srv.ocr_image(&args.prompt, &data_uri, args.max_tokens)?;

        md.push_str(&format!("\n\n<!-- page {} -->\n\n", i + 1));
        md.push_str(text.trim());
    }
    println!();

    let out_path = args.out.join(format!("{stem}.md"));
    std::fs::write(&out_path, md.trim_start())?;
    println!("  wrote {}", out_path.display());

    if args.keep_images {
        // Persist the scratch dir by leaking the handle; report where.
        let kept = tmp.keep();
        println!("  kept page images in {}", kept.display());
    }
    Ok(())
}
