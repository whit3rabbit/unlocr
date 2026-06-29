use crate::Res;
use std::fs;
use std::path::{Path, PathBuf};

mod download;
#[cfg(test)]
mod tests;

pub use download::{ensure, ensure_with_overrides, ensure_with_progress};

// Local llama.cpp path: the quantized GGUF build of Unlimited-OCR.
pub(crate) const REPO: &str = "sahilchachra/Unlimited-OCR-GGUF";
pub(crate) const MMPROJ: &str = "mmproj-Unlimited-OCR-F16.gguf";

/// Pinned model revision (a commit sha, NOT the mutable `main` ref).
pub(crate) const REV: &str = "028d04678db356095d0015b70f0803f2179180f4";

/// sha256 of each shipped GGUF at REV.
pub(crate) const DIGESTS: &[(&str, &str)] = &[
    (
        "Unlimited-OCR-BF16.gguf",
        "731b7d1f56c94198607e08cec6f11ed62e6493b8539f9f4ed337ddd1ab3a1896",
    ),
    (
        "Unlimited-OCR-Q8_0.gguf",
        "234c36f679a3768f5564e9e02c2c1deacbd5677b9c8558a57133f1813f6dd3b8",
    ),
    (
        "Unlimited-OCR-Q4_K_M.gguf",
        "c8461bded976eac709a33f6b26e1414efcd2124a203f2ee93ee984a4c9e9265b",
    ),
    (
        "mmproj-Unlimited-OCR-F16.gguf",
        "4f28c295e1fcf67a97488e356f2b4372da4702b77fdfad0fa138b5821325743c",
    ),
];

/// Paths to the resolved model and multimodal projector files.
#[derive(Debug)]
pub struct ModelFiles {
    /// Resolved path to the GGUF model file.
    pub model: PathBuf,
    /// Resolved path to the GGUF multimodal projector file.
    pub mmproj: PathBuf,
}

/// Outcome of comparing a downloaded file's sha256 to the pinned DIGESTS table.
#[derive(Debug)]
pub(crate) enum DigestCheck {
    /// Hash matches the pinned digest for this filename.
    Match,
    /// Hash does NOT match the pinned digest; reject the download.
    Mismatch { expected: String },
    /// No digest pinned for this filename (a custom quant); caller warns and proceeds.
    Unpinned,
}

/// Pure comparison of an `actual` sha256 hex against the pinned digest for `name`.
pub(crate) fn check_digest(name: &str, actual_hex: &str) -> DigestCheck {
    match DIGESTS.iter().find(|(n, _)| *n == name) {
        Some((_, expected)) if expected.eq_ignore_ascii_case(actual_hex) => DigestCheck::Match,
        Some((_, expected)) => DigestCheck::Mismatch {
            expected: (*expected).to_string(),
        },
        None => DigestCheck::Unpinned,
    }
}

/// sha256 of a byte slice as lowercase hex. Shared with the Windows tools
/// downloader (`tools.rs`) so there is one integrity-hash implementation.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_from_digest(hasher.finalize())
}

/// Lowercase-hex encode a sha256 digest (the shared tail of `file_sha256` and
/// `sha256_hex`).
pub(crate) fn hex_from_digest(digest: impl AsRef<[u8]>) -> String {
    use std::fmt::Write;
    let mut hex = String::with_capacity(64);
    for b in digest.as_ref() {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// sha256 of a file as lowercase hex, streamed so a multi-GB GGUF never loads into
/// memory. Hashing the finished `.part` (rather than incrementally during the
/// stream) keeps the resume path correct: the bytes already on disk are included.
pub(crate) fn file_sha256(path: &Path) -> Res<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20]; // 1 MiB
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_from_digest(hasher.finalize()))
}

/// `--model-dir` override, else the per-OS cache dir + `/unlocr`.
pub fn cache_dir(override_dir: Option<PathBuf>) -> Res<PathBuf> {
    let dir = match override_dir {
        Some(d) => d,
        None => base_cache_dir()?.join("unlocr"),
    };
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn base_cache_dir() -> Res<PathBuf> {
    if let Ok(x) = std::env::var("XDG_CACHE_HOME") {
        if !x.is_empty() {
            return Ok(PathBuf::from(x));
        }
    }
    if cfg!(target_os = "macos") {
        let home = std::env::var("HOME")?;
        Ok(PathBuf::from(home).join("Library/Caches"))
    } else if cfg!(target_os = "windows") {
        let local = std::env::var("LOCALAPPDATA")?;
        Ok(PathBuf::from(local))
    } else {
        let home = std::env::var("HOME")?;
        Ok(PathBuf::from(home).join(".cache"))
    }
}

/// Per-OS app-DATA base dir (user data, not regenerable cache): mirrors
/// `base_cache_dir`'s XDG-first + OS switch but for data locations. Public so the
/// GUI's SQLite store (`db.rs`) resolves its data dir with the same logic instead
/// of re-deriving the OS ladder in a second crate.
pub fn base_data_dir() -> Res<PathBuf> {
    if let Ok(x) = std::env::var("XDG_DATA_HOME") {
        if !x.is_empty() {
            return Ok(PathBuf::from(x));
        }
    }
    if cfg!(target_os = "macos") {
        let home = std::env::var("HOME")?;
        Ok(PathBuf::from(home).join("Library/Application Support"))
    } else if cfg!(target_os = "windows") {
        let appdata = std::env::var("APPDATA")?;
        Ok(PathBuf::from(appdata))
    } else {
        let home = std::env::var("HOME")?;
        Ok(PathBuf::from(home).join(".local/share"))
    }
}

/// GGUF filename for a quant tag, e.g. "Q8_0" -> "Unlimited-OCR-Q8_0.gguf".
pub(crate) fn model_filename(quant: &str) -> String {
    format!("Unlimited-OCR-{quant}.gguf")
}

/// Reject quant tags that could escape the cache dir or manipulate the download
/// URL.
pub fn validate_quant(quant: &str) -> Res<()> {
    let charset_ok = !quant.is_empty()
        && quant
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
    if !charset_ok || quant.contains("..") {
        return Err(
            format!("invalid quant {quant:?}: allowed characters are [A-Za-z0-9_.-]").into(),
        );
    }
    Ok(())
}

/// Check whether the model and projector GGUFs for `quant` exist in `cache`.
pub fn check_presence(cache: &Path, quant: &str) -> Res<(PathBuf, bool, PathBuf, bool)> {
    validate_quant(quant)?;
    let model = cache.join(model_filename(quant));
    let mmproj = cache.join(MMPROJ);
    Ok((
        model.clone(),
        model.is_file(),
        mmproj.clone(),
        mmproj.is_file(),
    ))
}

/// Quant tags whose GGUF is already cached on disk, e.g. ["Q8_0", "Q4_K_M"].
pub fn list_cached_quants(cache: &Path) -> Vec<String> {
    let prefix = "Unlimited-OCR-";
    let suffix = ".gguf";
    let mut quants: Vec<String> = match fs::read_dir(cache) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter_map(|name| {
                name.strip_prefix(prefix)
                    .and_then(|s| s.strip_suffix(suffix))
                    .map(|q| q.to_string())
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    quants.sort();
    quants
}
