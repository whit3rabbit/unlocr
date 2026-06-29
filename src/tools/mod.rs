//! Windows on-demand dependency downloader.
//!
//! Windows has no ubiquitous package manager for the app's external tools, unlike
//! macOS (brew) / Linux (apt/dnf). This module fetches sha256-pinned OFFICIAL release
//! binaries (pandoc, poppler/pdftoppm, llama-server CPU build) and extracts them into
//! the app cache (`<cache>/tools/<name>/`) so OCR + export work without a manual
//! install. Resolved transparently afterward: `preflight::locate` also scans the cache
//! tools dir, so existing callers find a downloaded tool with no change.
//!
//! Windows-only by design (the pin table has no macOS/Linux entries). No
//! redistribution: we ship only the URL + sha256; the binary is downloaded from
//! upstream at runtime, so there is no GPL-bundling obligation for the binaries.

use crate::{Progress, Res};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A pinned downloadable tool: where to fetch it, the sha256 to verify the asset
/// against, and the executable filename to resolve inside the extracted archive.
/// Versioned by the URL; bump `url` + `sha256` together when upgrading.
struct ToolPin {
    /// Stable name the GUI + `locate` use: "pandoc" | "pdftoppm" | "llama-server".
    name: &'static str,
    url: &'static str,
    /// Lowercase-hex sha256 of the asset (the upstream release "digest").
    sha256: &'static str,
    /// Executable filename to find within the extracted tree.
    exe: &'static str,
}

// Pins are per (OS, arch); only the set matching THIS build is compiled. The sha256 of
// each asset is its upstream release digest; bump alongside the URL when upgrading.
//
// Windows x86_64: pandoc, poppler (pdftoppm), llama-server CPU; all .zip with the exe
// (+ DLLs) inside. macOS: ONLY pandoc (a single static binary in a .zip, per-arch);
// poppler has no standalone macOS binary and llama.cpp ships .tar.gz, so both are left
// to Homebrew on macOS (the GUI offers a `brew install` button). Linux: none (deb/rpm
// declare the deps; apt/dnf cover them).

#[cfg(not(test))]
#[cfg(target_os = "windows")]
const PINS: &[ToolPin] = &[
    ToolPin {
        name: "pandoc",
        url: "https://github.com/jgm/pandoc/releases/download/3.10/pandoc-3.10-windows-x86_64.zip",
        sha256: "bb808d00fd58762299d64582a9b4c3e4b106cd929e62c5f19bcdcb496f1e54ae",
        exe: "pandoc.exe",
    },
    ToolPin {
        name: "pdftoppm",
        url: "https://github.com/oschwartz10612/poppler-windows/releases/download/v26.02.0-0/Release-26.02.0-0.zip",
        sha256: "993e4a94376ed712fafc7058d724ea0b943d118bbd2305cd9ed55174eb85cda5",
        exe: "pdftoppm.exe",
    },
    ToolPin {
        // CPU build (>= b8530, satisfies preflight's MIN_BUILD gate). GPU variants
        // (CUDA/Vulkan/HIP) are a large per-driver matrix left to manual install.
        name: "llama-server",
        url: "https://github.com/ggml-org/llama.cpp/releases/download/b9835/llama-b9835-bin-win-cpu-x64.zip",
        sha256: "982860c8dfc36ee82e41aa0885e1f49faa8d7cf07c7481a83f36fb0154e1c64c",
        exe: "llama-server.exe",
    },
];

#[cfg(not(test))]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PINS: &[ToolPin] = &[ToolPin {
    name: "pandoc",
    url: "https://github.com/jgm/pandoc/releases/download/3.10/pandoc-3.10-arm64-macOS.zip",
    sha256: "d9cad01d96ae774a0dc8c8c45bb1ad3e4c5ff2cc2e24f45958f5f9b7974aee34",
    exe: "pandoc",
}];

#[cfg(not(test))]
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const PINS: &[ToolPin] = &[ToolPin {
    name: "pandoc",
    url: "https://github.com/jgm/pandoc/releases/download/3.10/pandoc-3.10-x86_64-macOS.zip",
    sha256: "6334f4d9af7c9e37e761dfad56fa5507685f6d29724ebf31c4be6d5c654a3161",
    exe: "pandoc",
}];

