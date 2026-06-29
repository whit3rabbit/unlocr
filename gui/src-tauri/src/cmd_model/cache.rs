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
                pdftoppm: None,
                model_present: false,
                mmproj_present: false,
                quant,
                error: Some(format!("could not resolve model cache dir: {e}")),
            });
        }
    };

    let tools = match unlocr::preflight::check(llama_override.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            let (_, model_present, _, mmproj_present) = unlocr::model::check_presence(
                &cache, &quant,
            )
            .unwrap_or((Default::default(), false, Default::default(), false));
            return Ok(PreflightReport {
                ok: false,
                build_number: None,
                llama_server: llama_override.map(|p| p.display().to_string()),
                pdftoppm: None,
                model_present,
                mmproj_present,
                quant,
                error: Some(e.to_string()),
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
