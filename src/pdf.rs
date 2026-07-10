// Rasterize a PDF to one PNG per page using pdftoppm, returned in page order.

use crate::Res;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Run `pdftoppm -png -r <dpi> <pdf> <outdir>/page` and collect the PNGs sorted
/// by page number. Renders all pages; thin wrapper over `rasterize_range`.
/// `password`, when `Some`, is the PDF's user/open password (poppler `-upw`).
pub fn rasterize(
    pdftoppm: &Path,
    pdf: &Path,
    outdir: &Path,
    dpi: u32,
    password: Option<&str>,
) -> Res<Vec<PathBuf>> {
    rasterize_range(pdftoppm, pdf, outdir, dpi, None, None, password)
}

/// How often `rasterize_range` polls `outdir` for newly-written pages while
/// pdftoppm is still running. Short enough to feel live, long enough that
/// polling overhead is noise next to a page render. Shorter under test so a
/// real (tiny, fast) fixture PDF reliably produces an observable tick instead
/// of a flaky race against process exit.
#[cfg(not(test))]
const POLL_INTERVAL: Duration = Duration::from_millis(150);
#[cfg(test)]
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Like `rasterize`, but when `range` is `Some((first, last))` (1-based inclusive)
/// passes `-f`/`-l` so pdftoppm renders only that page span. An open upper bound
/// (`last == None`) emits `-f first` with no `-l`, which pdftoppm renders to the
/// end of the document. pdftoppm preserves the real page number in the filename
/// suffix (e.g. `-f 5` -> `page-5.png`), so `collect_pages`/`trailing_number` keep
/// working and the caller can recover the true page number. pdftoppm zero-pads the
/// suffix based on page count, so we sort by the parsed trailing integer rather
/// than lexically.
///
/// `on_page`, when `Some`, is called with the number of pages written so far
/// each time that count increases, while pdftoppm is still running (not after
/// it exits). pdftoppm writes each `page-N.png` fully before starting the
/// next, so a rising file count is a reliable progress signal. Pass `None`
/// for callers that don't need live feedback (the GUI preview cache).
pub fn rasterize_range(
    pdftoppm: &Path,
    pdf: &Path,
    outdir: &Path,
    dpi: u32,
    range: Option<(u32, Option<u32>)>,
    mut on_page: Option<&mut dyn FnMut(usize)>,
    password: Option<&str>,
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
    // User/open password for an encrypted PDF. `-upw` takes the value literally
    // (no `./` guard needed even if it starts with `-`); poppler ignores it on an
    // unencrypted PDF. Placed after the flags, before the pdf positional.
    if let Some(pw) = password {
        cmd.arg("-upw").arg(pw);
    }
    let mut child = cmd.arg(&pdf_arg).arg(&prefix).spawn()?;
    let mut last_count = 0usize;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                // try_wait() itself failed (rare OS-level condition): the child
                // may still be running. Child::drop does not kill or wait on the
                // process, so reap/kill it here before propagating rather than
                // orphaning pdftoppm.
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.into());
            }
        }
        std::thread::sleep(POLL_INTERVAL);
        let count = collect_pages(outdir).len();
        if count != last_count {
            last_count = count;
            if let Some(cb) = on_page.as_mut() {
                cb(count);
            }
        }
    };
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

/// Find a `pdfinfo` binary next to `pdftoppm` (the same poppler install ships
/// both, on every platform this app supports). Returns `None` when there's no
/// directory component (a bare command name resolved via PATH at spawn time,
/// not a concrete path) or no such file sits beside it.
fn sibling_pdfinfo(pdftoppm: &Path) -> Option<PathBuf> {
    let dir = pdftoppm.parent()?;
    let exe = if cfg!(windows) {
        "pdfinfo.exe"
    } else {
        "pdfinfo"
    };
    let pdfinfo = dir.join(exe);
    pdfinfo.is_file().then_some(pdfinfo)
}

/// Best-effort total page count for `pdf`, used to give a rasterizing progress
/// event a denominator when the caller didn't already pass an explicit
/// `--pages` range. Returns `None` on anything unexpected (no sibling
/// `pdfinfo`, spawn failure, non-zero exit, unparsable output) -- this is a
/// nice-to-have, not a hard dependency, so failures degrade silently.
pub fn total_pages(pdftoppm: &Path, pdf: &Path, password: Option<&str>) -> Option<u32> {
    info(pdftoppm, pdf, password).ok().map(|i| i.pages)
}

