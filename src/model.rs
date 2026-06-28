// Resolve the model cache directory and ensure the quant + projector GGUFs
// are present, downloading from Hugging Face on first use.

use crate::Res;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const REPO: &str = "sahilchachra/Unlimited-OCR-GGUF";
const MMPROJ: &str = "mmproj-Unlimited-OCR-F16.gguf";

pub struct ModelFiles {
    pub model: PathBuf,
    pub mmproj: PathBuf,
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

/// GGUF filename for a quant tag, e.g. "Q8_0" -> "Unlimited-OCR-Q8_0.gguf".
fn model_filename(quant: &str) -> String {
    format!("Unlimited-OCR-{quant}.gguf")
}

/// Reject quant tags that could escape the cache dir or manipulate the download
/// URL. `PathBuf::join` does NOT normalize `..`, so `--quant ../../evil` would
/// make `cache.join(name)` (and the `.part` create + `fs::rename`) write outside
/// the cache, and the same string lands unescaped in the Hugging Face URL.
/// Real GGUF quant tags are short alnum + `_.-` (e.g. "Q8_0", "BF16"). The check
/// lives here at the shared sink because both the CLI `--quant` flag and the
/// Tauri `ocr` command's quant field reach it (the latter bypasses clap).
pub fn validate_quant(quant: &str) -> Res<()> {
    let charset_ok = !quant.is_empty()
        && quant
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
    // `.` is allowed (e.g. tag forms), so reject `..` explicitly: it passes the
    // charset check but is a path-traversal component.
    if !charset_ok || quant.contains("..") {
        return Err(format!("invalid quant {quant:?}: allowed characters are [A-Za-z0-9_.-]").into());
    }
    Ok(())
}

/// Ensure the model + projector GGUFs are present, with no progress callback.
/// Equivalent to `ensure_with_progress` with a CLI-printing default sink that
/// reproduces the original `println!`/`print!` download output byte-for-byte.
/// Used by the CLI's `main`/doctor paths and as the fallback for callers that do
/// not care about download events (e.g. tests).
pub fn ensure(cache: &Path, quant: &str) -> Res<ModelFiles> {
    // Default progress sink = the original CLI output: "downloading X ...",
    // "\r  {pct:>3}%  (done / total MiB)", final newline. Keeping it here means
    // every caller that ignores progress still sees the same console behavior.
    let mut cli = |name: &str, pct: Option<u8>, total: u64, done: u64| match pct {
        // None = download-start line
        None => println!("downloading {name} ..."),
        Some(pct) => {
            print!("\r  {pct:>3}%  ({} / {} MiB)", done >> 20, total >> 20);
            let _ = std::io::stdout().flush();
        }
    };
    ensure_inner(cache, quant, &mut cli)
}

/// Like `ensure`, but routes download events through `on_progress` as
/// `Progress::Download { name, pct }` (pct is 0..=100) so a UI can subscribe.
/// The page-per-page loop already emits `Progress::Page` from `lib::ocr_pages`;
/// this closes the gap so the GUI can show download progress too. Called by
/// `run_ocr_job` with the caller's sink, so download + page events share one
/// callback.
pub fn ensure_with_progress<P>(cache: &Path, quant: &str, on_progress: &mut P) -> Res<ModelFiles>
where
    P: FnMut(crate::Progress),
{
    let mut sink = |name: &str, pct: Option<u8>, _total: u64, _done: u64| {
        // Map the low-level (name, pct-or-start) signal to the public Progress
        // enum. pct=None is "download started" -> emit pct=0 so a UI shows the
        // file is underway; otherwise forward the real percent.
        let pct = pct.unwrap_or(0);
        on_progress(crate::Progress::Download {
            name: name.to_string(),
            pct,
        });
    };
    ensure_inner(cache, quant, &mut sink)
}

/// Shared ensure logic. `progress` is a thin closure receiving (name, pct, total,
/// done) where `pct = None` signals the per-file "download started" line and
/// `pct = Some(p)` is a percent tick. Split out so the CLI-printing default
/// (`ensure`) and the enum-emitting variant (`ensure_with_progress`) share one
/// download implementation and cannot drift.
fn ensure_inner<F>(cache: &Path, quant: &str, progress: &mut F) -> Res<ModelFiles>
where
    F: FnMut(&str, Option<u8>, u64, u64),
{
    validate_quant(quant)?;
    let model_name = model_filename(quant);
    let model = cache.join(&model_name);
    let mmproj = cache.join(MMPROJ);

    ensure_file(&model, &model_name, progress)?;
    ensure_file(&mmproj, MMPROJ, progress)?;
    Ok(ModelFiles { model, mmproj })
}

fn ensure_file<F>(path: &Path, name: &str, progress: &mut F) -> Res<()>
where
    F: FnMut(&str, Option<u8>, u64, u64),
{
    if path.is_file() {
        return Ok(());
    }
    let url = format!("https://huggingface.co/{REPO}/resolve/main/{name}");
    download(&url, path, name, progress)?;
    Ok(())
}

/// Stream a URL to `<dest>.part`, then atomically rename. Reports rough progress
/// through `progress(name, pct, total, done)`: one `pct=None` "started" event,
/// then `pct=Some(percent)` ticks whenever the percent changes, and a final
/// `Some(100)`-adjacent flush handled by the caller's newline. The total is
/// unknown when the server omits Content-Length (0), in which case no ticks fire.
fn download<F>(url: &str, dest: &Path, name: &str, progress: &mut F) -> Res<()>
where
    F: FnMut(&str, Option<u8>, u64, u64),
{
    // Connect timeout only, NOT an overall timeout: the body is a multi-GB GGUF
    // and a slow-but-healthy transfer must not be killed. A hung *connect*
    // (HF unreachable) would otherwise block forever, unlike ocr_image (600s
    // overall) and the health poll (2s).
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .build();
    let resp = agent.get(url).call()?;
    let total: u64 = resp
        .header("Content-Length")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let part = dest.with_extension("part");
    let reader = resp.into_reader();

    // Stream into `.part`. On ANY error (including a short/truncated read), delete
    // the partial file before propagating so a corrupt artifact is never left
    // behind for `ensure_file` to mistake for a complete download on the next run.
    match stream_to_part(&part, name, total, reader, progress) {
        Ok(()) => {}
        Err(e) => {
            let _ = fs::remove_file(&part);
            return Err(e);
        }
    }

    fs::rename(&part, dest)?;
    Ok(())
}

/// Read `reader` fully into `part`, emitting progress, and verify the byte count
/// matches Content-Length when known. Split out so `download` can clean up the
/// `.part` file on any failure path with a single `match`.
fn stream_to_part<F, R>(
    part: &Path,
    name: &str,
    total: u64,
    mut reader: R,
    progress: &mut F,
) -> Res<()>
where
    F: FnMut(&str, Option<u8>, u64, u64),
    R: Read,
{
    let mut out = fs::File::create(part)?;

    // "download started" line, matching the original println! placement.
    progress(name, None, total, 0);

    let mut buf = vec![0u8; 1 << 20]; // 1 MiB
    let mut done: u64 = 0;
    let mut last_pct = u64::MAX;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        if total > 0 {
            let pct = done * 100 / total;
            if pct != last_pct {
                progress(name, Some(pct as u8), total, done);
                last_pct = pct;
            }
        }
    }
    if total > 0 {
        // Final newline after the last "\r  {pct}%  ..." line, matching the
        // original download()'s closing println!().
        println!();
        // A dropped connection yields read()==0 (EOF) early, indistinguishable
        // from a real end-of-stream. With a known length we can catch it: a short
        // file is a truncated download, not a valid GGUF. Reject it so the caller
        // deletes the `.part` instead of caching a corrupt model forever.
        if done != total {
            return Err(format!(
                "truncated download of {name}: got {done} of {total} bytes (connection dropped?)"
            )
            .into());
        }
    } else {
        // No Content-Length: an early EOF (dropped connection) is indistinguishable
        // from a real end-of-stream, so we cannot tell a complete GGUF from a
        // truncated one. Fail loud rather than rename a possibly-partial multi-GB
        // file to the final path and cache corruption forever (the previous bug:
        // the truncation check was nested under `total > 0`).
        // ponytail: HF's CDN always sends Content-Length, so this only fires behind
        // a proxy that strips it; retry usually fixes it. Switch to a checksum
        // verify against the HF metadata if length-less sources must be supported.
        return Err(format!(
            "download of {name} reported no Content-Length; cannot verify it is complete \
             (got {done} bytes). Retry, or check for a proxy stripping the header."
        )
        .into());
    }
    out.sync_all()?;
    Ok(())
}

