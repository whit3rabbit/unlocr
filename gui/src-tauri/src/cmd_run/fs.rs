use crate::state::AppState;
use crate::store;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, State};

/// Read a UTF-8 text file off disk into a String. Used by the frontend to fetch
/// the `{stem}.md` written by `run_ocr` (the path is returned from that command)
/// so the result can be rendered in a dedicated read-only markdown pane.
#[tauri::command]
pub(crate) fn read_text_file(path: String, state: State<'_, AppState>) -> Result<String, String> {
    let allowed = allowed_output_paths(&state);
    let canonical = check_readable(&path, &allowed)?;
    std::fs::read_to_string(&canonical)
        .map_err(|e| format!("failed to read {}: {e}", canonical.display()))
}

/// Overwrite a `.md` the review-pane editor is editing. Write scope is the SAME
/// backend-derived allowlist as `read_text_file`: the renderer may only overwrite a
/// file the app itself produced, never a path it chooses.
#[tauri::command]
pub(crate) fn write_text_file(
    path: String,
    content: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let allowed = allowed_output_paths(&state);
    let canonical = check_readable(&path, &allowed)?;
    std::fs::write(&canonical, content)
        .map_err(|e| format!("failed to write {}: {e}", canonical.display()))
}

/// Map a frontend export format to the pandoc writer name and output file extension.
fn pandoc_target(format: &str) -> Option<(&'static str, &'static str)> {
    match format {
        "html" => Some(("html", "html")),
        "txt" => Some(("plain", "txt")),
        "docx" => Some(("docx", "docx")),
        "odt" => Some(("odt", "odt")),
        "rtf" => Some(("rtf", "rtf")),
        _ => None,
    }
}

