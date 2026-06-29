use super::locate;
use std::path::Path;
use std::process::Command;

/// Detect what package managers are available on the user's PATH.
pub fn detect_package_managers() -> Vec<&'static str> {
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
pub fn parse_build(text: &str) -> Option<u64> {
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
