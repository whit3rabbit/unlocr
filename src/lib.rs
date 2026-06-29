// unlocr library: the OCR backend as callable functions, independent of the
// clap-based CLI. The binary crate (main.rs) and the Tauri host both build on
// top of this. Keeping clap out of here is load-bearing: the GUI needs to drive
// OCR with plain typed params and a progress sink, no Args/argv in sight.

/// Model management and caching utilities.
pub mod model;
/// PDF rendering and processing utilities.
pub mod pdf;
/// System check and preflight diagnostics.
pub mod preflight;
/// OCR server management.
pub mod server;
/// Tool resolution and downloading utilities.
pub mod tools;

// Note: ocr.rs is intentionally NOT a lib module here. It is bin-only CLI glue
// (`run_pdf(&Args)`) that converts the clap Args into the clap-free OcrOptions
// below and delegates the rasterize+OCR loop to `ocr_pages`. With bite 2 done,
// ocr.rs no longer has its own push_page; the lib's `push_page` below is the
// single canonical page-assembly implementation used by both paths.

/// Result type alias with a dynamic error type.
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
    /// Cap on vision tokens per image (`--image-max-tokens`). This is DeepSeek-OCR's
    /// base/large detail knob: more tokens = finer recognition, slower + more VRAM.
    /// None lets the model use its default. Local-server only (set at spawn); inert
    /// for a remote endpoint, whose server already fixed this at its own launch.
    pub image_max_tokens: Option<u32>,
    /// Named chat template forwarded to `--chat-template` (e.g. "deepseek-ocr").
    /// None = let llama-server use the template baked into the model. Local-only.
    pub chat_template: Option<String>,
    /// Sampling repetition penalty sent in the request body. None omits it (server
    /// default). >1.0 (e.g. 1.1) discourages the infinite-loop output some quants
    /// (notably Q4_K_M) fall into on dense pages.
    pub repeat_penalty: Option<f32>,
    /// Page span to OCR, 1-based inclusive `(first, last)`. None = all pages. An
    /// open upper bound (`last == None`) means "first..end of document": pdftoppm
    /// renders `-f first` with no `-l` to the last page natively. Maps to pdftoppm
    /// `-f`/`-l`, so a subset rasterizes only those pages (not the whole PDF then
    /// filtered). Caller validates `first >= 1` and `last >= first` when `last` is set.
    pub pages: Option<(u32, Option<u32>)>,
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
            image_max_tokens: None,
            chat_template: None,
            repeat_penalty: None,
            pages: None,
        }
    }
}

/// How assembled OCR markdown is laid out on disk. Kept out of `OcrOptions`
/// (the loop-driving struct stays free of an output-dir concept); it only steers
/// the shared `write_markdown_output` helper a caller invokes afterwards. Clap-free:
/// the bin crate has its own value-enum and the GUI passes a string that
/// `parse_output_mode` maps here.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum OutputMode {
    /// One `{stem}.md` with every page concatenated (the original behaviour).
    #[default]
    Single,
    /// A `{stem}/page-N.md` folder, one file per page.
    Pages,
    /// Both: the combined `{stem}.md` and the per-page folder.
    Both,
}

/// Map the CLI/GUI string ("single"|"pages"|"both", case-insensitive) to
/// `OutputMode`. Lives in the lib so the bin's clap enum and the GUI's string
/// param resolve through one definition. Unknown -> error.
pub fn parse_output_mode(s: &str) -> Res<OutputMode> {
    match s.trim().to_ascii_lowercase().as_str() {
        "single" => Ok(OutputMode::Single),
        "pages" => Ok(OutputMode::Pages),
        "both" => Ok(OutputMode::Both),
        other => {
            Err(format!("unknown output mode \"{other}\" (expected single|pages|both)").into())
        }
    }
}