/// Export the loaded review-pane markdown to another document format via pandoc.
#[tauri::command]
pub(crate) async fn export_markdown(
    app: AppHandle,
    src_path: String,
    format: String,
) -> Result<String, String> {
    let (writer, ext) =
        pandoc_target(&format).ok_or_else(|| format!("unsupported export format: {format}"))?;
    let pandoc = unlocr::preflight::locate("pandoc").ok_or_else(|| {
        concat!(
            "pandoc not found on PATH; it is required to export. Install it:\n",
            "  macOS:          brew install pandoc\n",
            "  Debian/Ubuntu:  sudo apt install pandoc\n",
            "  Fedora:         sudo dnf install pandoc\n",
            "  Windows:        scoop install pandoc  (or: winget install JohnMacFarlane.Pandoc)"
        )
        .to_string()
    })?;
    tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let state = app.state::<AppState>();
        let allowed = allowed_output_paths(&state);
        let canonical = check_readable(&src_path, &allowed)?;
        let out = canonical.with_extension(ext);
        let already_exported = state
            .exported_paths
            .lock()
            .map(|g| g.contains(&out))
            .unwrap_or(false);
        if out.exists() && !already_exported {
            return Err(format!(
                "export target already exists and was not produced by unlocr: {}. \
                 Remove it first or choose another format.",
                out.display()
            ));
        }
        let output = std::process::Command::new(&pandoc)
            .arg(&canonical)
            .args(["-f", "markdown", "-t", writer, "-s", "-o"])
            .arg(&out)
            .output()
            .map_err(|e| format!("failed to run pandoc: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "pandoc failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        if let Ok(mut g) = state.exported_paths.lock() {
            g.insert(out.clone());
        }
        Ok(out.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("export worker join failed: {e}"))?
}

/// Canonicalized set of files `read_text_file` may serve.
pub(crate) fn allowed_output_paths(state: &AppState) -> HashSet<PathBuf> {
    let mut set: HashSet<PathBuf> = state
        .read_allow
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    if let Ok(cache) = state.job_output_cache.lock() {
        if let Some(paths) = cache.as_ref() {
            set.extend(paths.iter().cloned());
            return set;
        }
    }
    let mut built = HashSet::new();
    for p in crate::store::peek_job_outputs().unwrap_or_default() {
        if let Ok(c) = std::fs::canonicalize(&p) {
            built.insert(c);
        }
    }
    if let Ok(mut cache) = state.job_output_cache.lock() {
        *cache = Some(built.clone());
    }
    set.extend(built);
    set
}

/// Pure-of-Tauri core of `read_text_file`: validate `path` and return the canonical target.
pub(crate) fn check_readable(path: &str, allowed: &HashSet<PathBuf>) -> Result<PathBuf, String> {
    if !store::is_md(Path::new(path)) {
        return Err(format!("refusing to read non-markdown path: {path}"));
    }
    let canonical =
        std::fs::canonicalize(path).map_err(|e| format!("cannot resolve path {path}: {e}"))?;
    if !store::is_md(&canonical) {
        return Err(format!(
            "refusing to read non-markdown path after resolution: {}",
            canonical.display()
        ));
    }
    if !allowed.contains(&canonical) {
        return Err(format!(
            "refusing to read a file the app did not produce: {}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::check_readable;
    use std::collections::HashSet;

    /// A path in the allowed set is served; an existing-but-unlisted .md is rejected
    /// (the core of the renderer-cannot-widen-scope guarantee).
    #[test]
    fn check_readable_exact_match_only() {
        let tmp = tempfile::tempdir().unwrap();
        let allow = tmp.path().join("allowed.md");
        let other = tmp.path().join("other.md");
        std::fs::write(&allow, b"# ok").unwrap();
        std::fs::write(&other, b"# secret").unwrap();

        // canonicalize so the set matches what check_readable computes internally.
        let mut set = HashSet::new();
        set.insert(std::fs::canonicalize(&allow).unwrap());

        let got = check_readable(allow.to_str().unwrap(), &set).unwrap();
        assert_eq!(got, std::fs::canonicalize(&allow).unwrap());

        // An existing .md that the app did not produce is refused.
        let err = check_readable(other.to_str().unwrap(), &set).unwrap_err();
        assert!(err.contains("did not produce"), "unexpected error: {err}");
    }

    /// The write path shares `check_readable`'s guard: an allowed `.md` overwrites,
    /// an existing-but-unlisted `.md` is refused (cannot widen scope to write).
    #[test]
    fn write_guard_overwrites_only_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let allow = tmp.path().join("allowed.md");
        let other = tmp.path().join("other.md");
        std::fs::write(&allow, b"# old").unwrap();
        std::fs::write(&other, b"# secret").unwrap();

        let mut set = HashSet::new();
        set.insert(std::fs::canonicalize(&allow).unwrap());

        // Allowed: resolve then overwrite.
        let canonical = check_readable(allow.to_str().unwrap(), &set).unwrap();
        std::fs::write(&canonical, b"# new").unwrap();
        assert_eq!(std::fs::read_to_string(&allow).unwrap(), "# new");

        // Unlisted: refused before any write; file is untouched.
        let err = check_readable(other.to_str().unwrap(), &set).unwrap_err();
        assert!(err.contains("did not produce"), "unexpected error: {err}");
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "# secret");
    }

    /// Export format mapping: known formats resolve to (writer, ext); unknown is None
    /// (rejected before pandoc runs, so the renderer can't pass an arbitrary writer).
    #[test]
    fn pandoc_target_maps_known_formats_only() {
        use super::pandoc_target;
        assert_eq!(pandoc_target("html"), Some(("html", "html")));
        assert_eq!(pandoc_target("txt"), Some(("plain", "txt")));
        assert_eq!(pandoc_target("docx"), Some(("docx", "docx")));
        assert_eq!(pandoc_target("odt"), Some(("odt", "odt")));
        assert_eq!(pandoc_target("rtf"), Some(("rtf", "rtf")));
        assert_eq!(pandoc_target("pdf"), None);
        assert_eq!(pandoc_target(""), None);
    }

    /// Non-.md paths are rejected before any filesystem access.
    #[test]
    fn check_readable_rejects_non_markdown() {
        let set = HashSet::new();
        let err = check_readable("/etc/passwd", &set).unwrap_err();
        assert!(err.contains("non-markdown"), "unexpected error: {err}");
    }
}
