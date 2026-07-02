use std::path::PathBuf;

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
    /// DRY sampler strength (llama.cpp `dry_multiplier`), sent per request. None
    /// omits every DRY field (remote body unchanged); the front ends default it
    /// to 1.0 on the managed-local llama-server path (any GGUF quant), where it
    /// stands in for the no-repeat-ngram logits processor upstream Python uses
    /// for loop prevention. 0.0 is a valid explicit "off". When set, the shared
    /// request builder also sends `dry_allowed_length: 4` (see `apply_sampling`).
    pub dry_multiplier: Option<f32>,
    /// llama.cpp `dry_base`: growth rate of the DRY penalty past
    /// `dry_allowed_length` (server default 1.75). Only reaches the wire when
    /// `dry_multiplier` is also set (see `apply_sampling`); a base with DRY
    /// disabled is inert. Opt-in only: unlike `repeat_penalty`/`dry_multiplier`,
    /// no local default is injected for this one, since it's newer/less
    /// battle-tested.
    pub dry_base: Option<f32>,
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
            prompt: "document parsing.".to_string(),
            port: 0,
            model_dir: None,
            keep_images: false,
            image_max_tokens: None,
            chat_template: None,
            repeat_penalty: None,
            dry_multiplier: None,
            dry_base: None,
            pages: None,
        }
    }
}

impl OcrOptions {
    /// Reject out-of-range numerics before they reach pdftoppm / llama-server.
    ///
    /// Single shared sink: both the CLI (`main.rs::run`) and the GUI
    /// (`cmd_run/ocr/validation.rs`) route through here, so the guard logic
    /// (and its error wording) lives in one place instead of two copies that
    /// drift. `dpi == 0` makes pdftoppm emit no pages, `image_max_tokens == 0`
    /// is rejected by llama-server at spawn, `max_tokens == 0` caps generation
    /// to nothing, a non-finite / <= 0 repeat penalty drives the sampler into
    /// degenerate output, and a page range with `first == 0` or `last < first`
    /// is meaningless (1-based). `pages` may carry an open upper bound
    /// (`last == None` -> first..end), which is allowed.
    pub fn validate(&self) -> crate::Res<()> {
        if self.dpi == 0 {
            return Err("dpi must be greater than 0".into());
        }
        if self.max_tokens == 0 {
            return Err("max_tokens must be greater than 0".into());
        }
        if self.image_max_tokens == Some(0) {
            return Err("image_max_tokens must be greater than 0".into());
        }
        if let Some(rp) = self.repeat_penalty {
            if !rp.is_finite() || rp <= 0.0 {
                return Err("repeat_penalty must be a finite value greater than 0".into());
            }
        }
        // Unlike repeat_penalty, 0.0 is meaningful here: it is the sampler's
        // own "disabled" value, so an explicit 0 is the documented way to turn
        // DRY off while leaving the field present.
        if let Some(dm) = self.dry_multiplier {
            if !dm.is_finite() || dm < 0.0 {
                return Err("dry_multiplier must be a finite value of 0 or greater".into());
            }
        }
        // Unlike dry_multiplier, 0 has no "off" meaning for a DRY base: the
        // exponential growth formula requires a positive base.
        if let Some(db) = self.dry_base {
            if !db.is_finite() || db <= 0.0 {
                return Err("dry_base must be a finite value greater than 0".into());
            }
        }
        if let Some((first, last)) = self.pages {
            if first == 0 {
                return Err("page range is 1-based; first page 0 is not valid".into());
            }
            if let Some(last) = last {
                if last < first {
                    return Err(format!("page range is reversed: {first}-{last}").into());
                }
            }
        }
        Ok(())
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
pub fn parse_output_mode(s: &str) -> crate::Res<OutputMode> {
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
    /// One page rasterized+OCR'd. `page` is the real 1-based page number
    /// (accounts for a `--pages` offset), `total` is the number of pages being
    /// processed THIS run (the rendered subset size, not the document's page
    /// count: with `--pages 5-10` on a 100-page PDF, `total` is 6).
    Page { page: usize, total: usize },
    /// One page rasterized (PDF->PNG) so far, fired while `pdftoppm` is still
    /// running and before any OCR starts. `page` is the real 1-based page
    /// number (accounts for a `--pages` offset). `total` is `Some` when the
    /// run's page count is known upfront (an explicit `--pages a-b` range, or
    /// a best-effort `pdfinfo` probe for the whole-document case); `None` when
    /// neither is available, in which case a UI shows a running count with no
    /// denominator.
    Rasterizing { page: usize, total: Option<usize> },
    /// One streaming token chunk emitted during OCR of a page. `page` is 1-based.
    /// The GUI appends `chunk` to the live transcript; the CLI may ignore it.
    PartialText { page: usize, chunk: String },
}
