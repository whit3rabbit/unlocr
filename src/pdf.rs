// Rasterize a PDF to one PNG per page using pdftoppm, returned in page order.

use crate::Res;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run `pdftoppm -png -r <dpi> <pdf> <outdir>/page` and collect the PNGs sorted
/// by page number. Renders all pages; thin wrapper over `rasterize_range`.
pub fn rasterize(pdftoppm: &Path, pdf: &Path, outdir: &Path, dpi: u32) -> Res<Vec<PathBuf>> {
    rasterize_range(pdftoppm, pdf, outdir, dpi, None)
}

/// Like `rasterize`, but when `range` is `Some((first, last))` (1-based inclusive)
/// passes `-f`/`-l` so pdftoppm renders only that page span. An open upper bound
/// (`last == None`) emits `-f first` with no `-l`, which pdftoppm renders to the
/// end of the document. pdftoppm preserves the real page number in the filename
/// suffix (e.g. `-f 5` -> `page-5.png`), so `collect_pages`/`trailing_number` keep
/// working and the caller can recover the true page number. pdftoppm zero-pads the
/// suffix based on page count, so we sort by the parsed trailing integer rather
/// than lexically.
pub fn rasterize_range(
    pdftoppm: &Path,
    pdf: &Path,
    outdir: &Path,
    dpi: u32,
    range: Option<(u32, Option<u32>)>,
) -> Res<Vec<PathBuf>> {
    let prefix = outdir.join("page");
    // pdftoppm has no `--` end-of-options guard, so a relative path that begins
    // with `-` (e.g. "-foo.pdf") would be parsed as a flag. Prefix "./" to keep it
    // a positional argument. Absolute paths start with `/` and are unaffected.
    let pdf_arg: PathBuf = match pdf.to_str() {
        Some(s) if s.starts_with('-') => Path::new(".").join(pdf),
        _ => pdf.to_path_buf(),
    };
    let mut cmd = Command::new(pdftoppm);
    cmd.arg("-png").arg("-r").arg(dpi.to_string());
    if let Some((first, last)) = range {
        cmd.arg("-f").arg(first.to_string());
        // Open upper bound (last == None) omits -l: pdftoppm renders to EOF.
        if let Some(last) = last {
            cmd.arg("-l").arg(last.to_string());
        }
    }
    let status = cmd.arg(&pdf_arg).arg(&prefix).status()?;
    if !status.success() {
        return Err(format!("pdftoppm failed ({status})").into());
    }

    let pages = collect_pages(outdir);
    // An empty result means pdftoppm ran cleanly but emitted nothing (the
    // requested page/range is past EOF). That is a value, not an error: callers
    // decide what an empty page set means (render_page -> out of range, ocr_pages
    // -> explicit "produced no pages"). Real failures (non-zero exit, spawn,
    // malformed PDF) already returned Err above.
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

/// Pull the trailing integer out of a stem like "page-12" -> 12. Pub(crate) so
/// `render_page` (lib.rs) can pick a specific page file out of a populated preview
/// cache dir (pdftoppm zero-pads the suffix by total page count, so the exact
/// filename is not predictable; match by parsed page number instead).
pub(crate) fn trailing_number(path: &Path) -> Option<u64> {
    let stem = path.file_stem()?.to_str()?;
    let digits: String = stem
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect();
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
        let pages72 =
            super::rasterize(pdftoppm, &pdf_path, out72.path(), 72).expect("rasterize at 72 dpi");
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
        let pages =
            super::rasterize(pdftoppm, &pdf_path, out.path(), 72).expect("rasterize fixture pdf");

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

    // Open upper bound (`Some((first, None))`) must render `first..EOF`, not a
    // single page: this is the fix for the GUI "pages N-end" path that previously
    // collapsed to one page. On the 2-page fixture, `(2, None)` yields exactly page
    // 2 (the last), and `(1, None)` yields both, proving `-f first` with no `-l`
    // reaches the end of the document. Skips without pdftoppm.
    #[test]
    fn rasterize_open_upper_bound_renders_to_end() {
        if std::process::Command::new("pdftoppm")
            .arg("-v")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            eprintln!("skipping rasterize_open_upper_bound_renders_to_end: pdftoppm not on PATH");
            return;
        }

        let pdf_dir = tempfile::tempdir().expect("tmp pdf dir");
        let pdf_path = pdf_dir.path().join("fixture.pdf");
        std::fs::write(&pdf_path, minimal_two_page_pdf()).expect("write fixture pdf");
        let pdftoppm = Path::new("pdftoppm");

        // (2, None) = from page 2 to end -> just page 2 on a 2-page PDF.
        let out_tail = tempfile::tempdir().expect("tmp tail dir");
        let tail =
            super::rasterize_range(pdftoppm, &pdf_path, out_tail.path(), 72, Some((2, None)))
                .expect("rasterize open tail");
        assert_eq!(
            tail.len(),
            1,
            "expected 1 page from (2, None), got {}",
            tail.len()
        );
        assert_eq!(trailing_number(&tail[0]), Some(2));

        // (1, None) = whole document -> both pages.
        let out_all = tempfile::tempdir().expect("tmp all dir");
        let all = super::rasterize_range(pdftoppm, &pdf_path, out_all.path(), 72, Some((1, None)))
            .expect("rasterize open all");
        assert_eq!(
            all.len(),
            2,
            "expected 2 pages from (1, None), got {}",
            all.len()
        );
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
            eprintln!(
                "skipping render_pages_returns_nonempty_pngs_and_caches: pdftoppm not on PATH"
            );
            return;
        }

        let pdf_dir = tempfile::tempdir().expect("tmp pdf dir");
        let pdf_path = pdf_dir.path().join("fixture.pdf");
        std::fs::write(&pdf_path, minimal_two_page_pdf()).expect("write fixture pdf");

        let cache = tempfile::tempdir().expect("tmp cache dir");
        let pdftoppm = Path::new("pdftoppm");

        let pages = crate::render_pages(pdftoppm, &pdf_path, 72, cache.path())
            .expect("render_pages on fixture");
        assert_eq!(
            pages.len(),
            2,
            "expected 2 preview PNGs, got {}",
            pages.len()
        );
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

    // Lazy preview path: `render_page` renders ONE page on demand (the GUI no longer
    // rasterizes every page on import). Prove it returns the requested page, reuses
    // the cache, and Errs for an out-of-range page (the frontend's end-of-doc signal).
    #[test]
    fn render_page_single_and_rejects_out_of_range() {
        if std::process::Command::new("pdftoppm")
            .arg("-v")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            eprintln!("skipping render_page_single_and_rejects_out_of_range: pdftoppm not on PATH");
            return;
        }

        let pdf_dir = tempfile::tempdir().expect("tmp pdf dir");
        let pdf_path = pdf_dir.path().join("fixture.pdf");
        std::fs::write(&pdf_path, minimal_two_page_pdf()).expect("write fixture pdf");

        let cache = tempfile::tempdir().expect("tmp cache dir");
        let pdftoppm = Path::new("pdftoppm");

        let p1 = crate::render_page(pdftoppm, &pdf_path, 72, cache.path(), 1).expect("page 1");
        assert_eq!(
            trailing_number(&p1),
            Some(1),
            "page 1 file should be page-1"
        );
        assert!(p1.exists() && std::fs::metadata(&p1).unwrap().len() > 0);

        let p2 = crate::render_page(pdftoppm, &pdf_path, 72, cache.path(), 2).expect("page 2");
        assert_eq!(
            trailing_number(&p2),
            Some(2),
            "page 2 file should be page-2"
        );

        // A page beyond the doc must Err even though the cache dir now holds pages 1-2.
        assert!(
            crate::render_page(pdftoppm, &pdf_path, 72, cache.path(), 3).is_err(),
            "page 3 of a 2-page PDF must be out-of-range, not a stale cache hit"
        );

        // Re-rendering page 1 is a cache hit: same path, no dependence on pdftoppm.
        let again =
            crate::render_page(pdftoppm, &pdf_path, 72, cache.path(), 1).expect("cache hit");
        assert_eq!(again, p1, "cache hit must return the same page path");
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
