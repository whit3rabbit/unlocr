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
use std::io::Read;
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

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PINS: &[ToolPin] = &[ToolPin {
    name: "pandoc",
    url: "https://github.com/jgm/pandoc/releases/download/3.10/pandoc-3.10-arm64-macOS.zip",
    sha256: "d9cad01d96ae774a0dc8c8c45bb1ad3e4c5ff2cc2e24f45958f5f9b7974aee34",
    exe: "pandoc",
}];

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const PINS: &[ToolPin] = &[ToolPin {
    name: "pandoc",
    url: "https://github.com/jgm/pandoc/releases/download/3.10/pandoc-3.10-x86_64-macOS.zip",
    sha256: "6334f4d9af7c9e37e761dfad56fa5507685f6d29724ebf31c4be6d5c654a3161",
    exe: "pandoc",
}];

// Any other target (Linux, etc.): nothing is auto-downloaded.
#[cfg(not(any(
    target_os = "windows",
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
)))]
const PINS: &[ToolPin] = &[];

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
    fs::create_dir_all(&dir)?;

    // Tool zips are tens of MB: download to memory, verify the hash (a truncated body
    // simply fails the check), then extract. No `.part`/resume needed at this size.
    let bytes = download_bytes(pin.url, name, pin.sha256, on_progress)?;
    extract_zip(&bytes, &dir)?;

    find_exe(&dir, pin.exe)
        .ok_or_else(|| format!("{} not found in the extracted {name} archive", pin.exe).into())
}

/// Download `url` into memory, emitting `Progress::Download` ticks, and verify the
/// body's sha256 equals `expected` (lowercase hex). Errors (and downloads nothing
/// further) on mismatch.
fn download_bytes(
    url: &str,
    name: &str,
    expected: &str,
    on_progress: &mut dyn FnMut(Progress),
) -> Res<Vec<u8>> {
    // Connect timeout + per-read watchdog (resets each chunk), no overall timeout:
    // same posture as the model downloader.
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(120))
        .build();
    let resp = agent.get(url).call()?;
    let total: u64 = resp
        .header("Content-Length")
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);

    let mut reader = resp.into_reader();
    // Cap the preallocation so a bogus Content-Length cannot request a huge Vec.
    let mut bytes = Vec::with_capacity((total.min(256 << 20)) as usize);
    let mut buf = vec![0u8; 1 << 16];
    let mut done: u64 = 0;
    let mut last_pct = u8::MAX;
    on_progress(Progress::Download {
        name: name.to_string(),
        pct: 0,
        done: 0,
        total,
    });
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
        done += n as u64;
        if let Some(pct) = (done * 100).checked_div(total).map(|v| v as u8) {
            if pct != last_pct {
                on_progress(Progress::Download {
                    name: name.to_string(),
                    pct,
                    done,
                    total,
                });
                last_pct = pct;
            }
        }
    }

    let actual = crate::model::sha256_hex(&bytes);
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(format!(
            "integrity check failed for {name}: sha256 {actual} does not match the pinned \
             digest {expected}. The download was rejected."
        )
        .into());
    }
    Ok(bytes)
}

