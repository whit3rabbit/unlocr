// unlocr: thin CLI wrapping llama.cpp's llama-server to OCR PDFs with the
// Unlimited-OCR (DeepSeek-OCR) model. PDF -> PNG (pdftoppm) -> per-page chat
// completion -> page-delimited markdown.

// The OCR backend (model/pdf/preflight/server) lives in the `unlocr` library
// crate (src/lib.rs). The bin crate is now CLI glue only: Args/clap parsing,
// input expansion, and the bin-only ocr::run_pdf delegator. Using the lib's
// modules keeps one compiled copy of the backend (so a `Server` passed from
// main is the same type the lib's ocr_pages expects) instead of two diverging
// copies compiled into bin and lib separately.
mod ocr;

use clap::{Parser, ValueEnum};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use unlocr::{model, preflight, server};

pub type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// HF repo of the full (non-GGUF) DeepSeek-OCR model. Served by vLLM, not
/// llama.cpp; `--gpu` points the remote endpoint at a local vLLM instance
/// serving this. See README "Run the full model on GPU" + colab/ notebook.
const UNLIMITED_OCR_REPO: &str = "baidu/Unlimited-OCR";
/// Default base URL of a local `vllm serve` OpenAI server (`--gpu` shortcut).
const VLLM_LOCAL_URL: &str = "http://localhost:8000";

#[derive(Parser, Debug)]
#[command(
    name = "unlocr",
    version,
    about = "OCR PDFs to markdown via Unlimited-OCR + llama.cpp"
)]
struct Args {
    /// Subcommand to execute (e.g. doctor, preflight)
    #[command(subcommand)]
    command: Option<Commands>,

    /// Input PDF file(s), folder(s), or glob pattern(s) (quote globs so the
    /// binary expands them; useful on Windows where PowerShell does not)
    inputs: Vec<PathBuf>,

    /// Recurse into subdirectories when an input is a folder
    #[arg(long)]
    recursive: bool,

    /// Read additional PDF paths from a text file (one per line; blank lines
    /// and lines starting with # are skipped)
    #[arg(long)]
    from_list: Option<PathBuf>,

    /// Output directory for the .md files (default: current dir)
    #[arg(long, default_value = ".")]
    out: PathBuf,

    /// Output file path for the single-input case (e.g. report.md). Joined under
    /// --out when relative; an absolute path is used verbatim. `.md` is appended
    /// when no extension is given. Rejected with multiple inputs (use --out <DIR>).
    #[arg(short = 'o', long)]
    output: Option<PathBuf>,

    /// Quality tier, an alias for a model quant:
    /// best=BF16 (5.47GB), good=Q8_0 (2.91GB, default), less=Q4_K_M (1.82GB).
    #[arg(long, value_enum, default_value_t = Quality::Good)]
    quality: Quality,

    /// Exact quant tag (matches Unlimited-OCR-<QUANT>.gguf), e.g. Q6_K, IQ4_XS.
    /// Overrides --quality when set.
    #[arg(long)]
    quant: Option<String>,

    /// Max tokens generated per page (caps runaway generation on dense pages)
    #[arg(long, default_value_t = 4096)]
    max_tokens: u32,

    /// Task preset that picks the OCR prompt: markdown (grounded markdown, default),
    /// free (plain text), figure (parse a chart/figure). Ignored when --prompt is set.
    #[arg(long, value_enum, default_value_t = Task::Markdown)]
    task: Task,

    /// OCR prompt sent with every page. Overrides --task when set.
    #[arg(long)]
    prompt: Option<String>,

    /// Pages to OCR: a single page ("5") or an inclusive 1-based range ("5-9").
    /// Omit to OCR all pages. Applies to every input PDF.
    #[arg(long)]
    pages: Option<String>,

    /// Rasterization DPI passed to pdftoppm (size of the PNG handed to the model)
    #[arg(long, default_value_t = 144)]
    dpi: u32,

    /// Cap on vision tokens per image (--image-max-tokens). DeepSeek-OCR's
    /// base/large detail knob: higher = finer recognition, slower + more VRAM.
    /// Omit to let the model use its default. Local mode only.
    #[arg(long)]
    image_max_tokens: Option<u32>,

