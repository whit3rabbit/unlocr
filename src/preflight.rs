// Locate llama-server + pdftoppm and validate the llama.cpp build is new
// enough to know the DeepSeek-OCR architecture.

use crate::Res;
use std::fs;
use std::path::{Path, PathBuf};

/// Installation hints and instructions.
pub mod hints;
/// System requirements checking and rating.
pub mod sysreq;
/// System information and check utilities.
pub mod system;

pub use hints::*;
pub use sysreq::*;
pub use system::*;

// b8530, released 2026-03-25T18:57:40Z, is the exact merge build of
// llama.cpp PR #17400 "mtmd: Add DeepSeekOCR Support". Builds below this
// cannot load the model. This is a soft warn only; server::start does the
// authoritative check by actually loading the model (catches forks too).
const MIN_BUILD: u64 = 8530;

const HOMEBREW_BINS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// Resolved paths to system dependencies (llama-server and pdftoppm).
pub struct Tools {
    /// Resolved path to the llama-server binary.
    pub llama_server: PathBuf,
    /// Resolved path to the pdftoppm binary.
    pub pdftoppm: PathBuf,
}

/// Where a resolved llama-server came from.
pub enum Provenance {
    /// unlocr's own R-SWA build under `<cache>/tools/llama-server/` (trusted:
    /// we built it from PR #24975, so it has the Unlimited-OCR patch).
    Managed,
    /// A `--llama-bin` override, or a PATH/Homebrew binary. Cannot be verified for
    /// the Unlimited-OCR R-SWA patch (PR #24975), which is unmerged upstream.
    External,
}

/// True when the user opts into an unverified external llama-server via
/// `UNLOCR_ALLOW_EXTERNAL_LLAMA` (any non-empty value).
fn allow_external_llama() -> bool {
    std::env::var_os("UNLOCR_ALLOW_EXTERNAL_LLAMA")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

fn llama_exe() -> &'static str {
    if cfg!(target_os = "windows") {
        "llama-server.exe"
    } else {
        "llama-server"
    }
}

/// Non-downloading resolution for diagnostics (doctor, the GUI status panel):
/// override -> managed cached build -> PATH/Homebrew. NEVER triggers a download, so
/// a passive status check does not pull a multi-hundred-MB binary. Returns None when
/// nothing is found.
pub fn find_llama_server(override_: Option<&Path>) -> Option<(PathBuf, Provenance)> {
    if let Some(p) = override_ {
        return Some((p.to_path_buf(), Provenance::External));
    }
    if let Ok(cache) = crate::model::cache_dir(None) {
        let managed = crate::tools::tools_dir(&cache).join("llama-server");
        if let Some(p) = crate::tools::find_exe(&managed, llama_exe()) {
            return Some((p, Provenance::Managed));
        }
    }
    locate("llama-server").map(|p| (p, Provenance::External))
}

/// Resolve llama-server for a RUN, preferring unlocr's managed R-SWA build and
/// auto-downloading it when absent (like the GGUF model). Order: `--llama-bin`
/// override -> managed cached build -> auto-download the managed build (where a pin
/// exists for this OS+arch) -> external PATH/Homebrew. Only unlocr's own download is
/// `Managed`; an override or PATH/brew binary is `External` (unverifiable for the
/// unmerged Unlimited-OCR R-SWA patch, PR #24975). A download failure (e.g. offline)
/// falls through to PATH so a user who already has a compatible build still runs.
pub fn resolve_llama_server(
    override_: Option<&Path>,
    on_progress: &mut dyn FnMut(crate::Progress),
) -> Res<(PathBuf, Provenance)> {
    if let Some(p) = override_ {
        return Ok((p.to_path_buf(), Provenance::External));
    }
    if let Ok(cache) = crate::model::cache_dir(None) {
        let managed = crate::tools::tools_dir(&cache).join("llama-server");
        if let Some(p) = crate::tools::find_exe(&managed, llama_exe()) {
            return Ok((p, Provenance::Managed));
        }
        if crate::tools::downloadable().contains(&"llama-server") {
            match crate::tools::ensure_tool(&cache, "llama-server", on_progress) {
                Ok(p) => return Ok((p, Provenance::Managed)),
                Err(e) => eprintln!(
                    "warning: could not download unlocr's bundled llama-server ({e}); \
                     falling back to any llama-server on PATH."
                ),
            }
        }
    }
    match locate("llama-server") {
        Some(p) => Ok((p, Provenance::External)),
        None => Err(generate_install_hint().into()),
    }
}