/// Extract a zip (from an in-memory buffer) into `dest`, preserving the directory tree
/// (poppler/llama keep their DLLs beside the exe). Guards against zip-slip: an entry
/// whose path escapes `dest` (via `..`, an absolute path, or a drive letter) is
/// rejected. `zip::read::ZipFile::enclosed_name` returns None for such entries.
fn extract_zip(bytes: &[u8], dest: &Path) -> Res<()> {
    use std::io::Cursor;
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
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
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a tiny in-memory zip with the given (path, bytes) entries.
    fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, data) in entries {
                w.start_file(*name, opts).unwrap();
                w.write_all(data).unwrap();
            }
            w.finish().unwrap();
        }
        buf
    }

    #[test]
    fn extract_then_find_exe_in_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        // Mirror poppler's layout: exe nested under a versioned dir + Library/bin.
        let zip = make_zip(&[
            ("poppler-26/Library/bin/pdftoppm.exe", b"MZ binary"),
            ("poppler-26/Library/bin/libfoo.dll", b"dll"),
            ("poppler-26/share/readme.txt", b"hi"),
        ]);
        let dest = tmp.path().join("pdftoppm");
        extract_zip(&zip, &dest).unwrap();
        let found = find_exe(&dest, "pdftoppm.exe").expect("exe resolved in nested dir");
        assert!(found.ends_with("Library/bin/pdftoppm.exe"));
        // The DLL beside it survives (directory tree preserved).
        assert!(found.with_file_name("libfoo.dll").exists());
    }

    /// On unix the extracted binary must keep its exec bit (mode 0755 in the archive),
    /// or the spawned tool won't run. Guards the macOS pandoc path.
    #[cfg(unix)]
    #[test]
    fn extract_preserves_exec_bit() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o755);
            w.start_file("pandoc-3.10/bin/pandoc", opts).unwrap();
            w.write_all(b"#!/bin/sh\n").unwrap();
            w.finish().unwrap();
        }
        let dest = tmp.path().join("pandoc");
        extract_zip(&buf, &dest).unwrap();
        let exe = find_exe(&dest, "pandoc").expect("pandoc resolved");
        let mode = fs::metadata(&exe).unwrap().permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "extracted binary not executable: {mode:o}"
        );
    }

    #[test]
    fn extract_rejects_zip_slip() {
        let tmp = tempfile::tempdir().unwrap();
        let zip = make_zip(&[("../escape.exe", b"evil")]);
        let dest = tmp.path().join("t");
        let err = extract_zip(&zip, &dest).unwrap_err();
        assert!(
            err.to_string().contains("unsafe path"),
            "expected zip-slip rejection, got: {err}"
        );
        // Nothing was written outside dest.
        assert!(!tmp.path().join("escape.exe").exists());
    }

    /// Every pin compiled for THIS build is well-formed: https url, 64-hex sha256, a
    /// non-empty exe whose extension matches the OS convention (.exe on Windows, bare
    /// elsewhere). Runs per-host, so each platform's CI validates its own `PINS`.
    #[test]
    fn pins_are_well_formed() {
        for t in PINS {
            assert!(t.url.starts_with("https://"), "{} url not https", t.name);
            assert_eq!(t.sha256.len(), 64, "{} sha256 not 64 hex chars", t.name);
            assert!(
                t.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                "{} sha256 not hex",
                t.name
            );
            assert!(!t.exe.is_empty(), "{} has empty exe", t.name);
            if cfg!(target_os = "windows") {
                assert!(t.exe.ends_with(".exe"), "{} exe should be .exe", t.name);
            } else {
                assert!(!t.exe.ends_with(".exe"), "{} exe should be bare", t.name);
            }
        }
    }

    #[test]
    fn find_exe_missing_dir_is_none() {
        let missing = std::env::temp_dir().join("unlocr-no-such-dir-xyz-123");
        assert!(find_exe(&missing, "pandoc").is_none());
    }

    /// `downloadable()` exactly mirrors the pins compiled for this build (Windows: three;
    /// macOS: pandoc; Linux/other: none). Per-host, so each OS's CI asserts its own set.
    #[test]
    fn downloadable_matches_pins() {
        let got = downloadable();
        let want: Vec<&str> = PINS.iter().map(|t| t.name).collect();
        assert_eq!(got, want);
        if cfg!(not(any(target_os = "windows", target_os = "macos"))) {
            assert!(got.is_empty(), "Linux/other must not offer auto-download");
        }
    }

    /// A tool with no pin for this platform is refused BEFORE any network IO, with a
    /// package-manager hint. Uses a name that is never pinned, so it is platform-agnostic
    /// (won't trigger a real download on Windows/macOS where real tools are pinned).
    #[test]
    fn ensure_tool_unknown_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let err = ensure_tool(tmp.path(), "definitely-not-a-tool", &mut |_| {}).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not available") && msg.contains("package manager"),
            "expected a package-manager hint, got: {msg}"
        );
    }
}
