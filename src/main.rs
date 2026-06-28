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

#[derive(Parser, Debug)]
#[command(name = "unlocr", version, about = "OCR PDFs to markdown via Unlimited-OCR + llama.cpp")]
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

    if let Some(cmd) = args.command {
        match cmd {
            Commands::Doctor { llama_bin, model_dir, quant } | Commands::Preflight { llama_bin, model_dir, quant } => {
                preflight::run_doctor(llama_bin.as_deref(), model_dir, &quant)?;
            }
        }
        return Ok(());
    }

    // Expand folders, globs, and --from-list into a concrete, deduped PDF list.
    let inputs = expand_inputs(&args.inputs, args.from_list.as_deref(), args.recursive)?;

    // 1. Preflight: locate external binaries and validate the llama.cpp build.
    let tools = preflight::check(args.llama_bin.as_deref())?;

    // 2. Ensure model + projector are present (download from HF if missing).
    // Explicit --quant wins; otherwise --quality maps to a quant.
    let quant = args.quant.clone().unwrap_or_else(|| args.quality.quant().to_string());
    let cache = model::cache_dir(args.model_dir.clone())?;
    let files = model::ensure(&cache, &quant)?;

    std::fs::create_dir_all(&args.out)?;

    // 3. Start llama-server once; it stays up for every page of every PDF.
    // Pass the raw port (0 = auto) so Server::start owns free-port resolution and
    // the bind-race retry; read the actual bound port back from srv.port.
    let srv = server::Server::start(&tools.llama_server, &files.model, &files.mmproj, args.port)?;
    let port = srv.port;
    println!("llama-server ready on 127.0.0.1:{port}");

    // 4. OCR each PDF.
    let mut failures = 0;
    for input in &inputs {
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
        // Path::is_dir). Skip symlinked entries so a cycle (e.g. a/ -> ..) cannot
        // recurse into a stack overflow, which `panic = "abort"` turns into a hard
        // abort. ponytail: skips symlinked dirs entirely; switch to a visited
        // canonical-path set if real symlinked trees must be followed.
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
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
        assert_eq!(deep, vec![root.join("UPPER.PDF"), root.join("a.pdf"), root.join("sub/b.pdf")]);

        // dedup: a.pdf via both folder and --from-list appears once
        let list = root.join("list.txt");
        fs::write(&list, format!("# comment\n\n{}\n", root.join("a.pdf").display())).unwrap();
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
        assert_eq!(expand_inputs(&[pat], None, false).unwrap(), vec![root.join("a.pdf")]);

        // literal non-existent path passes through verbatim (run_pdf validates later)
        let lit = PathBuf::from("does-not-exist.pdf");
        assert_eq!(expand_inputs(&[lit.clone()], None, false).unwrap(), vec![lit]);
    }

    #[test]
    fn quality_quant_mapping() {
        assert_eq!(Quality::Best.quant(), "BF16");
        assert_eq!(Quality::Good.quant(), "Q8_0");
        assert_eq!(Quality::Less.quant(), "Q4_K_M");
    }
}
