use crate::pdf;
use crate::Res;
use std::path::{Path, PathBuf};

/// Rasterize a PDF's pages to PNGs in a content-keyed cache dir, reusing the
/// cached PNGs on a repeat call instead of re-running pdftoppm. Returns the page
/// PNG paths in order. Used by the GUI preview pane; the OCR path keeps its own
/// ephemeral tempdir (`ocr_pages`), so CLI behavior is unchanged.
///
/// Cache key = hash(canonical PDF path + mtime + dpi): a changed file (different
/// mtime) or a different dpi misses and re-renders. `cache_root` is the resolved
/// unlocr cache dir; previews live under `<cache_root>/previews/<key>/`.
pub fn render_pages(pdftoppm: &Path, pdf: &Path, dpi: u32, cache_root: &Path) -> Res<Vec<PathBuf>> {
    // ponytail: unbounded cache (no eviction). It is under the OS cache dir, so the
    // user/OS can clear it; add an LRU/size cap here if the previews dir grows.
    let dir = preview_cache_dir(pdf, dpi, cache_root);

    // Cache hit: a prior render left page PNGs here. Reuse them (pdftoppm is
    // never invoked on this path).
    let cached = pdf::collect_pages(&dir);
    if !cached.is_empty() {
        return Ok(cached);
    }
    std::fs::create_dir_all(&dir)?;
    pdf::rasterize(pdftoppm, pdf, &dir, dpi)
}

/// Resolve the per-PDF previews directory: `<cache_root>/previews/<key>` where
/// key = hash(canonical PDF path + mtime + dpi). Deterministic for a given file
/// state, so repeat previews hit the same dir; a changed file (mtime) or dpi
/// keys to a fresh dir. Split out so the keying is unit-testable without pdftoppm.
pub(crate) fn preview_cache_dir(pdf: &Path, dpi: u32, cache_root: &Path) -> PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::UNIX_EPOCH;

    let canon = pdf.canonicalize().unwrap_or_else(|_| pdf.to_path_buf());
    let mtime = std::fs::metadata(&canon)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = DefaultHasher::new();
    canon.to_string_lossy().hash(&mut h);
    mtime.hash(&mut h);
    dpi.hash(&mut h);
    cache_root
        .join("previews")
        .join(format!("{:016x}", h.finish()))
}

/// Render and cache a SINGLE page (1-based) of a PDF to a PNG, returning its path.
/// Backs the GUI preview pane's lazy per-page load: importing a large PDF no longer
/// rasterizes every page up front (the all-pages `render_pages`), only the page the
/// user actually views. Shares `render_pages`' on-disk cache dir, so a page rendered
/// here is reused by a later full render and vice versa. Returns Err when `page` is
/// out of range (pdftoppm produces no file for it), which the GUI treats as "past
/// the last page" to bound navigation without a separate page-count probe.
pub fn render_page(
    pdftoppm: &Path,
    pdf: &Path,
    dpi: u32,
    cache_root: &Path,
    page: u32,
) -> Res<PathBuf> {
    let dir = preview_cache_dir(pdf, dpi, cache_root);
    let want = page as u64;
    // Cache hit: this exact page was rendered before (by render_page or render_pages).
    // collect_pages returns the whole dir, so match the specific page by number.
    if let Some(p) = pdf::collect_pages(&dir)
        .into_iter()
        .find(|p| pdf::trailing_number(p) == Some(want))
    {
        return Ok(p);
    }
    std::fs::create_dir_all(&dir)?;
    // Render just this page, then re-scan for the specific page file (so a cache dir
    // already holding OTHER pages cannot mask an out-of-range request). A real
    // pdftoppm failure (non-zero exit, spawn, malformed PDF) propagates via `?`;
    // rasterize_range returns an empty Vec when the page is past EOF, which the
    // find() below turns into the out-of-range error the GUI uses to bound nav.
    pdf::rasterize_range(pdftoppm, pdf, &dir, dpi, Some((page, Some(page))))?;
    pdf::collect_pages(&dir)
        .into_iter()
        .find(|p| pdf::trailing_number(p) == Some(want))
        .ok_or_else(|| format!("page {page} is out of range").into())
}
