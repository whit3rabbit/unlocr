use crate::Res;
use std::path::{Path, PathBuf};

/// Returns true if the file extension is case-insensitively "pdf".
pub fn is_pdf(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Collect *.pdf under `dir`, one level deep or recursively.
pub fn collect_pdfs(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) -> Res<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        // file_type() from the dir entry does NOT follow symlinks (unlike
        // Path::is_dir). Skip only symlinked *directories* so a cycle (e.g. a/ -> ..)
        // cannot recurse into a stack overflow, which `panic = "abort"` turns into a
        // hard abort. A symlinked PDF *file* is legitimate and must NOT be dropped, so
        // it falls through to the is_pdf branch. ponytail: skips symlinked dirs
        // entirely; switch to a visited canonical-path set if symlinked dir trees
        // must be followed.
        let ft = entry.file_type()?;
        let path = entry.path();
        // path.is_dir() follows the symlink; combined with is_symlink() it skips
        // only links that point at a directory.
        if ft.is_symlink() && path.is_dir() {
            continue;
        }
        if ft.is_dir() {
            if recursive {
                collect_pdfs(&path, recursive, out)?;
            }
        } else if is_pdf(&path) {
            out.push(path);
        }
    }
    Ok(())
}

/// Expand positional inputs (files, folders, glob patterns) plus an optional
/// --from-list file into a concrete, sorted, deduped list of PDF paths.
pub fn expand_inputs(
    raw: &[PathBuf],
    from_list: Option<&Path>,
    recursive: bool,
) -> Res<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = Vec::new();

    if let Some(list) = from_list {
        let text = std::fs::read_to_string(list)
            .map_err(|e| format!("--from-list {}: {e}", list.display()))?;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            out.push(PathBuf::from(line));
        }
    }

    for input in raw {
        if input.is_dir() {
            collect_pdfs(input, recursive, &mut out)?;
        } else if let Some(pat) = input.to_str().filter(|s| s.contains(['*', '?', '['])) {
            // Glob only when the path isn't a literal that exists. The shell
            // usually expands these already; this covers quoted globs and
            // PowerShell, which does not.
            if input.exists() {
                out.push(input.clone());
            } else {
                for m in glob::glob(pat).map_err(|e| format!("bad glob {pat}: {e}"))? {
                    let p = m?;
                    if is_pdf(&p) {
                        out.push(p);
                    }
                }
            }
        } else {
            out.push(input.clone()); // literal; ocr::run_pdf validates existence
        }
    }

    out.sort();
    out.dedup();
    if out.is_empty() {
        return Err("No input PDFs found. Run: unlocr --help for usage.".into());
    }
    Ok(out)
}