    /// Named chat template forwarded to llama-server's --chat-template (e.g.
    /// deepseek-ocr). Omit to use the template baked into the model. Local mode only.
    #[arg(long)]
    chat_template: Option<String>,

    /// Sampling repetition penalty (e.g. 1.1) sent with every page. Helps escape the
    /// infinite-loop output some quants (notably Q4_K_M) hit on dense pages. Omit
    /// for the server default.
    #[arg(long)]
    repeat_penalty: Option<f32>,

    /// Path to llama-server (default: PATH / Homebrew)
    #[arg(long)]
    llama_bin: Option<PathBuf>,

    /// Model cache directory (default: per-OS cache dir)
    #[arg(long)]
    model_dir: Option<PathBuf>,

    /// Use this model GGUF directly, skipping the HF download and the
    /// Unlimited-OCR-<quant>.gguf naming convention. With this set,
    /// --quant/--quality/--model-dir no longer select the model file.
    #[arg(long)]
    model: Option<PathBuf>,

    /// Projector (mmproj) GGUF override. Omit to use the standard
    /// cached/downloaded projector. Requires --model.
    #[arg(long)]
    mmproj: Option<PathBuf>,

    /// Port for llama-server (0 = auto-pick a free port)
    #[arg(long, default_value_t = 0)]
    port: u16,

    /// Keep the intermediate page PNGs instead of deleting them
    #[arg(long)]
    keep_images: bool,

    /// Remote OpenAI-compatible endpoint base URL (e.g. http://host:8080). When
    /// set, skips the local llama-server spawn and model download: pages are
    /// rasterized locally and OCR'd against this endpoint. Only llama.cpp
    /// (PR #17400), vLLM, and SGLang are known to support these OCR models;
    /// --quant/--quality/--llama-bin/--port are ignored in this mode.
    #[arg(long)]
    endpoint: Option<String>,

    /// Bearer token for --endpoint. Prefer the UNLOCR_API_KEY env var so the key
    /// does not leak via the process list / shell history.
    #[arg(long)]
    endpoint_key: Option<String>,

    /// Model name to send in the request body (required by litellm/vLLM gateways;
    /// omit for a bare remote llama-server).
    #[arg(long)]
    endpoint_model: Option<String>,

    /// Run the full (non-GGUF) Unlimited-OCR model on GPU via a local vLLM server.
    /// Shortcut: when set and --endpoint is unset, defaults --endpoint to
    /// http://localhost:8000 and --endpoint-model to baidu/Unlimited-OCR.
    /// Start the server first (see README "Run the full model on GPU"): vllm serve
    /// baidu/Unlimited-OCR. A Colab notebook (colab/) wires this end to end.
    #[arg(long)]
    gpu: bool,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
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

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Quality {
    /// BF16 (5.47GB), highest fidelity
    Best,
    /// Q8_0 (2.91GB), near-lossless
    Good,
    /// Q4_K_M (1.82GB), smallest/fastest
    Less,
}

impl Quality {
    fn quant(self) -> &'static str {
        match self {
            Quality::Best => "BF16",
            Quality::Good => "Q8_0",
            Quality::Less => "Q4_K_M",
        }
    }
}

/// Prompt presets the model understands. Maps to one of DeepSeek-OCR's task
/// prompts; `--prompt` overrides the resolved string for full control.
#[derive(Copy, Clone, Debug, ValueEnum)]
enum Task {
    /// Grounded markdown conversion (headings, layout). The default.
    Markdown,
    /// Plain-text OCR with no layout/markdown structure.
    Free,
    /// Parse a chart/figure into a structured description.
    Figure,
}

impl Task {
    fn prompt(self) -> &'static str {
        match self {
            // Keep in sync with OcrOptions::default().prompt (the no-flags default).
            Task::Markdown => "<|grounding|>Convert the document to markdown.",
            Task::Free => "Free OCR.",
            Task::Figure => "Parse the figure.",
        }
    }
}

impl Args {
    /// The OCR prompt to use: explicit `--prompt` wins; otherwise the `--task`
    /// preset. Mirrors the --quant-over---quality override pattern.
    fn resolved_prompt(&self) -> String {
        self.prompt
            .clone()
            .unwrap_or_else(|| self.task.prompt().to_string())
    }

