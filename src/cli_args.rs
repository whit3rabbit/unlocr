use crate::Res;
use clap::{Parser, ValueEnum};
use std::path::PathBuf;

/// HF repo of the full (non-GGUF) DeepSeek-OCR model. Served by vLLM, not
/// llama.cpp; `--gpu` points the remote endpoint at a local vLLM instance
/// serving this. See README "Run the full model on GPU" + colab/ notebook.
pub const UNLIMITED_OCR_REPO: &str = "baidu/Unlimited-OCR";
/// Default base URL of a local `vllm serve` OpenAI server (`--gpu` shortcut).
pub const VLLM_LOCAL_URL: &str = "http://localhost:8000";

/// Command-line arguments for the unlocr application.
#[derive(Parser, Debug)]
#[command(
    name = "unlocr",
    version,
    about = "OCR PDFs to markdown via Unlimited-OCR + llama.cpp"
)]
pub struct Args {
    /// Subcommand to execute (e.g. doctor, preflight)
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Input PDF/image file(s), folder(s), or glob pattern(s) (quote globs so
    /// the binary expands them; useful on Windows where PowerShell does not)
    pub inputs: Vec<PathBuf>,

    /// Recurse into subdirectories when an input is a folder
    #[arg(long)]
    pub recursive: bool,

    /// Read additional PDF/image paths from a text file (one per line; blank
    /// lines and lines starting with # are skipped)
    #[arg(long)]
    pub from_list: Option<PathBuf>,

    /// Output directory for the .md files (default: current dir)
    #[arg(long, default_value = ".")]
    pub out: PathBuf,

    /// Output file path for the single-input case (e.g. report.md). Note: if a
    /// relative path is provided, it is joined under the `--out` directory rather
    /// than overriding it. An absolute path is used verbatim. `.md` is appended
    /// when no extension is given. Rejected with multiple inputs (use --out <DIR>).
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,

    /// Output layout: `single` writes one `{stem}.md` (default); `pages` writes a
    /// `{stem}/page-N.md` folder (one file per page); `both` writes the combined
    /// file and the folder. `--output`/`-o` is ignored for the folder name in
    /// `pages`/`both` (the folder uses the input stem; a warning is printed).
    #[arg(long, value_enum, default_value_t = OutputModeArg::Single)]
    pub output_mode: OutputModeArg,

    /// Quality tier, an alias for a model quant:
    /// best=BF16 (5.47GB), good=Q8_0 (2.91GB, default), less=Q4_K_M (1.82GB).
    #[arg(long, value_enum, default_value_t = Quality::Good)]
    pub quality: Quality,

    /// Exact quant tag (matches Unlimited-OCR-<QUANT>.gguf), e.g. Q6_K, IQ4_XS.
    /// Overrides --quality when set.
    #[arg(long)]
    pub quant: Option<String>,

    /// Max tokens generated per page (caps runaway generation on dense pages)
    #[arg(long, default_value_t = 4096)]
    pub max_tokens: u32,

    /// Task preset that picks the OCR prompt: markdown (clean markdown, default),
    /// grounding (markdown + layout coordinates), free (plain text), figure (parse a
    /// chart/figure). Ignored when --prompt is set.
    #[arg(long, value_enum, default_value_t = Task::Markdown)]
    pub task: Task,

    /// OCR prompt sent with every page. Overrides --task when set.
    #[arg(long)]
    pub prompt: Option<String>,

    /// Pages to OCR: a single page ("5") or an inclusive 1-based range ("5-9").
    /// Omit to OCR all pages. Applies to every input PDF.
    #[arg(long)]
    pub pages: Option<String>,

    /// Rasterization DPI passed to pdftoppm (size of the PNG handed to the model)
    #[arg(long, default_value_t = 144)]
    pub dpi: u32,

    /// Cap on vision tokens per image (--image-max-tokens). DeepSeek-OCR's
    /// base/large detail knob: higher = finer recognition, slower + more VRAM.
    /// Omit to let the model use its default. Local mode only.
    #[arg(long)]
    pub image_max_tokens: Option<u32>,

    /// Named chat template forwarded to llama-server's --chat-template (e.g.
    /// deepseek-ocr). Omit to use the template baked into the model. Local mode only.
    #[arg(long)]
    pub chat_template: Option<String>,

    /// Sampling repetition penalty (e.g. 1.3) sent with every page. Helps escape
    /// the infinite-loop output some quants (notably Q4_K_M) hit on dense pages.
    /// Defaults to 1.3 on the local GGUF path; pass a value to override.
    /// Inert/omitted for remote (`--endpoint`/`--gpu`) mode, which does not
    /// exhibit the quant loop.
    #[arg(long)]
    pub repeat_penalty: Option<f32>,

    /// DRY sampler strength (llama.cpp `dry_multiplier`) sent with every page.
    /// Penalizes repeated *sequences* (the analog of upstream's no-repeat-ngram
    /// processor), catching the loops a mild repeat penalty cannot. Defaults to
    /// 1.0 on the local GGUF path; pass 0 to disable. Inert/omitted for remote
    /// (`--endpoint`/`--gpu`) mode, whose server rejects llama.cpp-only fields.
    #[arg(long)]
    pub dry_multiplier: Option<f32>,

    /// Base for llama.cpp's DRY exponential penalty growth (`dry_base`), sent
    /// only when --dry-multiplier is also set (a dry_base with DRY disabled is
    /// inert). Server default (1.75) applies when omitted; no local default is
    /// injected here. Higher values ramp the penalty more aggressively past
    /// dry_allowed_length. Inert/omitted for remote (`--endpoint`/`--gpu`) mode.
    #[arg(long)]
    pub dry_base: Option<f32>,

    /// DRY allowed run length (`dry_allowed_length`): tokens DRY tolerates before
    /// penalizing repeats. Sent only when --dry-multiplier is also set. Omitted =
    /// the local default of 4; pass 2 (the community anti-loop value) for dense
    /// math pages that still loop. Inert for remote (`--endpoint`/`--gpu`) mode.
    #[arg(long)]
    pub dry_allowed_length: Option<u32>,

    /// DRY scan window (`dry_penalty_last_n`): -1 = whole context (the anti-loop
    /// value), 0 = disabled, >0 = a fixed window. Sent only when --dry-multiplier
    /// is also set; omitted = server default. Inert for remote mode.
    #[arg(long, allow_hyphen_values = true)]
    pub dry_penalty_last_n: Option<i32>,

    /// Sampling temperature (e.g. 0.2 for slight variability). Defaults to 0 for
    /// deterministic OCR output (matches the historical fixed behavior and the
    /// upstream README's recommendation). Unlike the llama.cpp-only DRY/repeat-
    /// penalty knobs, this is a standard OpenAI field: it applies to both local
    /// and remote (`--endpoint`/`--gpu`) mode.
    #[arg(long)]
    pub temperature: Option<f32>,

    /// Path to llama-server (default: PATH / Homebrew)
    #[arg(long)]
    pub llama_bin: Option<PathBuf>,

    /// Model cache directory (default: per-OS cache dir)
    #[arg(long)]
    pub model_dir: Option<PathBuf>,

    /// Use this model GGUF directly, skipping the HF download and the
    /// Unlimited-OCR-<quant>.gguf naming convention. With this set,
    /// --quant/--quality/--model-dir no longer select the model file.
    #[arg(long)]
    pub model: Option<PathBuf>,

    /// Projector (mmproj) GGUF override. Omit to use the standard
    /// cached/downloaded projector. Requires --model.
    #[arg(long)]
    pub mmproj: Option<PathBuf>,

    /// Port for llama-server (0 = auto-pick a free port)
    #[arg(long, default_value_t = 0)]
    pub port: u16,

    /// Keep the intermediate page PNGs instead of deleting them
    #[arg(long)]
    pub keep_images: bool,

    /// Remote OpenAI-compatible endpoint base URL (e.g. http://host:8080). When
    /// set, skips the local llama-server spawn and model download: pages are
    /// rasterized locally and OCR'd against this endpoint. Only llama.cpp
    /// (PR #17400), vLLM, and SGLang are known to support these OCR models;
    /// --quant/--quality/--llama-bin/--port are ignored in this mode.
    #[arg(long)]
    pub endpoint: Option<String>,

    /// Bearer token for --endpoint. Prefer the UNLOCR_API_KEY env var so the key
    /// does not leak via the process list / shell history.
    #[arg(long)]
    pub endpoint_key: Option<String>,

    /// Model name to send in the request body (required by litellm/vLLM gateways;
    /// omit for a bare remote llama-server).
    #[arg(long)]
    pub endpoint_model: Option<String>,

    /// Run the full (non-GGUF) Unlimited-OCR model on GPU via a local vLLM server.
    /// Shortcut: when set and --endpoint is unset, defaults --endpoint to
    /// http://localhost:8000 and --endpoint-model to baidu/Unlimited-OCR.
    /// Start the server first (see README "Run the full model on GPU"): vllm serve
    /// baidu/Unlimited-OCR. A Colab notebook (colab/) wires this end to end.
    #[arg(long)]
    pub gpu: bool,
}

/// Subcommands available in the unlocr CLI.
#[derive(clap::Subcommand, Debug, Clone)]
pub enum Commands {
    /// Validate system dependencies, model files, RAM, and disk space
    Doctor {
        /// Path to llama-server (default: PATH / Homebrew)
        #[arg(long)]
        llama_bin: Option<PathBuf>,

        /// Model cache directory (default: per-OS cache dir)
        #[arg(long)]
        model_dir: Option<PathBuf>,

        /// Exact quant tag (matches Unlimited-OCR-<QUANT>.gguf), e.g. Q8_0, Q4_K_M.
        #[arg(long, default_value = "Q8_0")]
        quant: String,
    },
    /// Alias for doctor
    Preflight {
        /// Path to llama-server (default: PATH / Homebrew)
        #[arg(long)]
        llama_bin: Option<PathBuf>,

        /// Model cache directory (default: per-OS cache dir)
        #[arg(long)]
        model_dir: Option<PathBuf>,

        /// Exact quant tag (matches Unlimited-OCR-<QUANT>.gguf), e.g. Q8_0, Q4_K_M.
        #[arg(long, default_value = "Q8_0")]
        quant: String,
    },
}

/// Quality tiers for selecting the GGUF model quantization level. A friendly
/// 3-way alias over the full 13-quant lineup in `unlocr::model::KNOWN_QUANTS`
/// (`tier` field there cross-references these same 3 tags) -- keep the two in
/// sync if a tier's underlying quant ever changes.
#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum Quality {
    /// BF16 (5.47GB), highest fidelity
    Best,
    /// Q8_0 (2.91GB), near-lossless
    Good,
    /// Q4_K_M (1.82GB), smallest/fastest
    Less,
}