/// The result of OCR'ing one input: the combined page-delimited markdown, the
/// per-page texts (real 1-based page number + trimmed markdown), and, when
/// `keep_images` is set, the directory the page PNGs were kept in. Carrying the
/// per-page texts lets a caller write per-page files (`write_markdown_output`)
/// without re-splitting the combined string on its `<!-- page N -->` delimiters.
#[derive(Clone, Debug)]
pub struct OcrOutput {
    /// All pages joined with `<!-- page N -->` delimiters, leading whitespace
    /// trimmed. This is what `{stem}.md` (single/both) holds.
    pub combined: String,
    /// One `(page, text)` per processed page, in order. `page` is the real
    /// 1-based page number (accounts for `--pages` offsets).
    pub pages: Vec<(usize, String)>,
    /// `Some(dir)` when `keep_images` leaked the tempdir; `None` otherwise.
    pub kept_images: Option<PathBuf>,
}

/// Progress events the OCR pipeline emits so a UI (or the CLI's println) can
/// subscribe to download + per-page stages. Kept allocation-free and serializable.
#[derive(Clone, Debug)]
pub enum Progress {
    /// Model/projector download underway. `pct` is 0..=100. `done`/`total` are the
    /// byte counts (total = 0 when the server omits Content-Length) so a UI can show
    /// size and compute transfer speed from successive events.
    Download {
        name: String,
        pct: u8,
        done: u64,
        total: u64,
    },
    /// Server became healthy on this port.
    ServerReady { port: u16 },
    /// One page rasterized+OCR'd. `page` is 1-based, `total` is the page count.
    Page { page: usize, total: usize },
    /// One streaming token chunk emitted during OCR of a page. `page` is 1-based.
    /// The GUI appends `chunk` to the live transcript; the CLI may ignore it.
    PartialText { page: usize, chunk: String },
}

/// Drive one PDF end to end and return the assembled markdown, emitting progress
/// through `on_progress`. This is the canonical, clap-free OCR entry point used
/// by both the Tauri bridge and (after refactor) the CLI path.
///
/// The caller owns writing the markdown to disk: both the CLI and the GUI call
/// the shared `write_markdown_output` helper (single file / per-page folder /
/// both). That keeps the lib free of an output-dir concept in `OcrOptions`: the
/// layout decision is a parameter to the write helper, not a field on the
/// loop-driving struct.
///
/// Spawns llama-server and kills it on drop (server::Server's Drop), so the
/// success path does not orphan it.
///
/// Returns an `OcrOutput` (combined markdown + per-page texts + optional kept
/// image dir) so a caller can lay it out however it likes. This is the GUI's
/// entry point; the CLI drives `ocr_pages` directly.
///
/// `resolved_tools` lets a caller pass already-resolved `preflight::Tools` to skip
/// the `preflight::check` call (which runs `llama-server --version`). Pass `None`
/// to have `run_ocr_job` run the check itself (original behaviour). The GUI passes
/// tools it resolved in its own `preflight` command call so `--version` is invoked
/// only once per run.
pub fn run_ocr_job<P>(
    input: &Path,
    resolved_tools: Option<preflight::Tools>,
    opts: &OcrOptions,
    on_progress: &mut P,
) -> Res<OcrOutput>
where
    P: FnMut(Progress),
{
    if !input.is_file() {
        return Err(format!("not a file: {}", input.display()).into());
    }

    // Locate llama-server + pdftoppm. Accept pre-resolved tools from the caller
    // (e.g. the GUI that already ran preflight::check for its status panel) to
    // avoid a second `llama-server --version` invocation per run. Fall back to
    // check(None) when no tools are provided so the caller-agnostic path still works.
    let tools = match resolved_tools {
        Some(t) => t,
        None => preflight::check(None)?,
    };

    let cache = model::cache_dir(opts.model_dir.clone())?;
    // Route download events through the same sink as page events so the GUI can
    // subscribe to both. model::ensure_with_progress emits Progress::Download;
    // the plain model::ensure (CLI default) reproduces the original println
    // output byte-for-byte.
    let files = model::ensure_with_progress(&cache, &opts.quant, on_progress)?;

    // Pass the raw port (0 = auto): Server::start owns free-port resolution AND the
    // bind-race retry loop. Pre-resolving here would hand start a concrete port and
    // silently disable that retry. Read the real port back from the started server.
    let srv = server::Server::start(
        &tools.llama_server,
        &files.model,
        &files.mmproj,
        opts.port,
        opts.image_max_tokens,
        opts.chat_template.as_deref(),
    )?;
    on_progress(Progress::ServerReady { port: srv.port });

    let out = ocr_pages(&srv, &tools.pdftoppm, input, opts, on_progress, &|| false)?;

    // Drop kills llama-server. `out.kept_images` is Some(dir) only when
    // keep_images is set; bubble it up so the caller can report where the PNGs went.
    drop(srv);
    Ok(out)
}

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
fn preview_cache_dir(pdf: &Path, dpi: u32, cache_root: &Path) -> PathBuf {
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
    // already holding OTHER pages cannot mask an out-of-range request).
    //
    // Distinguish the two reasons rasterize_range can fail:
    //   - "produced no pages": pdftoppm ran cleanly but emitted nothing -> the page is
    //     past the end. This is the out-of-range signal the GUI uses to bound nav.
    //   - any other error (non-zero exit, spawn failure, malformed PDF): a REAL failure
    //     that must surface, not be silently reported as end-of-document (which the GUI
    //     would treat as "past the last page" and truncate navigation).
    if let Err(e) = pdf::rasterize_range(pdftoppm, pdf, &dir, dpi, Some((page, Some(page)))) {
        if !e.to_string().contains("produced no pages") {
            return Err(e);
        }
    }
    pdf::collect_pages(&dir)
        .into_iter()
        .find(|p| pdf::trailing_number(p) == Some(want))
        .ok_or_else(|| format!("page {page} is out of range").into())
}