/// Metadata about a PDF, shown by the GUI's "PDF info" popup. `file_size_bytes`
/// comes from the filesystem (locale-independent); everything else is parsed
/// from `pdfinfo`'s `Label:   value` output lines.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfInfo {
    pub pages: u32,
    pub title: Option<String>,
    pub author: Option<String>,
    pub producer: Option<String>,
    pub creation_date: Option<String>,
    pub page_size: Option<String>,
    pub pdf_version: Option<String>,
    pub encrypted: Option<bool>,
    pub file_size_bytes: u64,
}

/// User-triggered (not best-effort like `total_pages`): a missing `pdfinfo` or
/// a failed run is a real `Err` the GUI surfaces in the info dialog, rather
/// than a silently degraded field.
pub fn info(pdftoppm: &Path, pdf: &Path, password: Option<&str>) -> Res<PdfInfo> {
    let pdfinfo = sibling_pdfinfo(pdftoppm).ok_or(
        "pdfinfo not found next to pdftoppm (it ships in the same poppler install; \
         reinstalling poppler should add it)",
    )?;
    let mut cmd = Command::new(&pdfinfo);
    if let Some(pw) = password {
        cmd.arg("-upw").arg(pw);
    }
    let out = cmd.arg(pdf).output()?;
    if !out.status.success() {
        return Err(format!("pdfinfo failed ({})", out.status).into());
    }
    let text = String::from_utf8_lossy(&out.stdout);

    let field = |label: &str| -> Option<String> {
        text.lines().find_map(|l| {
            let v = l.strip_prefix(label)?.trim();
            (!v.is_empty()).then(|| v.to_string())
        })
    };

    let pages = field("Pages:")
        .and_then(|v| v.parse().ok())
        .ok_or("pdfinfo output had no parsable Pages: line")?;
    let encrypted = field("Encrypted:").map(|v| v.starts_with("yes"));
    let file_size_bytes = std::fs::metadata(pdf)?.len();

    Ok(PdfInfo {
        pages,
        title: field("Title:"),
        author: field("Author:"),
        producer: field("Producer:"),
        creation_date: field("CreationDate:"),
        page_size: field("Page size:"),
        pdf_version: field("PDF version:"),
        encrypted,
        file_size_bytes,
    })
}

/// Can `pdf` be opened with `password` (None = try with no password)? Prefers the
/// sibling `pdfinfo` (cheap, no render): exit 0 means the password (or lack of one)
/// unlocks it. When no sibling `pdfinfo` exists (pdftoppm resolved as a bare PATH
/// name), falls back to rendering page 1 with pdftoppm into a throwaway dir at a low
/// dpi -- a non-empty result means it opened. Any spawn/OS error counts as "cannot
/// open" (false), never a hard error, since this is a probe. Pub so the GUI's
/// password probe commands share this exact logic (incl. the no-sibling-pdfinfo
/// fallback) instead of re-deriving it from `info`.
pub fn can_open(pdftoppm: &Path, pdf: &Path, password: Option<&str>) -> bool {
    if let Some(pdfinfo) = sibling_pdfinfo(pdftoppm) {
        let mut cmd = Command::new(&pdfinfo);
        if let Some(pw) = password {
            cmd.arg("-upw").arg(pw);
        }
        return matches!(cmd.arg(pdf).output(), Ok(o) if o.status.success());
    }
    // No sibling pdfinfo: probe by rendering just page 1 at a small dpi.
    match tempfile::tempdir() {
        Ok(tmp) => rasterize_range(
            pdftoppm,
            pdf,
            tmp.path(),
            36,
            Some((1, Some(1))),
            None,
            password,
        )
        .map(|pages| !pages.is_empty())
        .unwrap_or(false),
        Err(_) => false,
    }
}

