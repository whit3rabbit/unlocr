use serde::Serialize;
use std::path::{Path, PathBuf};
use unlocr::OcrOptions;

/// Structured preflight result the frontend can render as a status panel.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreflightReport {
    ok: bool,
    build_number: Option<u64>,
    llama_server: Option<String>,
    /// Which llama-server the resolver picked: "managed" = unlocr's patched
    /// R-SWA build (has the Unlimited-OCR vision patch, PR #24975), "external" =
    /// a PATH/Homebrew/override binary that CANNOT be verified for that patch and
    /// is the common cause of the `ocr-ocr` repetition loops. None when no
    /// llama-server was found. The frontend warns on "external".
    provenance: Option<String>,
    pdftoppm: Option<String>,
    model_present: bool,
    mmproj_present: bool,
    quant: String,
    error: Option<String>,
}

/// Return the model cache directory path and the total size of its GGUF files in
/// bytes.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CacheInfo {
    path: String,
    size_bytes: u64,
}

/// Validate the runtime environment before a run: locate llama-server + pdftoppm,
/// read llama-server's build number, and check the model/projector GGUFs for the
/// given quant are present in the cache.
#[tauri::command]
pub(crate) fn preflight(
    llama_bin: Option<String>,
    quant: Option<String>,
) -> Result<PreflightReport, String> {
    let llama_override = llama_bin.map(PathBuf::from);
    let quant = quant
        .map(|q| {
            if q.trim().is_empty() {
                OcrOptions::default().quant
            } else {
                q
            }
        })
        .unwrap_or_else(|| OcrOptions::default().quant);

    if let Err(e) = unlocr::model::validate_quant(&quant) {
        return Ok(PreflightReport {
            ok: false,
            build_number: None,
            llama_server: None,
            provenance: None,
            pdftoppm: None,
            model_present: false,
            mmproj_present: false,
            quant,
            error: Some(e.to_string()),
        });
    }

    let cache = match unlocr::model::cache_dir(None) {
        Ok(c) => c,
        Err(e) => {
            return Ok(PreflightReport {
                ok: false,
                build_number: None,
                llama_server: None,
                provenance: None,
                pdftoppm: None,
                model_present: false,
                mmproj_present: false,
                quant,
                error: Some(format!("could not resolve model cache dir: {e}")),
            });
        }
    };

    // Passive status report: resolve WITHOUT downloading (find_llama_server), so
    // opening the model panel never triggers the multi-hundred-MB managed download.
    // Keep the Provenance so the frontend can warn when a stock/external binary
    // (the repetition-loop cause) is in use instead of the managed R-SWA build.
    let llama_resolved = unlocr::preflight::find_llama_server(llama_override.as_deref());
    let provenance = llama_resolved.as_ref().map(|(_, prov)| {
        match prov {
            unlocr::preflight::Provenance::Managed => "managed",
            unlocr::preflight::Provenance::External => "external",
        }
        .to_string()
    });
    let llama = llama_resolved.map(|(p, _)| p);
    let pdftoppm_path = unlocr::preflight::locate("pdftoppm");
    let tools = match (llama, pdftoppm_path) {
        (Some(llama_server), Some(pdftoppm)) => unlocr::preflight::Tools {
            llama_server,
            pdftoppm,
        },
        (llama, pdftoppm_path) => {
            let (_, model_present, _, mmproj_present) = unlocr::model::check_presence(
                &cache, &quant,
            )
            .unwrap_or((Default::default(), false, Default::default(), false));
            let mut missing = Vec::new();
            if llama.is_none() {
                missing.push("llama-server");
            }
            if pdftoppm_path.is_none() {
                missing.push("pdftoppm");
            }
            return Ok(PreflightReport {
                ok: false,
                build_number: None,
                llama_server: llama
                    .map(|p| p.display().to_string())
                    .or_else(|| llama_override.map(|p| p.display().to_string())),
                provenance: provenance.clone(),
                pdftoppm: pdftoppm_path.map(|p| p.display().to_string()),
                model_present,
                mmproj_present,
                quant,
                error: Some(format!("missing dependencies: {}", missing.join(", "))),
            });
        }
    };

    let build_number = unlocr::preflight::build_number(&tools.llama_server);
    let (_, model_present, _, mmproj_present) = unlocr::model::check_presence(&cache, &quant)
        .unwrap_or((Default::default(), false, Default::default(), false));

    Ok(PreflightReport {
        ok: true,
        build_number,
        llama_server: Some(tools.llama_server.display().to_string()),
        provenance,
        pdftoppm: Some(tools.pdftoppm.display().to_string()),
        model_present,
        mmproj_present,
        quant,
        error: None,
    })
}

/// Quant tags already downloaded to the model cache, for the model picker.
#[tauri::command]
pub(crate) fn list_local_models() -> Vec<String> {
    match unlocr::model::cache_dir(None) {
        Ok(cache) => unlocr::model::list_cached_quants(&cache),
        Err(_) => Vec::new(),
    }
}