    /// Apply the `--gpu` shortcut: fill the remote-endpoint defaults (local vLLM +
    /// the full DeepSeek-OCR model) only where unset, so an explicit `--endpoint`/
    /// `--endpoint-model` still wins. No-op unless `--gpu` is passed.
    fn apply_gpu_defaults(&mut self) {
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
    fn resolved_pages(&self) -> Res<Option<(u32, Option<u32>)>> {
        Ok(match &self.pages {
            None => None,
            Some(s) => Some(parse_pages(s)?),
        })
    }
}

/// Pure parser for the `--pages` value. Split out for unit testing. The CLI always
/// yields a closed range (last is `Some`); the open upper bound only exists in the
/// GUI path. Single "5" -> (5, Some(5)); "5-9" -> (5, Some(9)).
fn parse_pages(s: &str) -> Res<(u32, Option<u32>)> {
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

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Res<()> {
    let mut args = Args::parse();

    // --gpu is sugar over the remote-endpoint path: a local vLLM serving the full
    // DeepSeek-OCR model. From here on the normal remote path handles it; no
    // GPU-specific code below.
    args.apply_gpu_defaults();

    if let Some(cmd) = args.command {
        match cmd {
            Commands::Doctor {
                llama_bin,
                model_dir,
                quant,
            }
            | Commands::Preflight {
                llama_bin,
                model_dir,
                quant,
            } => {
                preflight::run_doctor(llama_bin.as_deref(), model_dir, &quant)?;
            }
        }
        return Ok(());
    }

    // Reject out-of-range numerics before they reach pdftoppm / llama-server.
    // Mirrors the GUI run_ocr guards: dpi=0 makes pdftoppm produce no pages,
    // image-max-tokens=0 is rejected by llama-server at spawn, and a repeat
    // penalty <= 0 (or non-finite) drives the sampler into degenerate output.
    if args.dpi == 0 {
        return Err("--dpi must be greater than 0".into());
    }
    if args.image_max_tokens == Some(0) {
        return Err("--image-max-tokens must be greater than 0".into());
    }
    if let Some(rp) = args.repeat_penalty {
        if !rp.is_finite() || rp <= 0.0 {
            return Err("--repeat-penalty must be a finite value greater than 0".into());
        }
    }
    // Surface a bad --pages value before any download/spawn (ocr::run_pdf reparses
    // it per input, but failing here keeps the error off the slow path).
    args.resolved_pages()?;

    // Expand folders, globs, and --from-list into a concrete, deduped PDF list.
    let inputs = expand_inputs(&args.inputs, args.from_list.as_deref(), args.recursive)?;

    // --output names one file; it is ambiguous across a batch. Reject before any
    // download/spawn. Covers both the local and remote paths (both share `inputs`).
    if args.output.is_some() && inputs.len() > 1 {
        return Err("--output names a single file; use --out <DIR> for multiple inputs".into());
    }

    // --model/--mmproj select a local GGUF to spawn llama-server with; remote mode
    // has no local model to load. Reject before the remote return rather than
    // silently ignoring them. Checked here so both the local and remote paths share it.
    if args.endpoint.is_some() && (args.model.is_some() || args.mmproj.is_some()) {
        return Err("--model/--mmproj are local-only; remove them when using --endpoint".into());
    }

    // Remote endpoint mode: rasterize locally, OCR against a remote
    // OpenAI-compatible server. No local llama-server spawn, no model download.
    if let Some(base_url) = args.endpoint.clone() {
        return run_remote(base_url, &inputs, &args);
    }

    // --mmproj alone is meaningless: it overrides the projector for a custom model,
    // but without --model the stock model + stock projector are the matched pair.
    // Checked before preflight so it fails fast without needing llama-server present.
    if args.mmproj.is_some() && args.model.is_none() {
        return Err("--mmproj requires --model".into());
    }

    // 1. Preflight: locate external binaries and validate the llama.cpp build.
    let tools = preflight::check(args.llama_bin.as_deref())?;

    // 2. Ensure model + projector are present (download from HF if missing).
    // Explicit --quant wins; otherwise --quality maps to a quant.
    let quant = args
        .quant
        .clone()
        .unwrap_or_else(|| args.quality.quant().to_string());
    let cache = model::cache_dir(args.model_dir.clone())?;
    // Custom-GGUF mode: route through ensure_with_overrides so override paths are
    // used verbatim (existence-checked in model.rs). The custom model is never
    // downloaded; at most the stock mmproj is fetched here. ensure_with_overrides
    // emits Progress::Download with a concrete pct, so print percent ticks (no
    // separate "downloading <name> ..." header line, the one cosmetic difference
    // from model::ensure's CLI output).
    let files = if args.model.is_some() {
        let mut on_progress = |p: unlocr::Progress| {
            if let unlocr::Progress::Download {
                pct, done, total, ..
            } = p
            {
                use std::io::Write;
                print!("\r  {pct:>3}%  ({} / {} MiB)", done >> 20, total >> 20);
                let _ = std::io::stdout().flush();
            }
        };
        model::ensure_with_overrides(
            &cache,
            &quant,
            args.model.as_deref(),
            args.mmproj.as_deref(),
            &mut on_progress,
        )?
    } else {
        model::ensure(&cache, &quant)?
    };

    std::fs::create_dir_all(&args.out)?;

    // 3. Start llama-server once; it stays up for every page of every PDF.
    // Pass the raw port (0 = auto) so Server::start owns free-port resolution and
    // the bind-race retry; read the actual bound port back from srv.port.
    let srv = server::Server::start(
        &tools.llama_server,
        &files.model,
        &files.mmproj,
        args.port,
        args.image_max_tokens,
        args.chat_template.as_deref(),
    )?;
    let port = srv.port;
    println!("llama-server ready on 127.0.0.1:{port}");

    // 4. OCR each PDF.
    let mut failures = 0;
    for input in &inputs {
        if let Err(e) = ocr::run_pdf(&srv, &tools.pdftoppm, input, &args) {
            eprintln!("error: {}: {e}", input.display());
            failures += 1;
        }
    }

    drop(srv); // explicit: kill llama-server before returning
    if failures > 0 {
        return Err(format!("{failures} input(s) failed").into());
    }
    Ok(())
}

/// OCR every input against a remote OpenAI-compatible endpoint. Pages are still
/// rasterized locally (pdftoppm), so this only skips the llama-server spawn and
/// the model download; --quant/--quality/--llama-bin/--port are inert here.
fn run_remote(base_url: String, inputs: &[PathBuf], args: &Args) -> Res<()> {
    // Only the rasterizer is needed locally; no llama-server, no GGUF.
    let pdftoppm = preflight::pdftoppm()?;

    // Key precedence: --endpoint-key, then UNLOCR_API_KEY. Prefer the env var so
    // the secret stays out of the process list / shell history.
    let api_key = args
        .endpoint_key
        .clone()
        .or_else(|| std::env::var("UNLOCR_API_KEY").ok());

    eprintln!(
        "warning: remote endpoint mode. Unlimited-OCR / DeepSeek-OCR is only known to run on \
         llama.cpp (PR #17400), vLLM, and SGLang. Ollama / LM Studio do not support these \
         OCR models; gateways (litellm/vLLM) need --endpoint-model set to the served name."
    );

    let endpoint = server::RemoteEndpoint {
        base_url,
        api_key,
        model: args.endpoint_model.clone(),
    };

    // Soft reachability check: some servers omit /v1/models, so warn but proceed.
    if let Err(e) = endpoint.probe() {
        eprintln!(
            "warning: could not reach {} (/v1/models): {e}. Proceeding anyway.",
            endpoint.base_url
        );
    }

    std::fs::create_dir_all(&args.out)?;
    println!("using remote endpoint {}", endpoint.base_url);

    let mut failures = 0;
    for input in inputs {
        if let Err(e) = ocr::run_pdf(&endpoint, &pdftoppm, input, args) {
            eprintln!("error: {}: {e}", input.display());
            failures += 1;
        }
    }
    if failures > 0 {
        return Err(format!("{failures} input(s) failed").into());
    }
    Ok(())
}

fn is_pdf(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Collect *.pdf under `dir`, one level deep or recursively.
fn collect_pdfs(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) -> Res<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        // file_type() from the dir entry does NOT follow symlinks (unlike
        // Path::is_dir). Skip only symlinked *directories* so a cycle (e.g. a/ -> ..)
        // cannot recurse into a stack overflow, which `panic = "abort"` turns into a
        // hard abort. A symlinked PDF *file* is legitimate and must NOT be dropped, so
        // it falls through to the is_pdf branch. ponytail: skips symlinked dirs
        // entirely; switch to a visited canonical-path set if symlinked dir trees
        // must be followed.
        let ft = entry.file_type()?;
        let path = entry.path();
        // path.is_dir() follows the symlink; combined with is_symlink() it skips
        // only links that point at a directory.
        if ft.is_symlink() && path.is_dir() {
            continue;
        }
        if ft.is_dir() {
            if recursive {
                collect_pdfs(&path, recursive, out)?;
            }
        } else if is_pdf(&path) {
            out.push(path);
        }
    }
    Ok(())
}

/// Expand positional inputs (files, folders, glob patterns) plus an optional
/// --from-list file into a concrete, sorted, deduped list of PDF paths.
fn expand_inputs(raw: &[PathBuf], from_list: Option<&Path>, recursive: bool) -> Res<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = Vec::new();

