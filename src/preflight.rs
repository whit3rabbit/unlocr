// Locate llama-server + pdftoppm and validate the llama.cpp build is new
// enough to know the DeepSeek-OCR architecture.

use crate::Res;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

// b8530, released 2026-03-25T18:57:40Z, is the exact merge build of
// llama.cpp PR #17400 "mtmd: Add DeepSeekOCR Support". Builds below this
// cannot load the model. This is a soft warn only; server::start does the
// authoritative check by actually loading the model (catches forks too).
const MIN_BUILD: u64 = 8530;

const HOMEBREW_BINS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

pub struct Tools {
    pub llama_server: PathBuf,
    pub pdftoppm: PathBuf,
}

pub fn run_doctor(llama_override: Option<&Path>, model_dir: Option<PathBuf>, quant: &str) -> Res<()> {
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

    // Check llama-server
    let llama_path = llama_override.map(|p| p.to_path_buf()).or_else(|| locate("llama-server"));
    match llama_path {
        Some(path) => {
            print!("  [OK] llama-server: found at {}", path.display());
            match build_number(&path) {
                Some(b) => {
                    print!(" (build b{b})");
                    if b < MIN_BUILD {
                        println!("\n       [WARN] build b{b} is older than b{MIN_BUILD}. DeepSeek-OCR support may fail.");
                    } else {
                        println!();
                    }
                }
                None => println!("\n       [WARN] could not parse llama-server build version."),
            }
        }
        None => {
            println!("  [FAIL] llama-server: not found on PATH.");
            let hint_str = generate_install_hint();
            for line in hint_str.lines() {
                println!("         {}", line);
            }
            tools_ok = false;
        }
    }

    // 2. Check model files
    println!("\nChecking model cache...");
    // Validate the user-supplied quant before it reaches check_presence: PathBuf
    // ::join does not normalize, so a traversing quant (e.g. "../../etc/passwd")
    // would otherwise turn into an is_file() probe outside the cache dir (a
    // filesystem existence oracle). Same guard the write path already enforces.
    crate::model::validate_quant(quant)?;
    let cache = crate::model::cache_dir(model_dir)?;
    println!("  Cache directory: {}", cache.display());

    let (model_path, model_present, mmproj_path, mmproj_present) = crate::model::check_presence(&cache, quant);
    
    if model_present {
        let size_str = match fs::metadata(&model_path) {
            Ok(meta) => format!("{:.2} GiB", meta.len() as f64 / 1024.0 / 1024.0 / 1024.0),
            Err(_) => "unknown size".to_string(),
        };
        println!("  [OK] Model file: present at {} ({})", model_path.display(), size_str);
    } else {
        println!("  [INFO] Model file: missing at {} (will download on first run)", model_path.display());
    }

    if mmproj_present {
        let size_str = match fs::metadata(&mmproj_path) {
            Ok(meta) => format!("{:.2} MiB", meta.len() as f64 / 1024.0 / 1024.0),
            Err(_) => "unknown size".to_string(),
        };
        println!("  [OK] Projector file: present at {} ({})", mmproj_path.display(), size_str);
    } else {
        println!("  [INFO] Projector file: missing at {} (will download on first run)", mmproj_path.display());
    }

    // 3. Check RAM availability
    println!("\nChecking system memory...");
    match get_total_ram_bytes() {
        Some(total_bytes) => {
            let total_gb = total_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
            print!("  Total physical RAM: {:.2} GB", total_gb);
            if total_gb < 4.0 {
                println!(" - [WARN] Very low memory. OCR will likely crash or run extremely slowly.");
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
                println!(" - [WARN] Low disk space. Downloading the model or rasterizing PDFs may fail.");
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

fn get_total_ram_bytes() -> Option<u64> {
    if cfg!(target_os = "macos") {
        let out = Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        stdout.parse().ok()
    } else if cfg!(target_os = "linux") {
        let meminfo = fs::read_to_string("/proc/meminfo").ok()?;
        for line in meminfo.lines() {
            if line.starts_with("MemTotal:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let kb: u64 = parts[1].parse().ok()?;
                    return Some(kb * 1024);
                }
            }
        }
        None
    } else if cfg!(target_os = "windows") {
        let out = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_PhysicalMemory | Measure-Object Capacity -Sum).Sum",
            ])
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        stdout.parse().ok()
    } else {
        None
    }
}

