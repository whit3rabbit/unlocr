// unlocr library: the OCR backend as callable functions, independent of the
// clap-based CLI. The binary crate (main.rs) and the Tauri host both build on
// top of this. Keeping clap out of here is load-bearing: the GUI needs to drive
// OCR with plain typed params and a progress sink, no Args/argv in sight.

pub mod model;
pub mod pdf;
pub mod preflight;
pub mod server;

// Note: ocr.rs is intentionally NOT a lib module here. It is bin-only CLI glue
// (`run_pdf(&Args)`) that converts the clap Args into the clap-free OcrOptions
// below and delegates the rasterize+OCR loop to `ocr_pages`. With bite 2 done,
// ocr.rs no longer has its own push_page; the lib's `push_page` below is the
// single canonical page-assembly implementation used by both paths.

pub type Res<T> = Result<T, Box<dyn std::error::Error>>;

use base64::Engine;
use std::path::{Path, PathBuf};

/// OCR parameters expressed as plain fields, no clap. These mirror the CLI
/// flags but are owned by the library so the Tauri host can construct them from
/// frontend state. Field set matches the board's OcrOptions spec exactly.
#[derive(Clone, Debug)]
pub struct OcrOptions {
    /// Quant tag (e.g. "Q8_0", "BF16", "Q4_K_M"). Resolved by the caller, not a
    /// quality enum, so the lib has no opinion about the best/good/less tiers.
    pub quant: String,
    /// Max tokens generated per page (caps runaway generation on dense pages).
    pub max_tokens: u32,
    /// Rasterization DPI passed to pdftoppm.
    pub dpi: u32,
    /// OCR prompt sent with every page.
    pub prompt: String,
    /// Port for llama-server (0 = auto-pick a free port).
    pub port: u16,
    /// Model cache directory override (None = per-OS default cache dir).
    pub model_dir: Option<PathBuf>,
    /// Keep the intermediate page PNGs instead of deleting them.
    pub keep_images: bool,
}

impl Default for OcrOptions {
    /// Defaults match the CLI's current defaults so a caller that does nothing
    /// special gets the same behavior as `unlocr` with no flags.
    fn default() -> Self {
        OcrOptions {
            quant: "Q8_0".to_string(),
            max_tokens: 4096,
            dpi: 144,
            prompt: "<|grounding|>Convert the document to markdown.".to_string(),
            port: 0,
            model_dir: None,
            keep_images: false,
        }
    }
}

/// Progress events the OCR pipeline emits so a UI (or the CLI's println) can
/// subscribe to download + per-page stages. Kept allocation-free and serializable.
#[derive(Clone, Debug)]
pub enum Progress {
    /// Model/projector download underway. `pct` is 0..=100.
    Download { name: String, pct: u8 },
    /// Server became healthy on this port.
    ServerReady { port: u16 },
    /// One page rasterized+OCR'd. `page` is 1-based, `total` is the page count.
    Page { page: usize, total: usize },
}

/// Drive one PDF end to end and return the assembled markdown, emitting progress
/// through `on_progress`. This is the canonical, clap-free OCR entry point used
/// by both the Tauri bridge and (after refactor) the CLI path.
///
/// The caller owns writing the markdown to disk: the CLI writes `{stem}.md`,
/// the GUI keeps it in memory / its own store. That keeps the lib free of an
/// output-dir concept.
///
/// Spawns llama-server and kills it on drop (server::Server's Drop), so the
/// success path does not orphan it.
pub fn run_ocr_job<P>(
    input: &Path,
    pdftoppm: &Path,
    llama_bin: Option<&Path>,
    opts: &OcrOptions,
    on_progress: &mut P,
) -> Res<String>
where
    P: FnMut(Progress),
{
    if !input.is_file() {
        return Err(format!("not a file: {}", input.display()).into());
    }

    // Locate llama-server + pdftoppm. preflight::check takes an optional override
    // for llama-server only; pdftoppm is resolved from PATH.
    let tools = preflight::check(llama_bin)?;

    let cache = model::cache_dir(opts.model_dir.clone())?;
    let files = model::ensure(&cache, &opts.quant)?;

    let port = if opts.port == 0 {
        server::free_port()?
    } else {
        opts.port
    };
    let srv = server::Server::start(&tools.llama_server, &files.model, &files.mmproj, port)?;
    on_progress(Progress::ServerReady { port });

    let (md, _kept) = ocr_pages(&srv, pdftoppm, input, opts, on_progress)?;

    // Drop kills llama-server. keep_images only matters for the CLI's temp dir,
    // which lives inside ocr_pages; honored there.
    drop(srv);
    Ok(md)
}