impl Quality {
    /// Returns the exact quantization tag corresponding to this quality tier.
    pub fn quant(self) -> &'static str {
        match self {
            Quality::Best => "BF16",
            Quality::Good => "Q8_0",
            Quality::Less => "Q4_K_M",
        }
    }
}

/// Prompt presets the model understands. Maps to one of DeepSeek-OCR's task
/// prompts; `--prompt` overrides the resolved string for full control.
#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum Task {
    /// Clean structured markdown (headings, lists, tables), no coordinates. The default.
    Markdown,
    /// Grounded markdown: each line tagged (text/title/...) with a bounding box.
    /// Use when you want layout coordinates (e.g. to rebuild HTML).
    Grounding,
    /// Plain-text OCR with no layout/markdown structure.
    Free,
    /// Parse a chart/figure into a structured description.
    Figure,
}

impl Task {
    /// Returns the prompt string corresponding to this task preset.
    pub fn prompt(self) -> &'static str {
        match self {
            // Keep in sync with OcrOptions::default().prompt (the no-flags default).
            Task::Markdown => "document parsing.",
            Task::Grounding => "<|grounding|>Convert the document to markdown.",
            Task::Free => "Free OCR.",
            Task::Figure => "Parse the figure.",
        }
    }
}

/// On-disk layout for OCR output. clap value-enum mirroring the lib's clap-free
/// `unlocr::OutputMode` (kept separate so the lib never depends on clap).
#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum OutputModeArg {
    /// One `{stem}.md` with every page concatenated. The default.
    Single,
    /// A `{stem}/page-N.md` folder, one file per page.
    Pages,
    /// Both the combined `{stem}.md` and the per-page folder.
    Both,
}

