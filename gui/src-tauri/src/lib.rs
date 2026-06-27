// Tauri backend for the Ferrum OCR shell scaffold.
//
// SCOPE NOTE: this card (EH-0001) only stands up a buildable Tauri shell. The
// real backend bridge (preflight + run_ocr over the `unlocr` library, with
// per-page progress events) is wired in later cards (EH-0002 extracts the lib,
// EH-0003 exposes the commands). Until then the commands below are intentional
// stubs so this crate compiles standalone, with no path dependency on the root
// `unlocr` crate (which is still a pure binary at this point). Keeping that
// decoupling is what lets `git status -- src/` stay clean here.

/// Placeholder preflight. Returns a fixed "not wired yet" message so the UI can
/// be exercised without a backend. EH-0003 replaces this with a real
/// `unlocr::preflight::check` call once the library is extracted.
#[tauri::command]
fn preflight(_llama_bin: Option<String>) -> Result<String, String> {
    Ok("Preflight not wired yet (see EH-0003).".to_string())
}

/// Placeholder OCR command. Does not run the pipeline. EH-0003 replaces this
/// with a `spawn_blocking` call into `unlocr::run_ocr`.
#[tauri::command]
async fn ocr(
    _inputs: Vec<String>,
    _out: String,
    _quant: Option<String>,
) -> Result<Vec<String>, String> {
    Err("OCR not wired yet (see EH-0003).".to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![preflight, ocr])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
