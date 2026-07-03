use std::path::{Path, PathBuf};
use unlocr::inputs::{is_image, is_pdf};

/// Recursion depth cap for a GUI-triggered folder scan. Generous for any real
/// document tree, but bounds a single OK click against an adversarial/mistaken
/// folder pick (e.g. a filesystem root) -- unlike the CLI's `--recursive`,
/// which is an explicit opt-in by a user who typed the flag, this is one
/// click in a picker that could aim at anything.
const SCAN_MAX_DEPTH: usize = 12;

/// Walk the staged folders (recursively, up to `SCAN_MAX_DEPTH`, when
/// `recursive` is true) matching PDFs and recognized images, passing any
/// staged plain files straight through untouched. Returns the flat, sorted,
/// deduped path list. Delegates to `unlocr::inputs::collect_pdfs_and_images`,
/// the same matcher the CLI's directory expansion uses.
#[tauri::command]
pub(crate) async fn scan_input_paths(
    paths: Vec<String>,
    recursive: bool,
) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<String>, String> {
        let mut out: Vec<PathBuf> = Vec::new();
        for p in paths {
            let path = Path::new(&p);
            if path.is_dir() {
                unlocr::inputs::collect_pdfs_and_images(
                    path,
                    recursive,
                    Some(SCAN_MAX_DEPTH),
                    &mut out,
                )
                .map_err(|e| format!("scanning {}: {e}", path.display()))?;
            } else if path.is_file() {
                // Same PDF/image filter folder-scanned entries go through
                // (collect_pdfs_and_images): a native file-picker's extension
                // filter is advisory, not enforced, so an individually staged
                // file can still be neither. Skip it here rather than queueing
                // it to fail later at OCR time.
                if is_pdf(path) || is_image(path) {
                    out.push(path.to_path_buf());
                }
            }
            // Neither a dir nor an existing file (e.g. a stale picker result
            // that vanished between selection and OK): skip silently rather
            // than fail the whole batch for one missing entry.
        }
        out.sort();
        out.dedup();
        Ok(out
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect())
    })
    .await
    .map_err(|e| format!("scan worker join failed: {e}"))?
}

#[cfg(test)]
mod tests {
    /// `gui/src/formats.js` hand-maintains a JS copy of `unlocr::inputs::IMAGE_EXTENSIONS`
    /// for the file-picker filter (comment there says "keep both in sync"). Nothing else
    /// enforces that, so this parses the JS array literal and diffs it against the Rust
    /// constant -- fails loudly the next time one list is edited and the other isn't.
    #[test]
    fn image_extensions_match_gui_formats_js() {
        let js_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../src/formats.js");
        let js = std::fs::read_to_string(js_path).unwrap_or_else(|e| panic!("read {js_path}: {e}"));
        let start = js
            .find("IMAGE_EXTENSIONS = [")
            .expect("IMAGE_EXTENSIONS literal not found in formats.js");
        let rest = &js[start..];
        let open = rest.find('[').unwrap();
        let close = rest
            .find(']')
            .expect("unterminated IMAGE_EXTENSIONS array in formats.js");
        let mut js_exts: Vec<&str> = rest[open + 1..close]
            .split(',')
            .map(|s| s.trim().trim_matches('"'))
            .filter(|s| !s.is_empty())
            .collect();
        js_exts.sort_unstable();

        let mut rust_exts: Vec<&str> = unlocr::inputs::IMAGE_EXTENSIONS.to_vec();
        rust_exts.sort_unstable();

        assert_eq!(
            js_exts, rust_exts,
            "gui/src/formats.js IMAGE_EXTENSIONS drifted from unlocr::inputs::IMAGE_EXTENSIONS"
        );
    }
}
