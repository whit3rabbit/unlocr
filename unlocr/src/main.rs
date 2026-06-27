// unlocr: thin CLI wrapping llama.cpp's llama-server to OCR PDFs with the
// Unlimited-OCR (DeepSeek-OCR) model. PDF -> PNG (pdftoppm) -> per-page chat
// completion -> page-delimited markdown.

mod model;
mod ocr;
mod pdf;
mod preflight;
mod server;

use clap::{Parser, ValueEnum};
use std::path::PathBuf;
use std::process::ExitCode;

pub type Res<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Parser, Debug)]
#[command(name = "unlocr", version, about = "OCR PDFs to markdown via Unlimited-OCR + llama.cpp")]
struct Args {
    /// Input PDF file(s)
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Output directory for the .md files (default: current dir)
    #[arg(long, default_value = ".")]
    out: PathBuf,

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

    /// OCR prompt sent with every page
    #[arg(long, default_value = "<|grounding|>Convert the document to markdown.")]
    prompt: String,

    /// Rasterization DPI passed to pdftoppm
    #[arg(long, default_value_t = 144)]
    dpi: u32,

    /// Path to llama-server (default: PATH / Homebrew)
    #[arg(long)]
    llama_bin: Option<PathBuf>,

    /// Model cache directory (default: per-OS cache dir)
    #[arg(long)]
    model_dir: Option<PathBuf>,

    /// Port for llama-server (0 = auto-pick a free port)
    #[arg(long, default_value_t = 0)]
    port: u16,

    /// Keep the intermediate page PNGs instead of deleting them
    #[arg(long)]
    keep_images: bool,
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
    let args = Args::parse();

    // 1. Preflight: locate external binaries and validate the llama.cpp build.
    let tools = preflight::check(args.llama_bin.as_deref())?;

    // 2. Ensure model + projector are present (download from HF if missing).
    // Explicit --quant wins; otherwise --quality maps to a quant.
    let quant = args.quant.clone().unwrap_or_else(|| args.quality.quant().to_string());
    let cache = model::cache_dir(args.model_dir.clone())?;
    let files = model::ensure(&cache, &quant)?;

    std::fs::create_dir_all(&args.out)?;

    // 3. Start llama-server once; it stays up for every page of every PDF.
    let port = if args.port == 0 { server::free_port()? } else { args.port };
    let srv = server::Server::start(&tools.llama_server, &files.model, &files.mmproj, port)?;
    println!("llama-server ready on 127.0.0.1:{port}");

    // 4. OCR each PDF.
    let mut failures = 0;
    for input in &args.inputs {
        if let Err(e) = ocr::run_pdf(&srv, &tools.pdftoppm, input, &args, port) {
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