/// Rasterize one PDF to PNGs and OCR each page in order, emitting a Page
/// progress event per page. Returns an `OcrOutput` carrying both the combined
/// page-delimited markdown and the per-page texts (so a caller can write
/// per-page files without re-splitting), plus, when `opts.keep_images` is set,
/// the directory the page PNGs were kept in (so the CLI can report it; the handle
/// is leaked there to keep the files on disk). Shared by run_ocr_job (lib) and,
/// after bite 2, the CLI's ocr::run_pdf.
pub fn ocr_pages<S, P>(
    srv: &S,
    pdftoppm: &Path,
    input: &Path,
    opts: &OcrOptions,
    on_progress: &mut P,
    should_cancel: &dyn Fn() -> bool,
) -> Res<OcrOutput>
where
    S: server::ImageOcr,
    P: FnMut(Progress),
{
    let tmp = tempfile::tempdir()?;
    let pages = pdf::rasterize_range(pdftoppm, input, tmp.path(), opts.dpi, opts.pages)?;
    let n = pages.len();

    // With a page range, the first rasterized page is the range's start, not page 1.
    // Derive the real page number so the `<!-- page N -->` delimiter and Progress
    // events reflect the actual page, not the loop index.
    let base = opts.pages.map(|(f, _)| f as usize).unwrap_or(1);

    let mut md = String::new();
    // Capture each page's text separately so a caller can write per-page files
    // (write_markdown_output) without re-splitting the combined string.
    let mut pages_text: Vec<(usize, String)> = Vec::with_capacity(n);
    for (i, page) in pages.iter().enumerate() {
        // Stop (GUI) sets this; the local backend also kills llama-server so an
        // in-flight stream errors out, but checking here stops the remote backend
        // (no pid to kill) at the next page boundary. Err is remapped to "stopped"
        // by the GUI's run_ocr (cmd_run.rs); the CLI never cancels (|| false).
        if should_cancel() {
            return Err("stopped".into());
        }
        let page_num = base + i;
        on_progress(Progress::Page {
            page: page_num,
            total: n,
        });

        let bytes = std::fs::read(page)?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let data_uri = format!("data:image/png;base64,{b64}");
        // Use streaming so the GUI receives PartialText events as tokens arrive.
        // The CLI's on_progress sink ignores PartialText (zero cost), while the
        // Tauri bridge forwards each chunk to the frontend for live appending.
        let text = srv.ocr_image_stream(
            &opts.prompt,
            &data_uri,
            opts.max_tokens,
            opts.repeat_penalty,
            &mut |chunk: &str| {
                on_progress(Progress::PartialText {
                    page: page_num,
                    chunk: chunk.to_string(),
                });
                !should_cancel()
            },
            should_cancel,
        )?;
        // push_page writes page idx+1, so pass the real page number minus one.
        push_page(&mut md, page_num - 1, text.trim());
        // Same trimmed text the combined string holds, retained per-page so a
        // caller can write per-page files without re-splitting on the delimiter.
        pages_text.push((page_num, text.trim().to_string()));
    }

    let kept = if opts.keep_images {
        // Leak the temp handle so the PNGs survive; return the path for the
        // caller (CLI) to report. `keep()` consumes the TempDir and returns
        // the directory PathBuf (no longer auto-deleted).
        Some(tmp.keep())
    } else {
        None
    };
    Ok(OcrOutput {
        combined: md.trim_start().to_string(),
        pages: pages_text,
        kept_images: kept,
    })
}