/// Check whether the model and projector GGUFs for `quant` exist in `cache`.
/// Returns `Err` on an invalid quant (same guard as `ensure`): PathBuf::join does
/// not normalize `..`, so a traversing quant would otherwise let the caller probe
/// for file existence outside the cache dir (a file-existence oracle). Callers that
/// already call `validate_quant` separately get defense in depth.
pub fn check_presence(cache: &Path, quant: &str) -> Res<(PathBuf, bool, PathBuf, bool)> {
    validate_quant(quant)?;
    let model = cache.join(model_filename(quant));
    let mmproj = cache.join(MMPROJ);
    Ok((model.clone(), model.is_file(), mmproj.clone(), mmproj.is_file()))
}

/// Quant tags whose GGUF is already cached on disk, e.g. ["Q8_0", "Q4_K_M"].
/// Scans `cache` for `Unlimited-OCR-<quant>.gguf` (the mmproj filename starts
/// with `mmproj-` so it is not matched). Sorted for a stable UI order; an
/// unreadable cache dir yields an empty list. Powers the GUI's model detection.
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

#[cfg(test)]
mod tests {
    use super::{
        cache_dir, check_presence, ensure_with_progress, list_cached_quants, model_filename,
        stream_to_part, validate_quant, MMPROJ,
    };
    use crate::Progress;
    use std::io::Cursor;