// Any other target (Linux, etc.): nothing is auto-downloaded.
#[cfg(not(test))]
#[cfg(not(any(
    target_os = "windows",
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
)))]
const PINS: &[ToolPin] = &[];

#[cfg(test)]
const PINS: &[ToolPin] = &[ToolPin {
    name: "pandoc",
    url: "https://github.com/jgm/pandoc/releases/download/3.10/pandoc-3.10-arm64-macOS.zip",
    sha256: "d9cad01d96ae774a0dc8c8c45bb1ad3e4c5ff2cc2e24f45958f5f9b7974aee34",
    exe: if cfg!(target_os = "windows") {
        "pandoc.exe"
    } else {
        "pandoc"
    },
}];

/// `<cache>/tools`: the dir holding downloaded tool trees.
pub fn tools_dir(cache: &Path) -> PathBuf {
    cache.join("tools")
}

/// Names of the tools THIS build can auto-download (per OS+arch; empty where none).
/// The GUI uses this to decide whether to show a direct Download button vs a package-
/// manager action/hint.
pub fn downloadable() -> Vec<&'static str> {
    PINS.iter().map(|t| t.name).collect()
}

/// Find an executable named `exe` anywhere under `dir` (bounded-depth walk). Used both
/// to short-circuit `ensure_tool` when a tool is already extracted and by
/// `preflight::locate` to resolve a downloaded tool by name. Returns None if `dir` is
/// absent or the file is not found.
pub fn find_exe(dir: &Path, exe: &str) -> Option<PathBuf> {
    fn walk(dir: &Path, exe: &str, depth: usize) -> Option<PathBuf> {
        if depth > 6 {
            return None;
        }
        let mut subdirs = Vec::new();
        for entry in fs::read_dir(dir).ok()?.flatten() {
            let p = entry.path();
            if p.is_file() {
                if p.file_name().and_then(|n| n.to_str()) == Some(exe) {
                    return Some(p);
                }
            } else if p.is_dir() {
                subdirs.push(p);
            }
        }
        for s in subdirs {
            if let Some(found) = walk(&s, exe, depth + 1) {
                return Some(found);
            }
        }
        None
    }
    walk(dir, exe, 0)
}

/// Ensure tool `name` is present in the cache, downloading + extracting it on first
/// use, and return the path to its executable. Idempotent: a second call finds the
/// already-extracted exe and does no network IO. Only tools in `PINS` for this build's
/// OS+arch can be fetched (Windows: all three; macOS: pandoc; Linux: none); anything
/// else errors with a package-manager hint. The asset is verified against its pinned
/// sha256 before extraction (supply-chain guard); a mismatch is rejected.
pub fn ensure_tool(
    cache: &Path,
    name: &str,
    on_progress: &mut dyn FnMut(Progress),
) -> Res<PathBuf> {
    let pin = PINS.iter().find(|t| t.name == name).ok_or_else(|| {
        format!(
            "{name} is not available for auto-download on this platform; install it with your \
             package manager (macOS: brew, Linux: apt/dnf)"
        )
    })?;

    let dir = tools_dir(cache).join(name);
    if let Some(p) = find_exe(&dir, pin.exe) {
        return Ok(p); // already extracted
    }

    // Create unique temporary directory inside the tools folder to prevent race conditions
    let tools_path = tools_dir(cache);
    fs::create_dir_all(&tools_path)?;

    let tmp_dir = tempfile::Builder::new()
        .prefix(&format!("{name}-"))
        .tempdir_in(&tools_path)?;
    let tmp_path = tmp_dir.path().to_path_buf();

    let zip_file_path = tmp_path.join("download.zip");

    download_to_file(pin.url, &zip_file_path, name, pin.sha256, on_progress)?;

    extract_zip(&zip_file_path, &tmp_path)?;

    // Delete the raw download zip file so it is not moved to the final directory
    let _ = fs::remove_file(&zip_file_path);

    if find_exe(&tmp_path, pin.exe).is_none() {
        return Err(format!("{} not found in the extracted {name} archive", pin.exe).into());
    }

    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    fs::rename(&tmp_path, &dir)?;
    let _ = tmp_dir.keep(); // prevent drop-cleanup since rename succeeded

    find_exe(&dir, pin.exe).ok_or_else(|| {
        format!(
            "{} not found in the extracted {name} archive after renaming",
            pin.exe
        )
        .into()
    })
}