fn get_free_disk_space_bytes(path: &Path) -> Option<u64> {
    if cfg!(target_os = "windows") {
        // Pass the path through an env var, not string interpolation, so
        // PowerShell never parses it as code. `path` is user-controlled
        // (--model-dir, or the LOCALAPPDATA/XDG_CACHE_HOME/HOME cache-dir env
        // vars); a value containing a single quote would otherwise terminate the
        // -Command string literal and inject arbitrary PowerShell. -LiteralPath
        // also stops wildcard/glob interpretation of the path.
        let out = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(Get-Item -LiteralPath $env:UNLOCR_DISK_PATH).Volume.Free",
            ])
            .env("UNLOCR_DISK_PATH", path)
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        stdout.parse().ok()
    } else {
        // macOS or Linux (Unix)
        let out = Command::new("df")
            .arg("-k")
            .arg(path)
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        if lines.len() < 2 {
            return None;
        }
        let headers: Vec<&str> = lines[0].split_whitespace().collect();
        let data_joined = lines[1..].join(" ");
        let data: Vec<&str> = data_joined.split_whitespace().collect();
        
        let avail_idx = headers.iter().position(|&h| h.contains("Avail") || h.contains("Free") || h.contains("avail"));
        if let Some(idx) = avail_idx {
            if idx < data.len() {
                let kb: u64 = data[idx].parse().ok()?;
                return Some(kb * 1024);
            }
        }
        
        if data.len() >= 4 {
            let kb: u64 = data[3].parse().ok()?;
            return Some(kb * 1024);
        }
        None
    }
}

pub fn check(llama_override: Option<&Path>) -> Res<Tools> {
    let llama_server = match llama_override {
        Some(p) => p.to_path_buf(),
        None => locate("llama-server").ok_or_else(|| {
            let hint_str = generate_install_hint();
            Box::<dyn std::error::Error>::from(hint_str)
        })?,
    };
    let pdftoppm =
        locate("pdftoppm").ok_or_else(|| hint("pdftoppm", "brew install poppler"))?;

    // Soft version gate. The hard gate is the real model load in server.rs.
    match build_number(&llama_server) {
        Some(b) if b < MIN_BUILD => eprintln!(
            "warning: llama-server build b{b} is older than b{MIN_BUILD} (DeepSeek-OCR support, PR #17400). \
             Run `brew upgrade llama.cpp` if model loading fails."
        ),
        Some(_) => {}
        None => eprintln!(
            "warning: could not parse llama-server version; need build >= b{MIN_BUILD}."
        ),
    }

    Ok(Tools { llama_server, pdftoppm })
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

/// Detect what package managers are available on the user's PATH.
fn detect_package_managers() -> Vec<&'static str> {
    let mut managers = Vec::new();
    if locate("brew").is_some() {
        managers.push("brew");
    }
    if locate("port").is_some() {
        managers.push("port");
    }
    if locate("conda").is_some() || locate("mamba").is_some() || locate("pixi").is_some() {
        managers.push("conda");
    }
    if locate("nix").is_some() {
        managers.push("nix");
    }
    if locate("winget").is_some() {
        managers.push("winget");
    }
    if locate("scoop").is_some() {
        managers.push("scoop");
    }
    managers
}

