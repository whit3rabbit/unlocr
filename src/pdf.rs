// Rasterize a PDF to one PNG per page using pdftoppm, returned in page order.

use crate::Res;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run `pdftoppm -png -r <dpi> <pdf> <outdir>/page` and collect the PNGs sorted
/// by page number. pdftoppm zero-pads the suffix based on page count, so we
/// sort by the parsed trailing integer rather than lexically.
pub fn rasterize(pdftoppm: &Path, pdf: &Path, outdir: &Path, dpi: u32) -> Res<Vec<PathBuf>> {
    let prefix = outdir.join("page");
    // pdftoppm has no `--` end-of-options guard, so a relative path that begins
    // with `-` (e.g. "-foo.pdf") would be parsed as a flag. Prefix "./" to keep it
    // a positional argument. Absolute paths start with `/` and are unaffected.
    let pdf_arg: PathBuf = match pdf.to_str() {
        Some(s) if s.starts_with('-') => Path::new(".").join(pdf),
        _ => pdf.to_path_buf(),
    };
    let status = Command::new(pdftoppm)
        .arg("-png")
        .arg("-r").arg(dpi.to_string())
        .arg(&pdf_arg)
        .arg(&prefix)
        .status()?;
    if !status.success() {
        return Err(format!("pdftoppm failed ({status})").into());
    }

    let pages = collect_pages(outdir);
    if pages.is_empty() {
        return Err("pdftoppm produced no pages".into());
    }
    Ok(pages)
}

/// Collect the page PNGs already in `outdir`, sorted by page number. Returns an
/// empty Vec when the dir is missing or holds no `page-N.png` files (used by the
/// preview cache to detect a hit without re-running pdftoppm).
pub fn collect_pages(outdir: &Path) -> Vec<PathBuf> {
    let mut pages: Vec<(u64, PathBuf)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(outdir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("png") {
                if let Some(n) = trailing_number(&path) {
                    pages.push((n, path));
                }
            }
        }
    }
    pages.sort_by_key(|(n, _)| *n);
    pages.into_iter().map(|(_, p)| p).collect()
}