#[cfg(test)]
type MockDownloadFn = Box<dyn Fn(&str, &str, &str) -> Res<Vec<u8>> + 'static>;

#[cfg(test)]
thread_local! {
    static MOCK_DOWNLOAD: std::cell::RefCell<Option<MockDownloadFn>> = std::cell::RefCell::new(None);
}

/// Download `url` directly to disk (flat memory footprint), emitting `Progress::Download` ticks,
/// and verify the body's sha256 equals `expected` (lowercase hex). Errors on mismatch.
fn download_to_file(
    url: &str,
    dest: &Path,
    name: &str,
    expected: &str,
    on_progress: &mut dyn FnMut(Progress),
) -> Res<()> {
    #[cfg(test)]
    {
        let mock = MOCK_DOWNLOAD.with(|m| m.borrow().as_ref().map(|f| f(url, name, expected)));
        if let Some(res) = mock {
            let bytes = res?;
            fs::write(dest, bytes)?;
            return Ok(());
        }
    }

    let url_str = url.to_string();
    let name_str = name.to_string();
    let dest_path = dest.to_path_buf();

    crate::server::block_on(async move {
        let client = reqwest::Client::new();
        let resp = client
            .get(&url_str)
            .timeout(Duration::from_secs(120))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            return Err(Box::<dyn std::error::Error>::from(format!(
                "download failed: HTTP error {status}"
            )));
        }

        let total = resp.content_length().unwrap_or(0);

        let mut file = fs::File::create(&dest_path)?;
        let mut done = 0u64;
        let mut last_pct = u8::MAX;

        on_progress(Progress::Download {
            name: name_str.clone(),
            pct: 0,
            done: 0,
            total,
        });

        let mut resp = resp;
        while let Some(chunk) = resp.chunk().await? {
            std::io::Write::write_all(&mut file, &chunk)?;
            done += chunk.len() as u64;
            if let Some(pct) = (done * 100).checked_div(total).map(|v| v as u8) {
                if pct != last_pct {
                    on_progress(Progress::Download {
                        name: name_str.clone(),
                        pct,
                        done,
                        total,
                    });
                    last_pct = pct;
                }
            }
        }
        file.sync_all()?;
        Ok::<(), Box<dyn std::error::Error>>(())
    })?;

    let actual = crate::model::file_sha256(dest)?;
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(format!(
            "integrity check failed for {name}: sha256 {actual} does not match the pinned \
             digest {expected}. The download was rejected."
        )
        .into());
    }
    Ok(())
}

/// Extract a zip (from disk path) into `dest`, preserving the directory tree
/// (poppler/llama keep their DLLs beside the exe). Guards against zip-slip: an entry
/// whose path escapes `dest` (via `..`, an absolute path, or a drive letter) is
/// rejected. `zip::read::ZipFile::enclosed_name` returns None for such entries.
fn extract_zip(zip_path: &Path, dest: &Path) -> Res<()> {
    let file = fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let rel = match entry.enclosed_name() {
            Some(p) => p,
            None => return Err(format!("unsafe path in archive: {}", entry.name()).into()),
        };
        let out = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
        } else {
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut f = fs::File::create(&out)?;
            std::io::copy(&mut entry, &mut f)?;
            // Preserve the unix exec bit so the extracted binary actually runs (the mac
            // pandoc zip stores mode 0755). Windows ignores this. Without it, a unix
            // extraction yields a non-executable file and the spawn fails.
            #[cfg(unix)]
            if let Some(mode) = entry.unix_mode() {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&out, fs::Permissions::from_mode(mode));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