/// Rasterize one PDF to PNGs and OCR each page in order, emitting a Page
/// progress event per page. Returns the page-delimited markdown and, when
/// `opts.keep_images` is set, the directory the page PNGs were kept in (so the
/// CLI can report it; the handle is leaked there to keep the files on disk).
/// Shared by run_ocr_job (lib) and, after bite 2, the CLI's ocr::run_pdf.
pub fn ocr_pages<P>(
    srv: &server::Server,
    pdftoppm: &Path,
    input: &Path,
    opts: &OcrOptions,
    on_progress: &mut P,
) -> Res<(String, Option<PathBuf>)>
where
    P: FnMut(Progress),
{
    let tmp = tempfile::tempdir()?;
    let pages = pdf::rasterize(pdftoppm, input, tmp.path(), opts.dpi)?;
    let n = pages.len();

    let mut md = String::new();
    for (i, page) in pages.iter().enumerate() {
        on_progress(Progress::Page { page: i + 1, total: n });

        let bytes = std::fs::read(page)?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let data_uri = format!("data:image/png;base64,{b64}");
        let text = srv.ocr_image(&opts.prompt, &data_uri, opts.max_tokens)?;
        push_page(&mut md, i, text.trim());
    }

    let kept = if opts.keep_images {
        // Leak the temp handle so the PNGs survive; return the path for the
        // caller (CLI) to report. `keep()` consumes the TempDir and returns
        // the directory PathBuf (no longer auto-deleted).
        Some(tmp.keep())
    } else {
        None
    };
    Ok((md.trim_start().to_string(), kept))
}

/// Append one page's text with a `<!-- page N -->` delimiter (1-based).
/// Canonical implementation: ocr_pages (lib) and the CLI path (via run_pdf's
/// delegation) both route through this, so page-delimited markdown is identical
/// across the CLI and GUI callers (covered by the lib test below).
pub fn push_page(md: &mut String, idx: usize, text: &str) {
    md.push_str(&format!("\n\n<!-- page {} -->\n\n", idx + 1));
    md.push_str(text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ocroptions_default_matches_cli() {
        // Defaults must mirror the CLI flags so a no-op caller matches `unlocr`.
        let o = OcrOptions::default();
        assert_eq!(o.quant, "Q8_0");
        assert_eq!(o.max_tokens, 4096);
        assert_eq!(o.dpi, 144);
        assert_eq!(o.prompt, "<|grounding|>Convert the document to markdown.");
        assert_eq!(o.port, 0);
        assert!(o.model_dir.is_none());
        assert!(!o.keep_images);
    }

    #[test]
    fn push_page_assembles_delimiters() {
        let mut md = String::new();
        push_page(&mut md, 0, "first");
        push_page(&mut md, 1, "second");
        assert_eq!(
            md.trim_start(),
            "<!-- page 1 -->\n\nfirst\n\n<!-- page 2 -->\n\nsecond"
        );
    }

    #[test]
    fn run_ocr_job_rejects_missing_file() {
        // Non-network path: run_ocr_job must fail fast on a non-existent input
        // before touching preflight/network. Locks the early validation.
        let mut progress = |_: Progress| {};
        let err = run_ocr_job(
            Path::new("/nonexistent/ferrum-bite-1.pdf"),
            Path::new("pdftoppm"),
            None,
            &OcrOptions::default(),
            &mut progress,
        );
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("not a file"), "unexpected error: {msg}");
    }
}
