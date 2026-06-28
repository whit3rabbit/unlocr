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
    /// Page span to OCR, 1-based inclusive `(first, last)`. None = all pages. Maps
    /// to pdftoppm `-f`/`-l`, so a subset rasterizes only those pages (not the whole
    /// PDF then filtered). Caller validates `first >= 1` and `last >= first`.
    pub pages: Option<(u32, u32)>,
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
/// The caller owns writing the markdown to disk: the CLI writes `{stem}.md`,
/// the GUI keeps it in memory / its own store. That keeps the lib free of an
/// output-dir concept.
///
/// Spawns llama-server and kills it on drop (server::Server's Drop), so the
/// success path does not orphan it.
///
/// Returns the assembled markdown and, when `opts.keep_images` is set, the
/// directory the page PNGs were kept in (None otherwise) so a caller can surface
/// it instead of leaking the images with no way to find them. This is the GUI's
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
) -> Res<(String, Option<PathBuf>)>
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

    let (md, kept) = ocr_pages(&srv, &tools.pdftoppm, input, opts, on_progress, &|| false)?;

    // Drop kills llama-server. `kept` is Some(dir) only when keep_images is set;
    // bubble it up so the caller can report where the PNGs went.
    drop(srv);
    Ok((md, kept))
}

/// Rasterize a PDF's pages to PNGs in a content-keyed cache dir, reusing the
/// cached PNGs on a repeat call instead of re-running pdftoppm. Returns the page
/// PNG paths in order. Used by the GUI preview pane; the OCR path keeps its own
/// ephemeral tempdir (`ocr_pages`), so CLI behavior is unchanged.
///
/// Cache key = hash(canonical PDF path + mtime + dpi): a changed file (different
/// mtime) or a different dpi misses and re-renders. `cache_root` is the resolved
/// unlocr cache dir; previews live under `<cache_root>/previews/<key>/`.
// ponytail: unbounded cache (no eviction). It is under the OS cache dir, so the
// user/OS can clear it; add an LRU/size cap here if the previews dir grows.
pub fn render_pages(pdftoppm: &Path, pdf: &Path, dpi: u32, cache_root: &Path) -> Res<Vec<PathBuf>> {
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
    if let Err(e) = pdf::rasterize_range(pdftoppm, pdf, &dir, dpi, Some((page, page))) {
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
/// progress event per page. Returns the page-delimited markdown and, when
/// `opts.keep_images` is set, the directory the page PNGs were kept in (so the
/// CLI can report it; the handle is leaked there to keep the files on disk).
/// Shared by run_ocr_job (lib) and, after bite 2, the CLI's ocr::run_pdf.
pub fn ocr_pages<S, P>(
    srv: &S,
    pdftoppm: &Path,
    input: &Path,
    opts: &OcrOptions,
    on_progress: &mut P,
    should_cancel: &dyn Fn() -> bool,
) -> Res<(String, Option<PathBuf>)>
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
            },
        )?;
        // push_page writes page idx+1, so pass the real page number minus one.
        push_page(&mut md, page_num - 1, text.trim());
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
        assert!(o.pages.is_none());
    }

    #[test]
    fn resolve_output_path_cases() {
        let dir = Path::new("/out");
        // Default: {stem}.md under out_dir.
        assert_eq!(
            resolve_output_path(dir, None, "doc"),
            Path::new("/out/doc.md")
        );
        // Relative name, no extension -> append .md, joined under out_dir.
        assert_eq!(
            resolve_output_path(dir, Some(Path::new("report")), "doc"),
            Path::new("/out/report.md")
        );
        // Relative name with .md -> preserved.
        assert_eq!(
            resolve_output_path(dir, Some(Path::new("report.md")), "doc"),
            Path::new("/out/report.md")
        );
        // Non-.md extension -> left as typed (caller's choice).
        assert_eq!(
            resolve_output_path(dir, Some(Path::new("report.txt")), "doc"),
            Path::new("/out/report.txt")
        );
        // Absolute path -> used verbatim, ignoring out_dir (ext appended when missing).
        assert_eq!(
            resolve_output_path(dir, Some(Path::new("/tmp/x")), "doc"),
            Path::new("/tmp/x.md")
        );
        assert_eq!(
            resolve_output_path(dir, Some(Path::new("/tmp/x.md")), "doc"),
            Path::new("/tmp/x.md")
        );
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
            None,
            &OcrOptions::default(),
            &mut progress,
        );
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("not a file"), "unexpected error: {msg}");
    }

    #[test]
    fn preview_cache_dir_is_deterministic_and_dpi_keyed() {
        // Same (file state, dpi) -> same dir; a different dpi -> different dir.
        // No pdftoppm: this locks the cache keying that decides hit vs re-render.
        let tmp = tempfile::tempdir().expect("tmp");
        let pdf = tmp.path().join("doc.pdf");
        std::fs::write(&pdf, b"%PDF-1.4 stub").expect("write stub");
        let root = tmp.path();

        let a = preview_cache_dir(&pdf, 144, root);
        let b = preview_cache_dir(&pdf, 144, root);
        let c = preview_cache_dir(&pdf, 72, root);
        assert_eq!(a, b, "same inputs must key to the same dir");
        assert_ne!(a, c, "different dpi must key to a different dir");
        assert!(a.starts_with(root.join("previews")));
    }

    /// EH-0003 acceptance 2: exercise the ocr:// state sequence (ServerReady ->
    /// Page 1 -> Page 2 ...) without a live desktop session.
    ///
    /// Strategy:
    ///   1. Spin up a minimal HTTP stub on a free port that returns a valid
    ///      OpenAI-style chat-completion response for any POST request.
    ///   2. Create a Server::for_test(port) pointing at it (dummy child, real port).
    ///   3. Rasterize a two-page fixture PDF via pdftoppm (skip on hosts without it).
    ///   4. Run ocr_pages and capture every Progress event in order.
    ///   5. Assert: exactly 2 Page events, page numbers 1 then 2, total always 2.
    ///
    /// This proves: listeners would receive events in the correct state order
    /// (download events are emitted by model::ensure_with_progress before
    /// run_ocr_job calls on_progress(ServerReady); the ServerReady -> Page
    /// subsequence is fully exercised here).
    #[test]
    fn ocr_state_sequence_ordering() {
        // Skip on hosts where pdftoppm is not installed (CI without poppler, etc.).
        if std::process::Command::new("pdftoppm")
            .arg("-v")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            eprintln!("skipping ocr_state_sequence_ordering: pdftoppm not on PATH");
            return;
        }

        // --- 1. Stub HTTP server + 2. Server::for_test ------------------------
        let stub_port = spawn_stub_ocr_server();
        let srv = server::Server::for_test(stub_port).expect("for_test");

        // --- 3. Fixture PDF + pdftoppm ----------------------------------------
        let pdf_dir = tempfile::tempdir().expect("tmp pdf dir");
        let pdf_path = pdf_dir.path().join("fixture.pdf");
        std::fs::write(&pdf_path, two_page_pdf_bytes()).expect("write fixture pdf");

        // --- 4. Run ocr_pages + capture Progress events -----------------------
        let pdftoppm_bin = std::path::Path::new("pdftoppm");
        let opts = OcrOptions::default();
        let mut events: Vec<Progress> = Vec::new();
        let result = ocr_pages(
            &srv,
            pdftoppm_bin,
            &pdf_path,
            &opts,
            &mut |p: Progress| {
                events.push(p);
            },
            &|| false,
        );
        assert!(result.is_ok(), "ocr_pages failed: {:?}", result.err());

        // --- 5. Assert ordering -----------------------------------------------
        // Filter to Page events only (Download events are from model::ensure_with_progress
        // which is not called by ocr_pages; ServerReady is emitted by run_ocr_job
        // before calling ocr_pages). From ocr_pages we expect exactly 2 Page events.
        let page_events: Vec<(usize, usize)> = events
            .iter()
            .filter_map(|e| {
                if let Progress::Page { page, total } = e {
                    Some((*page, *total))
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(
            page_events.len(),
            2,
            "expected 2 Page events, got {:?}",
            page_events
        );
        assert_eq!(
            page_events[0],
            (1, 2),
            "first event should be Page{{page:1, total:2}}"
        );
        assert_eq!(
            page_events[1],
            (2, 2),
            "second event should be Page{{page:2, total:2}}"
        );
        // ocr_pages emits Page plus PartialText (streamed OCR text). The stub
        // replies with a plain JSON completion (no SSE framing); ocr_via_stream's
        // non-streaming fallback delivers that text via on_token, so we expect one
        // PartialText per page carrying the stub's content. No other variant.
        for ev in &events {
            assert!(
                matches!(ev, Progress::Page { .. } | Progress::PartialText { .. }),
                "ocr_pages emitted unexpected event variant: {ev:?}"
            );
        }
        let partials: Vec<(usize, &str)> = events
            .iter()
            .filter_map(|e| match e {
                Progress::PartialText { page, chunk } => Some((*page, chunk.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            partials,
            vec![(1, "# page text"), (2, "# page text")],
            "expected one PartialText per page from the non-SSE fallback"
        );
    }

    #[test]
    fn render_pages_returns_cached_pngs_without_pdftoppm() {
        // Cache-hit path: seed the keyed dir with page PNGs, then render_pages must
        // return them sorted by page number and never run pdftoppm (a bogus binary
        // path proves it is not invoked on a hit).
        let tmp = tempfile::tempdir().expect("tmp");
        let pdf = tmp.path().join("doc.pdf");
        std::fs::write(&pdf, b"%PDF-1.4 stub").expect("write stub");
        let root = tmp.path();

        let dir = preview_cache_dir(&pdf, 144, root);
        std::fs::create_dir_all(&dir).expect("mk cache dir");
        // Out of order on disk; render_pages must return them 1,2,10.
        for n in [10u32, 1, 2] {
            std::fs::write(dir.join(format!("page-{n}.png")), b"\x89PNG").expect("seed png");
        }

        let bogus = Path::new("/nonexistent/pdftoppm");
        let pages = render_pages(bogus, &pdf, 144, root).expect("cache hit");
        let names: Vec<_> = pages
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["page-1.png", "page-2.png", "page-10.png"]);
    }

    /// A page range OCRs only the selected pages AND labels them with the real page
    /// number, not the loop index. Selecting page 2 of a 2-page PDF must produce a
    /// single `<!-- page 2 -->` block (regression guard for the base+i numbering).
    #[test]
    fn ocr_pages_honors_page_range() {
        if std::process::Command::new("pdftoppm")
            .arg("-v")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            eprintln!("skipping ocr_pages_honors_page_range: pdftoppm not on PATH");
            return;
        }

        let stub_port = spawn_stub_ocr_server();
        let srv = server::Server::for_test(stub_port).expect("for_test");

        let pdf_dir = tempfile::tempdir().expect("tmp pdf dir");
        let pdf_path = pdf_dir.path().join("fixture.pdf");
        std::fs::write(&pdf_path, two_page_pdf_bytes()).expect("write fixture pdf");

        let opts = OcrOptions {
            pages: Some((2, 2)),
            ..OcrOptions::default()
        };
        let mut events: Vec<Progress> = Vec::new();
        let (md, _kept) = ocr_pages(
            &srv,
            std::path::Path::new("pdftoppm"),
            &pdf_path,
            &opts,
            &mut |p: Progress| events.push(p),
            &|| false,
        )
        .expect("ocr_pages with range");

        // Exactly one page OCR'd, and it is reported/labeled as page 2.
        let page_events: Vec<(usize, usize)> = events
            .iter()
            .filter_map(|e| match e {
                Progress::Page { page, total } => Some((*page, *total)),
                _ => None,
            })
            .collect();
        assert_eq!(
            page_events,
            vec![(2, 1)],
            "should OCR only page 2 of 1 selected"
        );
        assert!(
            md.contains("<!-- page 2 -->"),
            "markdown must carry the real page number: {md}"
        );
        assert!(
            !md.contains("<!-- page 1 -->"),
            "page 1 must not be OCR'd: {md}"
        );
    }

    /// A `should_cancel` that is true on entry aborts before OCRing any page: no Page
    /// events, and an Err the GUI's run_ocr remaps to "stopped". Guards the page-loop
    /// cancellation check that makes Stop responsive (esp. for the remote backend,
    /// which has no llama-server pid to kill).
    #[test]
    fn ocr_pages_aborts_when_cancelled() {
        if std::process::Command::new("pdftoppm")
            .arg("-v")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            eprintln!("skipping ocr_pages_aborts_when_cancelled: pdftoppm not on PATH");
            return;
        }

        let stub_port = spawn_stub_ocr_server();
        let srv = server::Server::for_test(stub_port).expect("for_test");

        let pdf_dir = tempfile::tempdir().expect("tmp pdf dir");
        let pdf_path = pdf_dir.path().join("fixture.pdf");
        std::fs::write(&pdf_path, two_page_pdf_bytes()).expect("write fixture pdf");

        let opts = OcrOptions::default();
        let mut events: Vec<Progress> = Vec::new();
        let result = ocr_pages(
            &srv,
            std::path::Path::new("pdftoppm"),
            &pdf_path,
            &opts,
            &mut |p: Progress| events.push(p),
            &|| true,
        );

        assert!(
            result.is_err(),
            "cancelled run must return Err, got {result:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(e, Progress::Page { .. })),
            "no page should be OCR'd when cancelled on entry: {events:?}"
        );
    }

    /// Spawn a throwaway HTTP server that returns a valid OpenAI chat-completion for
    /// any POST and return its port. Used by the ocr_pages tests so they exercise the
    /// real rasterize+request loop without a live llama-server. Drains the request
    /// body before replying so ureq never sees a partial read.
    fn spawn_stub_ocr_server() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub server");
        let port = listener.local_addr().expect("local_addr").port();
        let resp_body = serde_json::json!({
            "choices": [{ "message": { "content": "# page text" } }]
        })
        .to_string();
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            resp_body.len(),
            resp_body,
        );
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader, Write};
            for stream in listener.incoming() {
                let Ok(s) = stream else { break };
                let mut reader = BufReader::new(s.try_clone().expect("clone socket"));
                let mut writer = s;
                let mut content_length: usize = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let trimmed = line.trim_end_matches(['\r', '\n']);
                    if trimmed.is_empty() {
                        break;
                    }
                    if trimmed.to_ascii_lowercase().starts_with("content-length:") {
                        if let Some(v) = trimmed.split_once(':').map(|x| x.1) {
                            content_length = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0u8; content_length];
                let _ = std::io::Read::read_exact(&mut reader, &mut body);
                let _ = Write::write_all(&mut writer, http_response.as_bytes());
            }
        });
        port
    }

    /// Minimal valid 2-page PDF (Catalog -> Pages with two text pages + computed
    /// xref). Inlined so the tests add no binary fixture to the repo.
    fn two_page_pdf_bytes() -> Vec<u8> {
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
        buf.push_str("xref\n0 8\n0000000000 65535 f \n");
        for off in &offsets {
            buf.push_str(&format!("{:010} 00000 n \n", off));
        }
        buf.push_str(&format!(
            "trailer<</Size 8/Root 1 0 R>>\nstartxref\n{xref_start}\n%%EOF\n",
        ));
        buf.into_bytes()
    }
}
