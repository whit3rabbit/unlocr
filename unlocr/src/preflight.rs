// Locate llama-server + pdftoppm and validate the llama.cpp build is new
// enough to know the DeepSeek-OCR architecture.

use crate::Res;
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

pub fn check(llama_override: Option<&Path>) -> Res<Tools> {
    let llama_server = match llama_override {
        Some(p) => p.to_path_buf(),
        None => locate("llama-server").ok_or_else(|| {
            hint("llama-server", "brew install llama.cpp")
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

fn hint(bin: &str, install: &str) -> Box<dyn std::error::Error> {
    format!("{bin} not found on PATH. Install it: `{install}` (macOS), or pass --llama-bin.").into()
}

/// Look up a binary on PATH, then in the known Homebrew prefixes.
fn locate(bin: &str) -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let cand = dir.join(bin);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    for dir in HOMEBREW_BINS {
        let cand = Path::new(dir).join(bin);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

fn build_number(llama_server: &Path) -> Option<u64> {
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
}