    #[test]
    fn list_cached_quants_matches_model_files_only() {
        // Only Unlimited-OCR-<quant>.gguf counts; the mmproj and unrelated files
        // are ignored, and the result is sorted.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join(model_filename("Q8_0")), b"x").unwrap();
        std::fs::write(dir.join(model_filename("Q4_K_M")), b"x").unwrap();
        std::fs::write(dir.join(MMPROJ), b"x").unwrap(); // must NOT be listed
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        assert_eq!(list_cached_quants(dir), vec!["Q4_K_M", "Q8_0"]);
        // Missing dir -> empty, no panic.
        assert!(list_cached_quants(&dir.join("nope")).is_empty());
    }

    #[test]
    fn stream_to_part_rejects_truncated_download() {
        // A dropped connection looks like an early EOF. With a known Content-Length
        // we must reject a short body so download() deletes the .part instead of
        // caching a corrupt GGUF. Claim 100 bytes, deliver 10.
        let tmp = tempfile::tempdir().unwrap();
        let part = tmp.path().join("model.part");
        let err = stream_to_part(
            &part,
            "model",
            100,
            Cursor::new(vec![0u8; 10]),
            &mut |_, _, _, _| {},
        )
        .expect_err("short body must be rejected");
        assert!(err.to_string().contains("truncated"), "got: {err}");
    }

    #[test]
    fn stream_to_part_rejects_missing_content_length() {
        // total==0 means the server omitted Content-Length: we cannot verify the
        // body is complete, so a partial/complete stream must be rejected rather
        // than renamed to the final GGUF and cached as a corrupt model.
        let tmp = tempfile::tempdir().unwrap();
        let part = tmp.path().join("model.part");
        let err = stream_to_part(
            &part,
            "model",
            0,
            Cursor::new(vec![0u8; 10]),
            &mut |_, _, _, _| {},
        )
        .expect_err("missing Content-Length must be rejected");
        assert!(err.to_string().contains("no Content-Length"), "got: {err}");
    }