impl OutputModeArg {
    /// Convert to the lib's clap-free enum.
    pub fn to_mode(self) -> unlocr::OutputMode {
        match self {
            OutputModeArg::Single => unlocr::OutputMode::Single,
            OutputModeArg::Pages => unlocr::OutputMode::Pages,
            OutputModeArg::Both => unlocr::OutputMode::Both,
        }
    }
}

impl Args {
    /// The OCR prompt to use: explicit `--prompt` wins; otherwise the `--task`
    /// preset. Mirrors the --quant-over---quality override pattern.
    pub fn resolved_prompt(&self) -> String {
        self.prompt
            .clone()
            .unwrap_or_else(|| self.task.prompt().to_string())
    }

    /// Apply the `--gpu` shortcut: fill the remote-endpoint defaults (local vLLM +
    /// the full DeepSeek-OCR model) only where unset, so an explicit `--endpoint`/
    /// `--endpoint-model` still wins. No-op unless `--gpu` is passed.
    pub fn apply_gpu_defaults(&mut self) {
        if !self.gpu {
            return;
        }
        if self.endpoint.is_none() {
            self.endpoint = Some(VLLM_LOCAL_URL.to_string());
        }
        if self.endpoint_model.is_none() {
            self.endpoint_model = Some(UNLIMITED_OCR_REPO.to_string());
        }
    }