/// One entry in the quant picker: the published quant tag, its exact download
/// size, an optional best/good/less tier alias, and whether it's already cached.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AvailableQuant {
    name: String,
    size_bytes: u64,
    tier: Option<String>,
    cached: bool,
}

/// Pure core of `list_available_quants`, over an explicit cached-quant set so
/// tests can supply one without touching the real cache dir.
fn available_quants(cached: &std::collections::HashSet<String>) -> Vec<AvailableQuant> {
    unlocr::model::known_quants()
        .iter()
        .map(|q| AvailableQuant {
            name: q.name.to_string(),
            size_bytes: q.size_bytes,
            tier: q.tier.map(|t| t.to_string()),
            cached: cached.contains(q.name),
        })
        .collect()
}

/// The full published quant lineup (all 13 tags, not just the 3 CLI Quality
/// tiers) with size/tier/cached-state, for a dynamic quant `<select>`. Falls
/// back to "none cached" (rather than erroring) if the cache dir can't be
/// resolved, so the dropdown still renders the full lineup, just all
/// not-yet-downloaded.
#[tauri::command]
pub(crate) fn list_available_quants() -> Vec<AvailableQuant> {
    let cached: std::collections::HashSet<String> = unlocr::model::cache_dir(None)
        .map(|c| unlocr::model::list_cached_quants(&c).into_iter().collect())
        .unwrap_or_default();
    available_quants(&cached)
}

/// Top-level model-cache artifacts: the model GGUFs plus any interrupted-download
/// `.part` files (download.rs writes `<name>.part` until the integrity check
/// passes). Shared by get_cache_info (so the size figure includes partials) and
/// clear_model_cache (so Clear Cache does not leave multi-GB partials behind).
fn is_cache_artifact(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|x| x.to_str()),
        Some("gguf") | Some("part")
    )
}

/// Recursively sum the byte size of every file under `dir`.
fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            match e.metadata() {
                Ok(m) if m.is_dir() => total += dir_size(&e.path()),
                Ok(m) => total += m.len(),
                Err(_) => {}
            }
        }
    }
    total
}

#[tauri::command]
pub(crate) fn get_cache_info() -> Result<CacheInfo, String> {
    let cache = unlocr::model::cache_dir(None).map_err(|e| e.to_string())?;
    let path = cache.display().to_string();
    let artifact_bytes: u64 = std::fs::read_dir(&cache)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| is_cache_artifact(&e.path()))
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0);
    let size_bytes = artifact_bytes + dir_size(&cache.join("previews"));
    Ok(CacheInfo { path, size_bytes })
}

/// One cached GGUF file (model quant or mmproj) for the Settings model-management
/// table: name, size, sha256, and last-modified (unix epoch seconds, matching the
/// job store's `created_at` convention so the frontend's existing `formatEpoch`
/// helper renders it with no new date-formatting code).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CachedFileInfo {
    name: String,
    size_bytes: u64,
    sha256: String,
    modified: Option<u64>,
}