    #[test]
    fn stream_to_part_accepts_full_download() {
        // Exact byte count = success; the file is written in full.
        let tmp = tempfile::tempdir().unwrap();
        let part = tmp.path().join("model.part");
        stream_to_part(
            &part,
            "model",
            10,
            Cursor::new(vec![7u8; 10]),
            &mut |_, _, _, _| {},
        )
        .expect("full body must succeed");
        assert_eq!(std::fs::metadata(&part).unwrap().len(), 10);
    }

    #[test]
    fn filename_format() {
        assert_eq!(model_filename("Q8_0"), "Unlimited-OCR-Q8_0.gguf");
        assert_eq!(model_filename("BF16"), "Unlimited-OCR-BF16.gguf");
    }

    #[test]
    fn validate_quant_blocks_traversal() {
        for ok in ["Q8_0", "BF16", "Q4_K_M", "F16.test"] {
            assert!(validate_quant(ok).is_ok(), "{ok} should be accepted");
        }
        for bad in [
            "",
            "..",
            "Q8_0/../../evil",
            "../../../../etc/passwd",
            "Q8_0/sub",
            "Q8_0\\sub",
            "a b",
            "x'; rm -rf /; echo '",
        ] {
            assert!(validate_quant(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn check_presence_rejects_traversal_quant() {
        // check_presence is a read path (is_file), but PathBuf::join does not
        // normalize `..` so a traversing quant would probe for file existence
        // outside the cache dir. The validate_quant guard at the top must fire.
        let tmp = tempfile::tempdir().unwrap();
        let err = check_presence(tmp.path(), "Q8_0/../../evil")
            .expect_err("traversing quant must be rejected");
        assert!(err.to_string().contains("invalid quant"), "got: {err}");
        // A valid quant must still succeed (returns false for absent files).
        let (model, model_ok, mmproj, mmproj_ok) =
            check_presence(tmp.path(), "Q8_0").expect("valid quant must not error");
        assert!(!model_ok, "model should be absent in empty tmp dir");
        assert!(!mmproj_ok, "mmproj should be absent in empty tmp dir");
        assert!(model.ends_with("Unlimited-OCR-Q8_0.gguf"));
        let _ = mmproj; // path returned, just not checked for name here
    }

    #[test]
    fn ensure_rejects_traversal_quant() {
        // The write path (ensure_inner -> File::create / fs::rename) must refuse
        // a traversing quant before touching the filesystem.
        let tmp = tempfile::tempdir().unwrap();
        let err = match ensure_with_progress(tmp.path(), "Q8_0/../../evil", &mut |_| {}) {
            Ok(_) => panic!("traversing quant should be rejected"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("invalid quant"), "got: {err}");
    }

    #[test]
    fn cache_dir_override_used_and_created() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("nested/cache");
        let got = cache_dir(Some(target.clone())).unwrap();
        assert_eq!(got, target);
        assert!(target.is_dir()); // create_dir_all ran
    }

    #[test]
    fn ensure_with_progress_no_events_when_files_present() {
        // Non-network fast path: when both GGUFs already exist on disk, the
        // download sink must never fire and ModelFiles must resolve to the
        // canonical cache paths. Locks the additive progress plumbing so a GUI
        // caller that pre-seeded the cache gets zero Download events.
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path();
        std::fs::write(cache.join(model_filename("Q8_0")), b"stub").unwrap();
        std::fs::write(cache.join("mmproj-Unlimited-OCR-F16.gguf"), b"stub").unwrap();

        let mut events: Vec<Progress> = Vec::new();
        let files = ensure_with_progress(cache, "Q8_0", &mut |p| events.push(p)).unwrap();
        assert!(events.is_empty(), "no download events expected, got {events:?}");
        assert_eq!(files.model, cache.join("Unlimited-OCR-Q8_0.gguf"));
        assert_eq!(
            files.mmproj,
            cache.join("mmproj-Unlimited-OCR-F16.gguf")
        );
    }
}