/// Does `pdf` require a user/open password? True ONLY when it cannot be opened with
/// no password AND poppler reports a password error -- so a corrupt or otherwise
/// unreadable PDF is not misreported as "needs password" (which would trap the GUI
/// in an infinite re-prompt: no password could ever satisfy it). Best-effort probe:
/// - opens with no password -> `false` (unencrypted or owner-only-restricted).
/// - sibling `pdfinfo` present -> `true` only if its stderr mentions "password".
/// - no sibling `pdfinfo` (bare-name pdftoppm) -> `true`, since we can't sniff the
///   reason and an encrypted PDF must still surface the prompt; a corrupt PDF in
///   that narrow config falls back to the prompt, escapable via Cancel.
pub fn needs_user_password(pdftoppm: &Path, pdf: &Path) -> bool {
    if can_open(pdftoppm, pdf, None) {
        return false;
    }
    match sibling_pdfinfo(pdftoppm) {
        Some(pdfinfo) => match Command::new(&pdfinfo).arg(pdf).output() {
            Ok(o) if !o.status.success() => String::from_utf8_lossy(&o.stderr)
                .to_lowercase()
                .contains("password"),
            _ => false,
        },
        None => true,
    }
}

/// Resolve which of `candidates` (if any) unlocks `pdf`, returning the password to
/// pass to `-upw` (`None` = no password needed):
/// - 0 candidates -> `Ok(None)` (unchanged behavior, no probe).
/// - 1+ candidates -> probe with no password first (covers unencrypted and
///   owner-only-restricted PDFs), then each candidate in order; the first that
///   opens wins. If the PDF is encrypted and none work, `Err` -- a clear
///   "password required or incorrect" the caller surfaces as a per-file skip (see
///   the CLI batch loop / GUI prompt), rather than a cryptic pdftoppm failure at
///   rasterize time.
pub fn select_password(pdftoppm: &Path, pdf: &Path, candidates: &[String]) -> Res<Option<String>> {
    if candidates.is_empty() {
        return Ok(None);
    }
    if can_open(pdftoppm, pdf, None) {
        return Ok(None);
    }
    for pw in candidates {
        if can_open(pdftoppm, pdf, Some(pw)) {
            return Ok(Some(pw.clone()));
        }
    }
    Err(format!(
        "password required or incorrect for {} (tried {} candidate(s))",
        pdf.display(),
        candidates.len()
    )
    .into())
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
    use std::path::{Path, PathBuf};

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
        let pages72 = super::rasterize(pdftoppm, &pdf_path, out72.path(), 72, None)
            .expect("rasterize at 72 dpi");
        let out144 = tempfile::tempdir().expect("tmp 144 dir");
        let pages144 = super::rasterize(pdftoppm, &pdf_path, out144.path(), 144, None)
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
        let pages = super::rasterize(pdftoppm, &pdf_path, out.path(), 72, None)
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

    // Locks the spawn+poll rewrite of rasterize_range (previously a blocking
    // `.status()` call): the callback must see at least one tick (POLL_INTERVAL
    // is 5ms under test, so a real pdftoppm invocation reliably crosses it),
    // counts must never decrease, and the final return value must be unchanged
    // by the refactor (still 2 pages, in order). Skips without pdftoppm.
    #[test]
    fn rasterize_range_reports_incremental_progress() {
        if std::process::Command::new("pdftoppm")
            .arg("-v")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            eprintln!(
                "skipping rasterize_range_reports_incremental_progress: pdftoppm not on PATH"
            );
            return;
        }

        let pdf_dir = tempfile::tempdir().expect("tmp pdf dir");
        let pdf_path = pdf_dir.path().join("fixture.pdf");
        std::fs::write(&pdf_path, minimal_two_page_pdf()).expect("write fixture pdf");
        let pdftoppm = Path::new("pdftoppm");
        let out = tempfile::tempdir().expect("tmp page dir");

        let mut seen: Vec<usize> = Vec::new();
        let mut on_page = |n: usize| seen.push(n);
        let pages = super::rasterize_range(
            pdftoppm,
            &pdf_path,
            out.path(),
            72,
            None,
            Some(&mut on_page),
            None,
        )
        .expect("rasterize with progress callback");

        assert_eq!(pages.len(), 2, "callback must not change the render result");
        assert!(
            !seen.is_empty(),
            "expected at least one progress tick before pdftoppm exits"
        );
        for w in seen.windows(2) {
            assert!(
                w[0] < w[1],
                "progress counts must strictly increase: {seen:?}"
            );
        }
        assert!(
            *seen.last().unwrap() <= 2,
            "reported count must not exceed the real page count: {seen:?}"
        );
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
        let tail = super::rasterize_range(
            pdftoppm,
            &pdf_path,
            out_tail.path(),
            72,
            Some((2, None)),
            None,
            None,
        )
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
        let all = super::rasterize_range(
            pdftoppm,
            &pdf_path,
            out_all.path(),
            72,
            Some((1, None)),
            None,
            None,
        )
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

        let pages = crate::render_pages(pdftoppm, &pdf_path, 72, cache.path(), None)
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
        let again = crate::render_pages(pdftoppm, &pdf_path, 72, cache.path(), None)
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

        let p1 =
            crate::render_page(pdftoppm, &pdf_path, 72, cache.path(), 1, None).expect("page 1");
        assert_eq!(
            trailing_number(&p1),
            Some(1),
            "page 1 file should be page-1"
        );
        assert!(p1.exists() && std::fs::metadata(&p1).unwrap().len() > 0);

        let p2 =
            crate::render_page(pdftoppm, &pdf_path, 72, cache.path(), 2, None).expect("page 2");
        assert_eq!(
            trailing_number(&p2),
            Some(2),
            "page 2 file should be page-2"
        );

        // A page beyond the doc must Err even though the cache dir now holds pages 1-2.
        assert!(
            crate::render_page(pdftoppm, &pdf_path, 72, cache.path(), 3, None).is_err(),
            "page 3 of a 2-page PDF must be out-of-range, not a stale cache hit"
        );

        // Re-rendering page 1 is a cache hit: same path, no dependence on pdftoppm.
        let again =
            crate::render_page(pdftoppm, &pdf_path, 72, cache.path(), 1, None).expect("cache hit");
        assert_eq!(again, p1, "cache hit must return the same page path");
    }

    // Both total_pages and info() need a resolvable `pdftoppm` AND a sibling
    // `pdfinfo` next to it, which a bare `Path::new("pdftoppm")` (PATH lookup,
    // no directory component) can't give us. Resolve the real binary path via
    // `which` (unix-only, matching this test environment) and return None if
    // either that or a sibling pdfinfo isn't available, so callers can skip
    // cleanly instead of duplicating this lookup per test.
    fn resolve_pdftoppm_with_pdfinfo() -> Option<PathBuf> {
        let which = std::process::Command::new("which").arg("pdftoppm").output();
        let resolved = match which {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            }
            _ => return None,
        };
        let pdftoppm = PathBuf::from(resolved);
        let has_pdfinfo = pdftoppm
            .parent()
            .map(|dir| {
                dir.join(if cfg!(windows) {
                    "pdfinfo.exe"
                } else {
                    "pdfinfo"
                })
                .is_file()
            })
            .unwrap_or(false);
        has_pdfinfo.then_some(pdftoppm)
    }

    #[test]
    fn total_pages_matches_known_fixture() {
        let Some(pdftoppm) = resolve_pdftoppm_with_pdfinfo() else {
            eprintln!(
                "skipping total_pages_matches_known_fixture: pdftoppm/sibling pdfinfo not resolvable"
            );
            return;
        };

        let pdf_dir = tempfile::tempdir().expect("tmp pdf dir");
        let pdf_path = pdf_dir.path().join("fixture.pdf");
        std::fs::write(&pdf_path, minimal_two_page_pdf()).expect("write fixture pdf");

        assert_eq!(
            super::total_pages(&pdftoppm, &pdf_path, None),
            Some(2),
            "pdfinfo-derived page count must match the 2-page fixture"
        );
    }

    #[test]
    fn info_reports_known_fixture_fields() {
        let Some(pdftoppm) = resolve_pdftoppm_with_pdfinfo() else {
            eprintln!("skipping info_reports_known_fixture_fields: pdftoppm/sibling pdfinfo not resolvable");
            return;
        };

        let pdf_dir = tempfile::tempdir().expect("tmp pdf dir");
        let pdf_path = pdf_dir.path().join("fixture.pdf");
        let bytes = minimal_two_page_pdf();
        std::fs::write(&pdf_path, &bytes).expect("write fixture pdf");

        let info = super::info(&pdftoppm, &pdf_path, None).expect("pdfinfo on 2-page fixture");
        assert_eq!(info.pages, 2, "must match the 2-page fixture");
        assert_eq!(
            info.file_size_bytes,
            bytes.len() as u64,
            "file size must come from the filesystem, not pdfinfo"
        );
    }

    // No candidates -> no password, no probe: the common (unencrypted) path is
    // unchanged. Needs no external binary since 0 candidates short-circuits.
    #[test]
    fn select_password_none_when_no_candidates() {
        let pdf = Path::new("/nonexistent.pdf");
        let got = super::select_password(Path::new("pdftoppm"), pdf, &[]).expect("no candidates");
        assert_eq!(
            got, None,
            "empty candidate list must resolve to no password"
        );
    }

    // A single candidate is probed like any other (no blind passthrough): an
    // unopenable PDF with one wrong candidate must Err with the clear
    // "password required or incorrect" message, not silently return the password to
    // fail cryptically at rasterize time. A nonexistent path fails every can_open
    // probe regardless of whether pdftoppm is on PATH, so this needs no fixture.
    #[test]
    fn select_password_single_candidate_is_probed() {
        let pdf = Path::new("/nonexistent.pdf");
        let cands = vec!["hunter2".to_string()];
        assert!(
            super::select_password(Path::new("pdftoppm"), pdf, &cands).is_err(),
            "a lone candidate is probed; an unopenable PDF must Err, not pass it through"
        );
    }

    // 2+ candidates on an UNENCRYPTED PDF: the no-password probe succeeds first, so
    // none of the candidates are needed and select resolves to None. Needs a
    // resolvable pdftoppm + sibling pdfinfo (the probe path).
    #[test]
    fn select_password_multi_unencrypted_resolves_none() {
        let Some(pdftoppm) = resolve_pdftoppm_with_pdfinfo() else {
            eprintln!("skipping select_password_multi_unencrypted_resolves_none: pdftoppm/pdfinfo not resolvable");
            return;
        };
        let pdf_dir = tempfile::tempdir().expect("tmp pdf dir");
        let pdf_path = pdf_dir.path().join("fixture.pdf");
        std::fs::write(&pdf_path, minimal_two_page_pdf()).expect("write fixture pdf");

        let cands = vec!["wrong-a".to_string(), "wrong-b".to_string()];
        let got = super::select_password(&pdftoppm, &pdf_path, &cands)
            .expect("unencrypted PDF opens with no password");
        assert_eq!(
            got, None,
            "an unencrypted PDF must resolve to no password even with candidates present"
        );
    }

    // Encrypted PDF: the right password unlocks, a wrong-only list errors. poppler
    // cannot create an encrypted PDF, so generate one with `qpdf --encrypt` and skip
    // when qpdf (or pdftoppm/pdfinfo) is unavailable, matching the pdftoppm-on-PATH
    // skip pattern used throughout this module.
    #[test]
    fn select_password_encrypted_picks_working_and_errors_on_none() {
        let Some(pdftoppm) = resolve_pdftoppm_with_pdfinfo() else {
            eprintln!("skipping select_password_encrypted: pdftoppm/pdfinfo not resolvable");
            return;
        };
        if std::process::Command::new("qpdf")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            eprintln!("skipping select_password_encrypted: qpdf not on PATH");
            return;
        }

        let dir = tempfile::tempdir().expect("tmp dir");
        let plain = dir.path().join("plain.pdf");
        let enc = dir.path().join("enc.pdf");
        std::fs::write(&plain, minimal_two_page_pdf()).expect("write plain pdf");
        // qpdf --encrypt <user-pw> <owner-pw> <keylen> -- in out
        let status = std::process::Command::new("qpdf")
            .args(["--encrypt", "s3cret", "ownerpw", "256", "--"])
            .arg(&plain)
            .arg(&enc)
            .status()
            .expect("run qpdf");
        assert!(status.success(), "qpdf must produce an encrypted PDF");

        // Right password present among candidates -> Some(right).
        let ok = vec!["nope".to_string(), "s3cret".to_string()];
        let got = super::select_password(&pdftoppm, &enc, &ok).expect("a candidate unlocks it");
        assert_eq!(
            got.as_deref(),
            Some("s3cret"),
            "select must pick the candidate that actually unlocks the PDF"
        );

        // Only wrong passwords -> Err (the CLI batch loop / GUI surfaces this as a skip).
        let bad = vec!["nope".to_string(), "still-wrong".to_string()];
        assert!(
            super::select_password(&pdftoppm, &enc, &bad).is_err(),
            "an encrypted PDF with no working candidate must Err"
        );
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