/// Pure core of `list_cached_files`, over an explicit `cache` dir so tests can
/// point it at a tempdir instead of the real per-OS cache. Reuses
/// `unlocr::model::file_sha256` (already streamed/1MiB-chunked; no new hashing
/// logic). `.part` files (interrupted downloads) are excluded: they are not a
/// usable model, and `clear_model_cache` already treats them separately from
/// finished `.gguf` files.
///
/// Hashing every cached file on each Settings-open costs real seconds for
/// multi-GB GGUFs; acceptable since opening Settings is an explicit, infrequent
/// user action, not a hot path.
fn list_cached_files_in(cache: &Path) -> Result<Vec<CachedFileInfo>, String> {
    let mut out = Vec::new();
    let entries = std::fs::read_dir(cache).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("gguf") {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let meta = entry.metadata().map_err(|e| e.to_string())?;
        let sha256 = unlocr::model::file_sha256(&path).map_err(|e| e.to_string())?;
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        out.push(CachedFileInfo {
            name,
            size_bytes: meta.len(),
            sha256,
            modified,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Per-file listing of every cached GGUF (model quants + mmproj) with name, size,
/// sha256, and mtime, for the Settings model-management table.
#[tauri::command]
pub(crate) fn list_cached_files() -> Result<Vec<CachedFileInfo>, String> {
    let cache = unlocr::model::cache_dir(None).map_err(|e| e.to_string())?;
    list_cached_files_in(&cache)
}

/// `filename` is renderer-supplied (echoed back from a model-files table row),
/// so it is guarded like a quant tag (`unlocr::model::validate_quant`'s charset
/// check): reject anything containing a path separator or `..`, and require the
/// `.gguf` extension `list_cached_files` itself only ever returns.
fn is_safe_cache_filename(filename: &str) -> bool {
    !filename.is_empty()
        && !filename.contains('/')
        && !filename.contains('\\')
        && !filename.contains("..")
        && filename.ends_with(".gguf")
}

/// Pure core of `remove_cached_file`, over an explicit `cache` dir so tests can
/// point it at a tempdir. Confirms the resolved path's parent is exactly the
/// cache dir as defense in depth against any unexpected `join()` behavior.
fn remove_cached_file_in(cache: &Path, filename: &str) -> Result<(), String> {
    if !is_safe_cache_filename(filename) {
        return Err(format!("invalid filename {filename:?}"));
    }
    let path = cache.join(filename);
    if path.parent() != Some(cache) {
        return Err("resolved path escapes the model cache dir".to_string());
    }
    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Delete one cached GGUF by filename.
#[tauri::command]
pub(crate) fn remove_cached_file(filename: String) -> Result<(), String> {
    let cache = unlocr::model::cache_dir(None).map_err(|e| e.to_string())?;
    remove_cached_file_in(&cache, &filename)
}

/// Delete all model GGUFs and interrupted-download `.part` files from the model
/// cache AND the previews/ dir.
#[tauri::command]
pub(crate) fn clear_model_cache() -> Result<(), String> {
    let cache = unlocr::model::cache_dir(None).map_err(|e| e.to_string())?;
    let entries = std::fs::read_dir(&cache).map_err(|e| e.to_string())?;
    let mut errors: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if is_cache_artifact(&path) {
            if let Err(e) = std::fs::remove_file(&path) {
                errors.push(format!("{}: {e}", path.display()));
            }
        }
    }
    match std::fs::remove_dir_all(cache.join("previews")) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => errors.push(format!("previews/: {e}")),
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "some files could not be removed: {}",
            errors.join("; ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `available_quants` returns all 13 known quants regardless of cache
    /// state, flagging only the ones present in the given cached-set.
    #[test]
    fn available_quants_reports_full_lineup_and_cached_flags() {
        let mut cached = std::collections::HashSet::new();
        cached.insert("Q8_0".to_string());

        let quants = available_quants(&cached);

        assert_eq!(quants.len(), unlocr::model::known_quants().len());
        let q8 = quants.iter().find(|q| q.name == "Q8_0").unwrap();
        assert!(q8.cached);
        assert_eq!(q8.tier.as_deref(), Some("good"));
        let q6k = quants.iter().find(|q| q.name == "Q6_K").unwrap();
        assert!(!q6k.cached);
        assert_eq!(q6k.tier, None);
    }

    /// `is_safe_cache_filename` rejects any path-traversal shape and requires
    /// the `.gguf` extension `list_cached_files` always returns; a plain
    /// filename is accepted.
    #[test]
    fn is_safe_cache_filename_rejects_traversal_and_bad_extension() {
        assert!(!is_safe_cache_filename("../x.gguf"));
        assert!(!is_safe_cache_filename("a/b.gguf"));
        assert!(!is_safe_cache_filename("a\\b.gguf"));
        assert!(!is_safe_cache_filename(""));
        assert!(!is_safe_cache_filename("notgguf.txt"));
        assert!(is_safe_cache_filename("Unlimited-OCR-Q8_0.gguf"));
    }

    /// `remove_cached_file_in` refuses an unsafe filename before touching disk
    /// (no file is created in this test, so a successful delete would itself
    /// prove the guard did not run).
    #[test]
    fn remove_cached_file_in_rejects_unsafe_filename() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(remove_cached_file_in(tmp.path(), "../escape.gguf").is_err());
    }

    /// `list_cached_files_in` reports name/size/sha256 for `.gguf` files only,
    /// skipping `.part` artifacts, matching real content hashes.
    #[test]
    fn list_cached_files_in_reports_gguf_only_with_correct_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let gguf_path = tmp.path().join("Unlimited-OCR-Q8_0.gguf");
        std::fs::write(&gguf_path, b"fake gguf bytes").unwrap();
        std::fs::write(
            tmp.path().join("Unlimited-OCR-Q4_K_M.gguf.part"),
            b"partial",
        )
        .unwrap();

        let files = list_cached_files_in(tmp.path()).unwrap();

        assert_eq!(files.len(), 1, "the .part file must be excluded");
        let f = &files[0];
        assert_eq!(f.name, "Unlimited-OCR-Q8_0.gguf");
        assert_eq!(f.size_bytes, b"fake gguf bytes".len() as u64);
        assert_eq!(f.sha256, unlocr::model::sha256_hex(b"fake gguf bytes"));
        assert!(f.modified.is_some());
    }

    /// `remove_cached_file_in` deletes exactly the named file and leaves
    /// sibling cache files untouched.
    #[test]
    fn remove_cached_file_in_deletes_only_named_file() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("Unlimited-OCR-Q8_0.gguf");
        let sibling = tmp.path().join("Unlimited-OCR-Q4_K_M.gguf");
        std::fs::write(&target, b"a").unwrap();
        std::fs::write(&sibling, b"b").unwrap();

        remove_cached_file_in(tmp.path(), "Unlimited-OCR-Q8_0.gguf").unwrap();

        assert!(!target.exists());
        assert!(sibling.exists());
    }
}