    /// Parse `--pages` into a 1-based inclusive `(first, last)` range, or None when
    /// the flag is absent (= all pages). Accepts "5" (single) and "5-9" (range).
    /// Rejects 0, reversed ranges, and non-numeric input so a bad flag fails before
    /// any spawn rather than silently OCR'ing the wrong pages.
    pub fn resolved_pages(&self) -> Res<Option<(u32, Option<u32>)>> {
        Ok(match &self.pages {
            None => None,
            Some(s) => Some(parse_pages(s)?),
        })
    }
}

/// Pure parser for the `--pages` value. Split out for unit testing. The CLI always
/// yields a closed range (last is `Some`); the open upper bound only exists in the
/// GUI path. Single "5" -> (5, Some(5)); "5-9" -> (5, Some(9)).
pub fn parse_pages(s: &str) -> Res<(u32, Option<u32>)> {
    let s = s.trim();
    let parse_one = |p: &str| -> Res<u32> {
        let n: u32 = p
            .trim()
            .parse()
            .map_err(|_| format!("invalid page number: {p:?}"))?;
        if n == 0 {
            return Err("page numbers are 1-based; 0 is not valid".into());
        }
        Ok(n)
    };
    let (first, last) = match s.split_once('-') {
        Some((a, b)) => (parse_one(a)?, parse_one(b)?),
        None => {
            let n = parse_one(s)?;
            (n, n)
        }
    };
    if last < first {
        return Err(format!("page range is reversed: {first}-{last}").into());
    }
    Ok((first, Some(last)))
}
