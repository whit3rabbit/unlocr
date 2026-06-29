use serde::Serialize;
use tauri::{AppHandle, Emitter};
use unlocr::Progress;

/// Per-tool availability for the Settings "Dependencies" panel.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStatus {
    pub name: String,
    pub found: bool,
    pub path: Option<String>,
    pub downloadable: bool,
}

/// The OS this build targets: "windows" | "macos" | "linux" | "unknown".
#[tauri::command]
pub(crate) fn host_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}

/// Report which external tools resolve and which can be auto-downloaded (Windows only).
#[tauri::command]
pub(crate) fn list_tools() -> Vec<ToolStatus> {
    let dl = unlocr::tools::downloadable();
    ["pandoc", "pdftoppm", "llama-server"]
        .iter()
        .map(|name| {
            let path = unlocr::preflight::locate(name);
            ToolStatus {
                name: (*name).to_string(),
                found: path.is_some(),
                path: path.map(|p| p.display().to_string()),
                downloadable: dl.contains(name),
            }
        })
        .collect()
}

/// Progress payload for `tool://download`.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolDownload {
    name: String,
    pct: u8,
    done: u64,
    total: u64,
}

/// Download + extract a pinned external tool (pandoc / pdftoppm / llama-server) into the app cache.
#[tauri::command]
pub(crate) async fn download_tool(app: AppHandle, name: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let cache = unlocr::model::cache_dir(None).map_err(|e| e.to_string())?;
        let mut on_progress = |p: Progress| {
            if let Progress::Download {
                name,
                pct,
                done,
                total,
            } = p
            {
                let _ = app.emit(
                    "tool://download",
                    ToolDownload {
                        name,
                        pct,
                        done,
                        total,
                    },
                );
            }
        };
        let path = unlocr::tools::ensure_tool(&cache, &name, &mut on_progress)
            .map_err(|e| e.to_string())?;
        Ok(path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("tool download worker join failed: {e}"))?
}

/// Whether Homebrew is on PATH.
#[tauri::command]
pub(crate) fn brew_available() -> bool {
    unlocr::preflight::locate("brew").is_some()
}

/// Run `brew install <formula>` for one of the app's known formulae and return brew's combined output.
#[tauri::command]
pub(crate) async fn brew_install(formula: String) -> Result<String, String> {
    const ALLOWED: &[&str] = &["poppler", "llama.cpp", "pandoc"];
    if !ALLOWED.contains(&formula.as_str()) {
        return Err(format!("not an installable formula: {formula}"));
    }
    let brew = unlocr::preflight::locate("brew")
        .ok_or_else(|| "Homebrew not found on PATH. Install it from https://brew.sh".to_string())?;
    tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let out = std::process::Command::new(&brew)
            .arg("install")
            .arg(&formula)
            .output()
            .map_err(|e| format!("failed to run brew: {e}"))?;
        if out.status.success() {
            Ok(format!("installed {formula}"))
        } else {
            Err(format!(
                "brew install {formula} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    })
    .await
    .map_err(|e| format!("brew worker join failed: {e}"))?
}
