use std::path::Path;
use unlocr::OcrOptions;

/// Rasterize a PDF to per-page PNGs for the preview pane, cached on disk by the
/// core lib so a repeat preview skips pdftoppm.
#[tauri::command]
pub(crate) async fn render_pages(
    pdf_path: String,
    dpi: Option<u32>,
) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<String>, String> {
        let dpi = dpi.unwrap_or_else(|| OcrOptions::default().dpi);
        let cache = unlocr::model::cache_dir(None).map_err(|e| e.to_string())?;
        let pdftoppm = unlocr::preflight::pdftoppm().map_err(|e| e.to_string())?;
        let pages = unlocr::render_pages(&pdftoppm, Path::new(&pdf_path), dpi, &cache)
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
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let dpi = dpi.unwrap_or_else(|| OcrOptions::default().dpi);
        let cache = unlocr::model::cache_dir(None).map_err(|e| e.to_string())?;
        let pdftoppm = unlocr::preflight::pdftoppm().map_err(|e| e.to_string())?;
        let path = unlocr::render_page(&pdftoppm, Path::new(&pdf_path), dpi, &cache, page)
            .map_err(|e| e.to_string())?;
        Ok(path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("render worker join failed: {e}"))?
}

/// Metadata about the PDF currently shown in the preview pane (title, author,
/// page count, file size, ...), for the info-icon popup next to "PDF Preview".
#[tauri::command]
pub(crate) async fn pdf_info(pdf_path: String) -> Result<unlocr::pdf::PdfInfo, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<unlocr::pdf::PdfInfo, String> {
        let pdftoppm = unlocr::preflight::pdftoppm().map_err(|e| e.to_string())?;
        unlocr::pdf::info(&pdftoppm, Path::new(&pdf_path)).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("pdf info worker join failed: {e}"))?
}