/// Generate rich, tailored install instructions for llama-server.
pub fn generate_install_hint() -> String {
    let managers = detect_package_managers();
    let mut methods = Vec::new();

    if cfg!(target_os = "macos") {
        if managers.contains(&"brew") {
            methods.push("  - Homebrew: brew install llama.cpp");
        }
        if managers.contains(&"port") {
            methods.push("  - MacPorts: sudo port install llama.cpp");
        }
        if managers.contains(&"conda") {
            methods.push("  - Conda-forge: conda install -c conda-forge llama-cpp");
        }
        if managers.contains(&"nix") {
            methods.push("  - Nix: nix profile install nixpkgs#llama-cpp");
        }
        if methods.is_empty() {
            methods.push("  - Homebrew (Recommended): brew install llama.cpp");
            methods.push("  - Conda-forge: conda install -c conda-forge llama-cpp");
            methods.push("  - MacPorts: sudo port install llama.cpp");
            methods.push("  - Nix: nix profile install nixpkgs#llama-cpp");
        }
    } else if cfg!(target_os = "windows") {
        if managers.contains(&"winget") {
            methods.push("  - Winget: winget install llama.cpp");
        }
        if managers.contains(&"scoop") {
            methods.push("  - Scoop: scoop install llama-cpp");
        }
        if managers.contains(&"conda") {
            methods.push("  - Conda-forge: conda install -c conda-forge llama-cpp");
        }
        if methods.is_empty() {
            methods.push("  - Winget (Recommended): winget install llama.cpp");
            methods.push("  - Scoop: scoop install llama-cpp");
            methods.push("  - Conda-forge: conda install -c conda-forge llama-cpp");
        }
    } else {
        // Linux / other Unix
        if managers.contains(&"brew") {
            methods.push("  - Homebrew: brew install llama.cpp");
        }
        if managers.contains(&"conda") {
            methods.push("  - Conda-forge: conda install -c conda-forge llama-cpp");
        }
        if managers.contains(&"nix") {
            methods.push("  - Nix: nix profile install nixpkgs#llama-cpp");
        }
        if methods.is_empty() {
            methods.push("  - Homebrew: brew install llama.cpp");
            methods.push("  - Conda-forge: conda install -c conda-forge llama-cpp");
            methods.push("  - Nix: nix profile install nixpkgs#llama-cpp");
            methods.push("  - Build from source: see https://github.com/ggml-org/llama.cpp/blob/master/docs/install.md");
        }
    }

    let mut hint = "llama-server (from llama.cpp >= b8530) is required.\nInstall it using one of these options:\n".to_string();
    for m in &methods {
        hint.push_str(m);
        hint.push('\n');
    }
    hint.push_str("Or pass --llama-bin with the path to the executable.");
    hint
}

/// Look up a binary on PATH, then in the known Homebrew prefixes.
fn locate(bin: &str) -> Option<PathBuf> {
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
    None
}

/// Parse the build number out of llama-server's `--version` output. Returns None
/// when llama-server is missing, unreadable, or its version line cannot be parsed
/// (commit hashes are skipped). Pub so the Tauri host can surface the build number
/// in its preflight report without re-implementing the parse. Additive: existing
/// callers (`check`, `run_doctor`) are unchanged.
pub fn build_number(llama_server: &Path) -> Option<u64> {
    let out = Command::new(llama_server).arg("--version").output().ok()?;
    // llama.cpp prints the version line to stderr.
    let text = String::from_utf8_lossy(&out.stderr);
    let text = if text.trim().is_empty() {
        String::from_utf8_lossy(&out.stdout).into_owned()
    } else {
        text.into_owned()
    };
    parse_build(&text)
}

/// Extract the build number from llama.cpp's version output.
/// Accepts `version: 9770 (75ad0b23e)`, bare `4229`, or tag `b8530`.
fn parse_build(text: &str) -> Option<u64> {
    for raw in text.split_whitespace() {
        let tok = raw.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        // `b<digits>` (release tag form)
        if let Some(rest) = tok.strip_prefix('b') {
            if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
                return rest.parse().ok();
            }
        }
        // bare integer (commit hashes have letters, so they're skipped)
        if !tok.is_empty() && tok.bytes().all(|b| b.is_ascii_digit()) {
            return tok.parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse_build;

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
        assert!(hint.contains("llama-server (from llama.cpp >= b8530) is required."));
        assert!(hint.contains("llama-cpp") || hint.contains("llama.cpp"));
    }
}
