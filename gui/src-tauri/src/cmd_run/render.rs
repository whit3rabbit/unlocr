use std::path::Path;
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;
use unlocr::OcrOptions;

/// Rasterize a PDF to per-page PNGs for the preview pane, cached on disk by the
/// core lib so a repeat preview skips pdftoppm. `password` (session-only, never
/// persisted) unlocks an encrypted PDF; None for the common unencrypted case.
#[tauri::command]
pub(crate) async fn render_pages(
    pdf_path: String,
    dpi: Option<u32>,
    password: Option<String>,
) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<String>, String> {
        let dpi = dpi.unwrap_or_else(|| OcrOptions::default().dpi);
        let cache = unlocr::model::cache_dir(None).map_err(|e| e.to_string())?;
        let pdftoppm = unlocr::preflight::pdftoppm().map_err(|e| e.to_string())?;
        let pages = unlocr::render_pages(
            &pdftoppm,
            Path::new(&pdf_path),
            dpi,
            &cache,
            password.as_deref(),
        )
        .map_err(|e| e.to_string())?;
        Ok(pages
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect())
    })
    .await
    .map_err(|e| format!("render worker join failed: {e}"))?
}

/// Render ONE page (1-based) of a PDF for the preview pane, returning its PNG path.
#[tauri::command]
pub(crate) async fn render_page(
    pdf_path: String,
    page: u32,
    dpi: Option<u32>,
    password: Option<String>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let dpi = dpi.unwrap_or_else(|| OcrOptions::default().dpi);
        let cache = unlocr::model::cache_dir(None).map_err(|e| e.to_string())?;
        let pdftoppm = unlocr::preflight::pdftoppm().map_err(|e| e.to_string())?;
        let path = unlocr::render_page(
            &pdftoppm,
            Path::new(&pdf_path),
            dpi,
            &cache,
            page,
            password.as_deref(),
        )
        .map_err(|e| e.to_string())?;
        Ok(path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("render worker join failed: {e}"))?
}

/// Metadata about the PDF currently shown in the preview pane (title, author,
/// page count, file size, ...), for the info-icon popup next to "PDF Preview".
/// `password` unlocks an encrypted PDF (pdfinfo fails outright without it on an
/// open-password PDF, so the fields are unreadable until it is supplied).
#[tauri::command]
pub(crate) async fn pdf_info(
    pdf_path: String,
    password: Option<String>,
) -> Result<unlocr::pdf::PdfInfo, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<unlocr::pdf::PdfInfo, String> {
        let pdftoppm = unlocr::preflight::pdftoppm().map_err(|e| e.to_string())?;
        unlocr::pdf::info(&pdftoppm, Path::new(&pdf_path), password.as_deref())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("pdf info worker join failed: {e}"))?
}

/// Does this PDF need a user/open password? Delegates to the core
/// `pdf::needs_user_password`, which opens with no password and only reports "needs
/// password" when poppler says so -- so a corrupt PDF is not misclassified into an
/// unsatisfiable prompt loop, and the no-sibling-pdfinfo fallback (bare-name
/// pdftoppm) is handled identically to the CLI run path. The frontend uses this to
/// decide whether to show the password prompt before running OCR.
#[tauri::command]
pub(crate) async fn pdf_needs_password(pdf_path: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<bool, String> {
        let pdftoppm = unlocr::preflight::pdftoppm().map_err(|e| e.to_string())?;
        Ok(unlocr::pdf::needs_user_password(
            &pdftoppm,
            Path::new(&pdf_path),
        ))
    })
    .await
    .map_err(|e| format!("pdf needs-password worker join failed: {e}"))?
}

/// Does `password` unlock `pdf_path`? Validates a candidate from the password
/// prompt before the run, so a wrong password re-prompts instead of failing mid-OCR.
/// Uses the same `pdf::can_open` probe as the run path, so it validates even when
/// pdftoppm resolves as a bare PATH name (no sibling pdfinfo to query).
#[tauri::command]
pub(crate) async fn check_pdf_password(pdf_path: String, password: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<bool, String> {
        let pdftoppm = unlocr::preflight::pdftoppm().map_err(|e| e.to_string())?;
        Ok(unlocr::pdf::can_open(
            &pdftoppm,
            Path::new(&pdf_path),
            Some(&password),
        ))
    })
    .await
    .map_err(|e| format!("pdf check-password worker join failed: {e}"))?
}

/// Open the native file picker on the BACKEND and read the chosen bulk password file
/// (one password per line; blank lines and `#`-prefixed lines skipped, mirroring the
/// CLI `--password-file`). Returns the candidate list, or `None` if the user
/// cancelled. Opening the dialog server-side means no renderer-supplied path is ever
/// read: the webview cannot widen the read scope to an arbitrary file (mirrors
/// `read_text_file`'s backend-derived allowlist). The list is held in the frontend
/// for the session and never persisted; contents are only used as `-upw` candidates.
#[tauri::command]
pub(crate) async fn pick_password_file(app: AppHandle) -> Result<Option<Vec<String>>, String> {
    let picked = app
        .dialog()
        .file()
        .add_filter("Text", &["txt"])
        .blocking_pick_file();
    let Some(fp) = picked else {
        return Ok(None);
    };
    let path = fp.into_path().map_err(|e| e.to_string())?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)
            .collect(),
    ))
}