    if let Some(list) = from_list {
        let text = std::fs::read_to_string(list)
            .map_err(|e| format!("--from-list {}: {e}", list.display()))?;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            out.push(PathBuf::from(line));
        }
    }

    for input in raw {
        if input.is_dir() {
            collect_pdfs(input, recursive, &mut out)?;
        } else if let Some(pat) = input.to_str().filter(|s| s.contains(['*', '?', '['])) {
            // Glob only when the path isn't a literal that exists. The shell
            // usually expands these already; this covers quoted globs and
            // PowerShell, which does not.
            if input.exists() {
                out.push(input.clone());
            } else {
                for m in glob::glob(pat).map_err(|e| format!("bad glob {pat}: {e}"))? {
                    let p = m?;
                    if is_pdf(&p) {
                        out.push(p);
                    }
                }
            }
        } else {
            out.push(input.clone()); // literal; ocr::run_pdf validates existence
        }
    }

    out.sort();
    out.dedup();
    if out.is_empty() {
        return Err("No input PDFs found. Run: unlocr --help for usage.".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn markdown_task_matches_default_prompt() {
        // The "markdown" preset is the no-flags default; the GUI's TASK_PROMPTS
        // map (gui/src/main.js) mirrors these strings. If the default prompt is
        // ever changed, this fails so the preset (and the JS copy) get updated too.
        assert_eq!(
            Task::Markdown.prompt(),
            unlocr::OcrOptions::default().prompt,
            "Task::Markdown must equal the OcrOptions default prompt"
        );
    }

    #[test]
    fn expand_folder_list_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.pdf"), b"").unwrap();
        fs::write(root.join("UPPER.PDF"), b"").unwrap(); // is_pdf must be case-insensitive
        fs::write(root.join("not.txt"), b"").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/b.pdf"), b"").unwrap();

        // non-recursive: top-level pdfs (any case), .txt excluded; sorted
        let flat = expand_inputs(&[root.to_path_buf()], None, false).unwrap();
        assert_eq!(flat, vec![root.join("UPPER.PDF"), root.join("a.pdf")]);

        // recursive: includes nested b.pdf
        let deep = expand_inputs(&[root.to_path_buf()], None, true).unwrap();
        assert_eq!(
            deep,
            vec![
                root.join("UPPER.PDF"),
                root.join("a.pdf"),
                root.join("sub/b.pdf")
            ]
        );

        // dedup: a.pdf via both folder and --from-list appears once
        let list = root.join("list.txt");
        fs::write(
            &list,
            format!("# comment\n\n{}\n", root.join("a.pdf").display()),
        )
        .unwrap();
        let merged = expand_inputs(&[root.to_path_buf()], Some(&list), false).unwrap();
        assert_eq!(merged, vec![root.join("UPPER.PDF"), root.join("a.pdf")]);

        // empty folder errors
        let empty = tempfile::tempdir().unwrap();
        assert!(expand_inputs(&[empty.path().to_path_buf()], None, false).is_err());
    }

    #[test]
    fn expand_glob_and_literal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.pdf"), b"").unwrap();
        fs::write(root.join("note.txt"), b"").unwrap();

        // glob pattern: matches a.pdf, skips note.txt (is_pdf filter)
        let pat = PathBuf::from(root.join("*.pdf").to_str().unwrap());
        assert_eq!(
            expand_inputs(&[pat], None, false).unwrap(),
            vec![root.join("a.pdf")]
        );

        // literal non-existent path passes through verbatim (run_pdf validates later)
        let lit = PathBuf::from("does-not-exist.pdf");
        assert_eq!(
            expand_inputs(std::slice::from_ref(&lit), None, false).unwrap(),
            vec![lit]
        );
    }

    #[test]
    fn quality_quant_mapping() {
        assert_eq!(Quality::Best.quant(), "BF16");
        assert_eq!(Quality::Good.quant(), "Q8_0");
        assert_eq!(Quality::Less.quant(), "Q4_K_M");
    }

    #[test]
    fn task_prompt_mapping() {
        assert_eq!(Task::Free.prompt(), "Free OCR.");
        assert_eq!(Task::Figure.prompt(), "Parse the figure.");
        // CLI parity: the default task's prompt must equal the no-flags default so
        // `unlocr file.pdf` behaves identically before and after presets existed.
        assert_eq!(
            Task::Markdown.prompt(),
            unlocr::OcrOptions::default().prompt
        );
    }

    #[test]
    fn resolved_prompt_prefers_explicit_over_task() {
        let mut args = Args::parse_from(["unlocr", "x.pdf", "--task", "free"]);
        assert_eq!(args.resolved_prompt(), "Free OCR.");
        // Explicit --prompt wins over the task preset.
        args.prompt = Some("custom".to_string());
        assert_eq!(args.resolved_prompt(), "custom");
    }

    #[test]
    fn parse_pages_accepts_single_and_range() {
        assert_eq!(parse_pages("5").unwrap(), (5, Some(5)));
        assert_eq!(parse_pages("5-9").unwrap(), (5, Some(9)));
        assert_eq!(parse_pages(" 5 - 9 ").unwrap(), (5, Some(9))); // whitespace tolerant
        assert_eq!(parse_pages("3-3").unwrap(), (3, Some(3))); // degenerate range
    }

    #[test]
    fn parse_pages_rejects_bad_input() {
        assert!(parse_pages("0").is_err()); // 1-based
        assert!(parse_pages("9-5").is_err()); // reversed
        assert!(parse_pages("1-0").is_err()); // zero bound
        assert!(parse_pages("abc").is_err()); // non-numeric
        assert!(parse_pages("").is_err()); // empty
        assert!(parse_pages("5-").is_err()); // missing bound
    }

    #[test]
    fn gpu_flag_fills_remote_defaults() {
        // --gpu with nothing else points the remote path at a local vLLM serving
        // the full Unlimited-OCR model. Mirrors the normalization in run().
        let mut args = Args::parse_from(["unlocr", "x.pdf", "--gpu"]);
        args.apply_gpu_defaults();
        assert_eq!(args.endpoint.as_deref(), Some("http://localhost:8000"));
        assert_eq!(args.endpoint_model.as_deref(), Some("baidu/Unlimited-OCR"));

        // An explicit --endpoint wins; --gpu only fills the model default.
        let mut args =
            Args::parse_from(["unlocr", "x.pdf", "--gpu", "--endpoint", "http://host:9000"]);
        args.apply_gpu_defaults();
        assert_eq!(args.endpoint.as_deref(), Some("http://host:9000"));
        assert_eq!(args.endpoint_model.as_deref(), Some("baidu/Unlimited-OCR"));

        // No --gpu = no remote defaults injected (stays local GGUF path).
        let mut args = Args::parse_from(["unlocr", "x.pdf"]);
        args.apply_gpu_defaults();
        assert_eq!(args.endpoint, None);
        assert_eq!(args.endpoint_model, None);
    }

    #[test]
    fn resolved_pages_none_when_flag_absent() {
        let args = Args::parse_from(["unlocr", "x.pdf"]);
        assert_eq!(args.resolved_pages().unwrap(), None);
        let args = Args::parse_from(["unlocr", "x.pdf", "--pages", "2-3"]);
        assert_eq!(args.resolved_pages().unwrap(), Some((2, Some(3))));
    }
}