/// Pull the trailing integer out of a stem like "page-12" -> 12.
fn trailing_number(path: &Path) -> Option<u64> {
    let stem = path.file_stem()?.to_str()?;
    let digits: String = stem.chars().rev().take_while(|c| c.is_ascii_digit()).collect();
    digits.chars().rev().collect::<String>().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::trailing_number;
    use std::path::Path;

    #[test]
    fn extracts_page_number() {
        assert_eq!(trailing_number(Path::new("/tmp/page-1.png")), Some(1));
        assert_eq!(trailing_number(Path::new("/tmp/page-12.png")), Some(12));
        assert_eq!(trailing_number(Path::new("/tmp/page-007.png")), Some(7));
        assert_eq!(trailing_number(Path::new("/tmp/nope.png")), None);
    }

    // EH-0002 acceptance evidence: prove the `dpi` parameter actually reaches
    // pdftoppm (i.e. OcrOptions.dpi -> ocr_pages -> rasterize is honored), without
    // the network/model path. Rasterize the same fixture at two DPIs and assert
    // the PNG pixel dimensions scale ~proportionally with the ratio. A 100x100pt
    // MediaBox at 72dpi is ~100px; at 144dpi it should be ~200px. We allow a few
    // px of pdftoppm rounding. This is the "PNG dimensions scale with the new
    // DPI" check from the card, made runnable on any host with pdftoppm.
    #[test]
    fn rasterize_dpi_scales_png_dimensions() {
        if std::process::Command::new("pdftoppm")
            .arg("-v")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            eprintln!("skipping rasterize_dpi_scales_png_dimensions: pdftoppm not on PATH");
            return;
        }

        let pdf_bytes = minimal_two_page_pdf();
        let pdf_dir = tempfile::tempdir().expect("tmp pdf dir");
        let pdf_path = pdf_dir.path().join("fixture.pdf");
        std::fs::write(&pdf_path, &pdf_bytes).expect("write fixture pdf");

        let pdftoppm = Path::new("pdftoppm");

        let out72 = tempfile::tempdir().expect("tmp 72 dir");
        let pages72 = super::rasterize(pdftoppm, &pdf_path, out72.path(), 72)
            .expect("rasterize at 72 dpi");
        let out144 = tempfile::tempdir().expect("tmp 144 dir");
        let pages144 = super::rasterize(pdftoppm, &pdf_path, out144.path(), 144)
            .expect("rasterize at 144 dpi");

        let (w72, h72) = png_dimensions(&pages72[0]).expect("read 72dpi png dims");
        let (w144, h144) = png_dimensions(&pages144[0]).expect("read 144dpi png dims");

        // 100x100pt page: 72dpi -> ~100px, 144dpi -> ~200px. Allow rounding slack.
        assert!(
            w144 > w72 && (w144 as f64 / w72 as f64) > 1.8,
            "width did not scale with dpi: 72dpi w={w72}, 144dpi w={w144}"
        );
        assert!(
            h144 > h72 && (h144 as f64 / h72 as f64) > 1.8,
            "height did not scale with dpi: 72dpi h={h72}, 144dpi h={h144}"
        );
    }

    // Read (width, height) from a PNG's IHDR chunk. Bytes 16..20 = width,
    // 20..24 = height (big-endian u32), no image crate needed.
    fn png_dimensions(path: &Path) -> Option<(u32, u32)> {
        let bytes = std::fs::read(path).ok()?;
        if bytes.len() < 24 || &bytes[0..8] != b"\x89PNG\r\n\x1a\n" {
            return None;
        }
        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        Some((w, h))
    }

    // Bite 4 lock: exercise `rasterize` end-to-end on a generated 2-page PDF
    // without touching the network. This is the non-network piece of run_ocr_job
    // (which delegates rasterize+OCR to ocr_pages -> pdf::rasterize). pdftoppm is
    // an unbundled runtime dep, so we skip on hosts without it rather than fail.
    #[test]
    fn rasterize_real_two_page_pdf_in_order() {
        // Only run when pdftoppm is on PATH; matches the CLI's own resolution.
        if std::process::Command::new("pdftoppm")
            .arg("-v")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            eprintln!("skipping rasterize_real_two_page_pdf_in_order: pdftoppm not on PATH");
            return;
        }

        let pdf_bytes = minimal_two_page_pdf();
        let pdf_dir = tempfile::tempdir().expect("tmp pdf dir");
        let pdf_path = pdf_dir.path().join("fixture.pdf");
        std::fs::write(&pdf_path, &pdf_bytes).expect("write fixture pdf");

        let out = tempfile::tempdir().expect("tmp page dir");
        let pdftoppm = Path::new("pdftoppm");
        let pages = super::rasterize(pdftoppm, &pdf_path, out.path(), 72)
            .expect("rasterize fixture pdf");

        // 2 pages, sorted ascending, each one rasterizes to a non-empty PNG whose
        // trailing number parses to 1 then 2.
        assert_eq!(pages.len(), 2, "expected 2 pages, got {}", pages.len());
        for (i, p) in pages.iter().enumerate() {
            assert_eq!(p.extension().unwrap(), "png");
            let meta = std::fs::metadata(p).expect("page png metadata");
            assert!(meta.len() > 0, "page {} png is empty", i + 1);
            assert_eq!(trailing_number(p), Some((i + 1) as u64));
        }
    }

    // GUI-12 (EH-0011) acceptance evidence, headless: the preview pane calls
    // `crate::render_pages`, which the Tauri `render_pages` command thinly wraps.
    // Prove that path returns non-empty page PNGs and that a second call is a
    // cache hit (same paths, pdftoppm not required to re-run). Closes the runtime
    // verification gap without a live desktop window. Skips without pdftoppm.
    #[test]
    fn render_pages_returns_nonempty_pngs_and_caches() {
        if std::process::Command::new("pdftoppm")
            .arg("-v")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            eprintln!("skipping render_pages_returns_nonempty_pngs_and_caches: pdftoppm not on PATH");
            return;
        }

        let pdf_dir = tempfile::tempdir().expect("tmp pdf dir");
        let pdf_path = pdf_dir.path().join("fixture.pdf");
        std::fs::write(&pdf_path, minimal_two_page_pdf()).expect("write fixture pdf");

        let cache = tempfile::tempdir().expect("tmp cache dir");
        let pdftoppm = Path::new("pdftoppm");

        let pages = crate::render_pages(pdftoppm, &pdf_path, 72, cache.path())
            .expect("render_pages on fixture");
        assert_eq!(pages.len(), 2, "expected 2 preview PNGs, got {}", pages.len());
        for (i, p) in pages.iter().enumerate() {
            assert_eq!(p.extension().unwrap(), "png");
            assert!(p.exists(), "page {} png missing", i + 1);
            assert!(
                std::fs::metadata(p).expect("png metadata").len() > 0,
                "page {} png is empty",
                i + 1
            );
        }

        // Second call hits the per-PDF preview cache: same paths, no re-render.
        let again = crate::render_pages(pdftoppm, &pdf_path, 72, cache.path())
            .expect("render_pages cache hit");
        assert_eq!(again, pages, "cache hit must return the same page paths");
    }

    // Smallest valid 2-page PDF pdftoppm accepts: a Catalog -> Pages with two
    // pages, each with a short text content stream, plus a valid xref/trailer
    // (poppler 26.06 does not reconstruct a missing xref, so we compute offsets).
    // Kept inline so the test adds no binary fixture to the repo and stays scoped
    // to src/.
    fn minimal_two_page_pdf() -> Vec<u8> {
        let p1 = "<</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Contents 4 0 R/Resources<</Font<</F1 7 0 R>>>>>>";
        let p2 = "<</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Contents 6 0 R/Resources<</Font<</F1 7 0 R>>>>>>";
        let objs: [&str; 7] = [
            "<</Type/Catalog/Pages 2 0 R>>",
            "<</Type/Pages/Kids[3 0 R 5 0 R]/Count 2>>",
            p1,
            "<</Length 38>>stream\nBT /F1 12 Tf 10 80 Td (Page one) Tj ET\nendstream",
            p2,
            "<</Length 38>>stream\nBT /F1 12 Tf 10 80 Td (Page two) Tj ET\nendstream",
            "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>",
        ];

        let mut buf = String::from("%PDF-1.4\n");
        let mut offsets: Vec<usize> = Vec::with_capacity(objs.len());
        for (i, obj) in objs.iter().enumerate() {
            offsets.push(buf.len());
            buf.push_str(&format!("{} 0 obj{}\nendobj\n", i + 1, obj));
        }
        let xref_start = buf.len();
        buf.push_str("xref\n0 8\n");
        buf.push_str("0000000000 65535 f \n");
        for off in &offsets {
            buf.push_str(&format!("{:010} 00000 n \n", off));
        }
        buf.push_str(&format!(
            "trailer<</Size 8/Root 1 0 R>>\nstartxref\n{}\n%%EOF\n",
            xref_start
        ));
        buf.into_bytes()
    }
}