/// Runs diagnostic checks on dependencies, model files, memory, and disk space.
pub fn run_doctor(
    llama_override: Option<&Path>,
    model_dir: Option<PathBuf>,
    quant: &str,
) -> Res<()> {
    println!("=== unlocr doctor ===");

    // 1. Check tools
    println!("\nChecking external dependencies...");
    let mut tools_ok = true;

    // Check pdftoppm
    match locate("pdftoppm") {
        Some(path) => println!("  [OK] pdftoppm: found at {}", path.display()),
        None => {
            println!("  [FAIL] pdftoppm: not found on PATH.");
            println!("         Hint: install poppler (e.g. `brew install poppler` on macOS, `apt install poppler-utils` on Debian/Ubuntu)");
            tools_ok = false;
        }
    }

    // Check llama-server (non-downloading: report the managed R-SWA build if cached,
    // else whatever is on PATH -- unverified for the Unlimited-OCR patch, PR #24975).
    match find_llama_server(llama_override) {
        Some((path, Provenance::Managed)) => {
            println!(
                "  [OK] llama-server: found at {} (unlocr managed R-SWA build)",
                path.display()
            );
        }
        Some((path, Provenance::External)) => {
            print!("  [OK] llama-server: found at {}", path.display());
            match build_number(&path) {
                Some(b) => println!(
                    " (build b{b}; external -- unverified for the Unlimited-OCR R-SWA patch, PR #24975)"
                ),
                None => println!(
                    "\n       [WARN] external llama-server: could not parse the build version, \
                     and cannot verify the Unlimited-OCR R-SWA patch (PR #24975)."
                ),
            }
        }
        None => {
            println!(
                "  [INFO] llama-server: no managed R-SWA build cached (auto-downloads on first run)."
            );
            let hint_str = generate_install_hint();
            for line in hint_str.lines() {
                println!("         {}", line);
            }
        }
    }

    // 2. Check model files
    println!("\nChecking model cache...");
    let cache = crate::model::cache_dir(model_dir)?;
    println!("  Cache directory: {}", cache.display());

    // check_presence now validates the quant internally (defense in depth), so
    // the explicit validate_quant call above is no longer needed here. The `?`
    // propagates an invalid-quant error with the same message.
    let (model_path, model_present, mmproj_path, mmproj_present) =
        crate::model::check_presence(&cache, quant)?;

    if model_present {
        let size_str = match fs::metadata(&model_path) {
            Ok(meta) => format!("{:.2} GiB", meta.len() as f64 / 1024.0 / 1024.0 / 1024.0),
            Err(_) => "unknown size".to_string(),
        };
        println!(
            "  [OK] Model file: present at {} ({})",
            model_path.display(),
            size_str
        );
    } else {
        println!(
            "  [INFO] Model file: missing at {} (will download on first run)",
            model_path.display()
        );
    }

    if mmproj_present {
        let size_str = match fs::metadata(&mmproj_path) {
            Ok(meta) => format!("{:.2} MiB", meta.len() as f64 / 1024.0 / 1024.0),
            Err(_) => "unknown size".to_string(),
        };
        println!(
            "  [OK] Projector file: present at {} ({})",
            mmproj_path.display(),
            size_str
        );
    } else {
        println!(
            "  [INFO] Projector file: missing at {} (will download on first run)",
            mmproj_path.display()
        );
    }

    // 3. Check RAM availability
    println!("\nChecking system memory...");
    match get_total_ram_bytes() {
        Some(total_bytes) => {
            let total_gb = total_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
            print!("  Total physical RAM: {:.2} GB", total_gb);
            if total_gb < 4.0 {
                println!(
                    " - [WARN] Very low memory. OCR will likely crash or run extremely slowly."
                );
            } else if total_gb < 8.0 {
                println!(" - [WARN] Low memory. Good/Best models may exceed available RAM.");
            } else {
                println!(" - [OK]");
            }
        }
        None => {
            println!("  [WARN] Could not retrieve system memory information.");
        }
    }

    // 4. Check Disk Space availability
    println!("\nChecking disk space...");
    match get_free_disk_space_bytes(&cache) {
        Some(free_bytes) => {
            let free_gb = free_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
            print!("  Free space on cache partition: {:.2} GB", free_gb);
            if free_gb < 5.0 {
                println!(
                    " - [WARN] Low disk space. Downloading the model or rasterizing PDFs may fail."
                );
            } else {
                println!(" - [OK]");
            }
        }
        None => {
            println!("  [WARN] Could not retrieve disk space information.");
        }
    }

    println!("\nDiagnostics complete.");
    if tools_ok {
        println!("System is ready to run OCR.");
    } else {
        println!("Warning: some issues were found. Please resolve the [FAIL] items above.");
    }

    Ok(())
}

