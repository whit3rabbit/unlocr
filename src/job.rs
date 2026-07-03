use crate::inputs::{is_image, is_pdf, sniff_image_mime};
use crate::model;
use crate::options::{OcrOptions, OcrOutput, Progress};
use crate::output::{push_page, strip_layout_annotations, AnnotationStripper};
use crate::pdf;
use crate::preflight;
use crate::server;
use crate::Res;
use base64::Engine;
use std::path::{Path, PathBuf};

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

    // With a page range, the first rasterized page is the range's start, not page 1
    // (also needed below for the OCR loop's page numbering). Meaningless for a
    // single-image input (always page 1), but harmless to compute either way.
    let base = opts.pages.map(|(f, _)| f as usize).unwrap_or(1);

    // Two input shapes share the rest of this function: a PDF gets rasterized to
    // one PNG per page (as before); a single recognized image is read+sniffed once
    // and treated as a synthetic one-page "PDF" so every line below (progress,
    // streaming, annotation stripping, push_page/pages_text, keep_images) runs
    // unforked. `page_mime` is computed once here so the per-page loop's data_uri
    // no longer hardcodes "image/png" for a real image input.
    let (pages, page_mime): (Vec<PathBuf>, &'static str) = if is_pdf(input) {
        // Total for the Rasterizing progress event: an explicit `--pages a-b` range
        // already tells us the count; otherwise fall back to a best-effort `pdfinfo`
        // probe (None if pdfinfo isn't resolvable, in which case the event carries no
        // denominator).
        let raster_total = match opts.pages {
            Some((_, Some(last))) => Some(last as usize - base + 1),
            Some((first, None)) => pdf::total_pages(pdftoppm, input)
                // `first` can exceed the real page count (e.g. `--pages 50-` on a
                // shorter PDF); rasterize_range then renders nothing and the empty-
                // pages check below errors out. saturating_sub avoids an underflow
                // panic (debug) / wraparound (release) computing this progress total.
                .map(|n| (n as usize).saturating_sub(first as usize) + 1),
            None => pdf::total_pages(pdftoppm, input).map(|n| n as usize),
        };
        let pages = pdf::rasterize_range(
            pdftoppm,
            input,
            tmp.path(),
            opts.dpi,
            opts.pages,
            Some(&mut |count: usize| {
                on_progress(Progress::Rasterizing {
                    page: base + count - 1,
                    total: raster_total,
                });
            }),
        )?;
        (pages, "image/png") // pdftoppm always emits PNG
    } else if is_image(input) {
        // No pdftoppm, no dpi/pages meaning (documented no-ops, see options.rs).
        // Read once, sniff once, and reject BEFORE this reaches the model with a
        // wrong MIME claim: don't trust the extension, verify content.
        let bytes = std::fs::read(input)?;
        let claimed_ext = input
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let sniffed = sniff_image_mime(&bytes).ok_or_else(|| {
            format!(
                "{}: not a recognized image (no PNG/JPEG/TIFF/WEBP/BMP magic bytes found); \
                 refusing to OCR a file whose content does not match an image format",
                input.display()
            )
        })?;
        let ext_matches = match claimed_ext.as_str() {
            "png" => sniffed == "image/png",
            "jpg" | "jpeg" => sniffed == "image/jpeg",
            "tif" | "tiff" => sniffed == "image/tiff",
            "webp" => sniffed == "image/webp",
            "bmp" => sniffed == "image/bmp",
            _ => false,
        };
        if !ext_matches {
            return Err(format!(
                "{}: file extension \".{claimed_ext}\" does not match its content (sniffed as \
                 {sniffed}); refusing to guess",
                input.display()
            )
            .into());
        }
        // One Rasterizing tick for UI consistency with the PDF path.
        on_progress(Progress::Rasterizing {
            page: base,
            total: Some(1),
        });
        // keep_images preserves `tmp` via tmp.keep() below; unlike the PDF path
        // (which renders pages into `tmp` via pdftoppm), a single-image input
        // never otherwise writes into `tmp`, so tmp.keep() would preserve an
        // empty directory. Copy the already-read bytes in when keep_images is
        // set so the kept directory actually contains the image.
        let page_path = if opts.keep_images {
            let dest = tmp.path().join(
                input
                    .file_name()
                    .ok_or_else(|| format!("{}: no file name", input.display()))?,
            );
            std::fs::write(&dest, &bytes)?;
            dest
        } else {
            input.to_path_buf()
        };
        (vec![page_path], sniffed)
    } else {
        return Err(format!(
            "{}: not a PDF or a recognized image ({})",
            input.display(),
            crate::inputs::IMAGE_EXTENSIONS.join("/")
        )
        .into());
    };
    // rasterize_range returns an empty Vec (not an Err) when pdftoppm runs clean
    // but emits nothing. For an OCR run that means nothing was rendered (empty
    // PDF or a page range past EOF): error out rather than write a silent empty
    // file. render_page intentionally keeps empty as a value for out-of-range
    // detection; this run path treats it as a failure.
    if pages.is_empty() {
        return Err("pdftoppm produced no pages".into());
    }
    let n = pages.len();

    // The model emits layout-grounded output (`label [x, y, x, y]content`) that
    // upstream cleans in Python; we port that cleanup here (output.rs). Gate on
    // the model-facing prompt itself instead of a parallel flag: the grounding
    // task preset (CLI Task::Grounding, GUI TASK_PROMPTS.grounding) carries the
    // `<|grounding|>` marker, and a user who typed it explicitly wants raw boxes.
    let strip = !opts.prompt.contains("<|grounding|>");

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
        let data_uri = format!("data:{page_mime};base64,{b64}");
        // Use streaming so the GUI receives PartialText events as tokens arrive.
        // The CLI's on_progress sink ignores PartialText (zero cost), while the
        // Tauri bridge forwards each chunk to the frontend for live appending.
        // When stripping, chunks are line-buffered through the stripper (an
        // annotation prefix can be split across chunks), so the live transcript
        // advances per completed line; the cancel check still runs every chunk.
        let mut stripper = AnnotationStripper::new();
        // Accumulate the already-stripped output as it streams, rather than
        // re-running strip_layout_annotations over the raw text afterward: that
        // both avoids scanning every page twice and guarantees the stored text
        // is byte-identical to what the PartialText stream showed (the two
        // strippers previously disagreed on a trailing newline when the raw
        // text ended in "\n").
        let mut stripped = String::new();
        // Some ImageOcr impls (e.g. a stub that only implements ocr_image) fall
        // back to ImageOcr::ocr_image_stream's default, which never calls
        // on_token at all -- distinguish that from a real call delivering the
        // whole body in one shot (the non-SSE fallback in ocr_via_stream),
        // which DOES call on_token once with an unterminated line and must
        // still flush through `stripper.finish()` below.
        let mut streamed_any = false;
        let raw_text = srv.ocr_image_stream(
            &opts.prompt,
            &data_uri,
            opts.max_tokens,
            opts.repeat_penalty,
            opts.dry_multiplier,
            opts.dry_base,
            &mut |chunk: &str| {
                streamed_any = true;
                let chunk = if strip {
                    let s = stripper.push(chunk);
                    stripped.push_str(&s);
                    s
                } else {
                    chunk.to_string()
                };
                if !chunk.is_empty() {
                    on_progress(Progress::PartialText {
                        page: page_num,
                        chunk,
                    });
                }
                !should_cancel()
            },
            should_cancel,
        )?;
        let text = if strip {
            if streamed_any {
                let tail = stripper.finish();
                if !tail.is_empty() {
                    on_progress(Progress::PartialText {
                        page: page_num,
                        chunk: tail.clone(),
                    });
                }
                stripped.push_str(&tail);
                stripped
            } else {
                // on_token was never called (default ImageOcr::ocr_image_stream
                // fallback): nothing went through the stripper, so strip the raw
                // text directly.
                strip_layout_annotations(&raw_text)
            }
        } else {
            raw_text
        };
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