/// Resolve the output `.md` path for one input. Shared by the CLI (`ocr::run_pdf`)
/// and the GUI (`run_ocr`) so both agree on where a result is written.
///
/// - `out_dir`: the chosen output folder. A relative `out_file` is joined under it;
///   the default `{stem}.md` is written into it.
/// - `out_file`: optional explicit filename/path (single-input only). An absolute
///   path is used verbatim (ignoring `out_dir`). When it has no extension, `.md`
///   is appended; a non-`.md` extension is left exactly as typed.
/// - `stem`: input file stem, used for the default `{stem}.md` when `out_file` is None.
pub fn resolve_output_path(out_dir: &Path, out_file: Option<&Path>, stem: &str) -> PathBuf {
    match out_file {
        None => out_dir.join(format!("{stem}.md")),
        Some(p) => {
            // Append .md only when no extension is present; respect a typed extension.
            let p = if p.extension().is_none() {
                p.with_extension("md")
            } else {
                p.to_path_buf()
            };
            if p.is_absolute() {
                p
            } else {
                out_dir.join(p)
            }
        }
    }
}

/// Write assembled OCR output to disk per `mode`, returning every path written
/// (combined file first in `Both`). Shared by the CLI (`ocr::run_pdf`) and the
/// GUI (`run_ocr`) so both front ends agree on layout. The caller owns any
/// read-allowlist (the GUI inserts these into `AppState.read_allow`); this fn
/// only writes files + their parent dirs.
///
/// - `Single`: one `{stem}.md` (or `out_file` if given) holding `output.combined`.
/// - `Pages`: a `{out_dir}/{stem}/page-N.md` folder, one file per page. `out_file`
///   is ignored for the folder name (the caller warns when it was set). Page
///   numbers are zero-padded to the width of the largest page number so files
///   sort lexicographically (page-01 before page-10).
/// - `Both`: the combined file plus the per-page folder.
pub fn write_markdown_output(
    mode: OutputMode,
    out_dir: &Path,
    out_file: Option<&Path>,
    stem: &str,
    output: &OcrOutput,
) -> Res<Vec<PathBuf>> {
    let mut written: Vec<PathBuf> = Vec::new();

    if matches!(mode, OutputMode::Single | OutputMode::Both) {
        let combined_path = resolve_output_path(out_dir, out_file, stem);
        if let Some(parent) = combined_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&combined_path, &output.combined)?;
        written.push(combined_path);
    }

    if matches!(mode, OutputMode::Pages | OutputMode::Both) {
        let folder = out_dir.join(stem);
        std::fs::create_dir_all(&folder)?;
        // Zero-pad to the largest page number's width (min 2) so a listing sorts
        // page-01 before page-10. Width defaults to 2 when there are no pages
        // (defensive: rasterize_range errors on zero pages before we get here).
        let width = output
            .pages
            .last()
            .map(|(n, _)| n.to_string().len())
            .unwrap_or(2)
            .max(2);
        for (page_num, text) in &output.pages {
            let path = folder.join(format!("page-{page_num:0width$}.md"));
            std::fs::write(&path, text)?;
            written.push(path);
        }
    }

    Ok(written)
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
#[path = "lib_tests.rs"]
mod tests;