/// Validates that both llama-server and pdftoppm are installed and reachable,
/// resolving (and auto-downloading, via `resolve_llama_server`) unlocr's managed
/// R-SWA llama-server. `on_progress` receives `Progress::Download` ticks for that
/// download; pass a no-op sink if you don't surface it.
pub fn check(
    llama_override: Option<&Path>,
    on_progress: &mut dyn FnMut(crate::Progress),
) -> Res<Tools> {
    let (llama_server, provenance) = resolve_llama_server(llama_override, on_progress)?;
    let pdftoppm = locate("pdftoppm").ok_or_else(|| hint("pdftoppm", "brew install poppler"))?;

    // Compatibility warning. The Unlimited-OCR R-SWA patch (PR #24975) is unmerged
    // upstream, so no stock build has it and the build number cannot prove support.
    // An external binary is therefore unverifiable; warn unless the user opts out.
    // The managed build is our own R-SWA build, trusted silently. The hard gate is
    // the real model load in server::start (catches an incompatible binary anyway).
    if matches!(provenance, Provenance::External) && !allow_external_llama() {
        eprintln!(
            "warning: using an external llama-server ({}); unlocr cannot verify it includes \
             the Unlimited-OCR Reference Sliding Window Attention patch (PR #24975). unlocr \
             ships a patched build -- drop --llama-bin to use it, or set \
             UNLOCR_ALLOW_EXTERNAL_LLAMA=1 to silence this.",
            llama_server.display()
        );
        // Secondary: the base DeepSeek-OCR arch also needs >= b8530.
        if let Some(b) = build_number(&llama_server) {
            if b < MIN_BUILD {
                eprintln!(
                    "warning: llama-server build b{b} is also older than b{MIN_BUILD} \
                     (DeepSeek-OCR base support, PR #17400)."
                );
            }
        }
    }

    Ok(Tools {
        llama_server,
        pdftoppm,
    })
}

/// Resolve `mlxcel-server` for `--mlx` mode, auto-downloading it into the managed
/// cache if absent (like the GGUF llama-server path, minus the provenance/patch
/// concerns: mlxcel ships an official signed release, not an unmerged patch we
/// maintain ourselves). Apple Silicon only; errors immediately on any other
/// target rather than pretending a download might work.
pub fn resolve_mlxcel_server(
    override_: Option<&Path>,
    model_dir: Option<PathBuf>,
    on_progress: &mut dyn FnMut(crate::Progress),
) -> Res<PathBuf> {
    if !crate::server::mlx_platform_supported() {
        return Err(
            "--mlx requires macOS on Apple Silicon (mlxcel-server has no build for this \
             platform); use the default llama.cpp/GGUF path instead"
                .into(),
        );
    }
    if let Some(p) = override_ {
        return Ok(p.to_path_buf());
    }
    let cache = crate::model::cache_dir(model_dir)?;
    let managed = crate::tools::tools_dir(&cache).join("mlxcel-server");
    if let Some(p) = crate::tools::find_exe(&managed, "mlxcel-server") {
        return Ok(p);
    }
    crate::tools::ensure_tool(&cache, "mlxcel-server", on_progress)
}

/// Resolve pdftoppm alone. Remote inference still rasterizes locally, so the GUI's
/// remote mode needs poppler but NOT llama-server; `check` requires both, which
/// would wrongly block remote on a machine without llama.cpp. Same lookup +
/// install hint as `check`'s pdftoppm step.
pub fn pdftoppm() -> Res<PathBuf> {
    locate("pdftoppm").ok_or_else(|| hint("pdftoppm", "brew install poppler"))
}

fn hint(bin: &str, install: &str) -> Box<dyn std::error::Error> {
    format!("{bin} not found on PATH. Install it: `{install}` (macOS), or pass --llama-bin.").into()
}

/// Look up a binary on PATH, then in the known Homebrew prefixes. Public so the GUI
/// can resolve optional tools (e.g. pandoc for the review-pane export) using the same
/// PATH + Homebrew-prefix search the CLI uses, instead of duplicating the logic.
pub fn locate(bin: &str) -> Option<PathBuf> {
    let mut names = vec![bin.to_string()];
    if cfg!(target_os = "windows") && !bin.ends_with(".exe") {
        names.insert(0, format!("{bin}.exe"));
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in &names {
                let cand = dir.join(name);
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }
    for dir in HOMEBREW_BINS {
        for name in &names {
            let cand = Path::new(dir).join(name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    // Tools fetched by the on-demand downloader live under <cache>/tools/<tool>/...
    // (Windows). Skip the walk unless that dir actually exists (it only ever exists
    // after a Windows download), so non-Windows hosts pay one stat here instead of
    // six failed `read_dir` calls per `locate()`. PATH/Homebrew still wins above.
    if let Ok(cache) = crate::model::cache_dir(None) {
        let tools = crate::tools::tools_dir(&cache);
        if tools.exists() {
            for name in &names {
                if let Some(p) = crate::tools::find_exe(&tools, name) {
                    return Some(p);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::hints::parse_build;

    #[test]
    fn parses_build_numbers() {
        assert_eq!(parse_build("b4488"), Some(4488));
        assert_eq!(parse_build("4229"), Some(4229));
        assert_eq!(parse_build("version: 5123 (abc)"), Some(5123));
        // real-world line; commit hash must not be mistaken for the build
        assert_eq!(parse_build("version: 9770 (75ad0b23e)"), Some(9770));
        assert_eq!(parse_build("hello world"), None);
        assert_eq!(parse_build(""), None);
    }

    #[test]
    fn test_generate_install_hint() {
        let hint = super::generate_install_hint();
        assert!(hint.contains("llama-server"));
        assert!(hint.contains("PR #24975"));
        assert!(hint.contains("llama-cpp") || hint.contains("llama.cpp"));
    }
}
