use crate::options::{OcrOutput, OutputMode};
use crate::Res;
use std::path::{Path, PathBuf};

/// Resolve the output `.md` path for one input. Shared by the CLI (`ocr::run_pdf`)
/// and the GUI (`run_ocr`) so both agree on where a result is written.
///
/// - `out_dir`: the chosen output folder. A relative `out_file` is joined under it;
///   the default `{stem}.md` is written into it.
/// - `out_file`: optional explicit filename/path (single-input only). An absolute
///   path is used verbatim (ignoring `out_dir`). When it has no extension, `.md`
///   is appended; a non-`.md` extension is left exactly as typed.
/// - `stem`: input file stem, used for the default `{stem}.md` when `out_file` is None.
pub fn resolve_output_path(out_dir: &Path, out_file: Option<&Path>, stem: &str) -> PathBuf {
    match out_file {
        None => out_dir.join(format!("{stem}.md")),
        Some(p) => {
            // Append .md only when no extension is present; respect a typed extension.
            let p = if p.extension().is_none() {
                p.with_extension("md")
            } else {
                p.to_path_buf()
            };
            if p.is_absolute() {
                p
            } else {
                out_dir.join(p)
            }
        }
    }
}

/// Write assembled OCR output to disk per `mode`, returning every path written
/// (combined file first in `Both`). Shared by the CLI (`ocr::run_pdf`) and the
/// GUI (`run_ocr`) so both front ends agree on layout. The caller owns any
/// read-allowlist (the GUI inserts these into `AppState.read_allow`); this fn
/// only writes files + their parent dirs.
///
/// - `Single`: one `{stem}.md` (or `out_file` if given) holding `output.combined`.
/// - `Pages`: a `{out_dir}/{stem}/page-N.md` folder, one file per page. `out_file`
///   is ignored for the folder name (the caller warns when it was set). Page
///   numbers are zero-padded to the width of the largest page number so files
///   sort lexicographically (page-01 before page-10).
/// - `Both`: the combined file plus the per-page folder.
pub fn write_markdown_output(
    mode: OutputMode,
    out_dir: &Path,
    out_file: Option<&Path>,
    stem: &str,
    output: &OcrOutput,
) -> Res<Vec<PathBuf>> {
    let mut written: Vec<PathBuf> = Vec::new();

    if matches!(mode, OutputMode::Single | OutputMode::Both) {
        let combined_path = resolve_output_path(out_dir, out_file, stem);
        if let Some(parent) = combined_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&combined_path, &output.combined)?;
        written.push(combined_path);
    }

    if matches!(mode, OutputMode::Pages | OutputMode::Both) {
        // Guard against path traversal: a file stem of ".." (file named "...pdf")
        // or "." ("..pdf") would resolve out_dir.join(stem) to an unintended parent
        // or same directory. Reject both before any I/O. See also FINDING-001/FINDING-002.
        if stem == "." || stem == ".." {
            return Err(
                format!("invalid output stem '{stem}': stems must not be '.' or '..'").into(),
            );
        }
        let folder = out_dir.join(stem);
        std::fs::create_dir_all(&folder)?;
        // Clear stale `page-*.md` from a prior run into the SAME folder before
        // writing. Without this, OCR'ing a shorter document (or a different
        // same-stem input) over an earlier run leaves the earlier run's
        // higher-numbered pages behind, silently mixing two documents' pages.
        if let Ok(rd) = std::fs::read_dir(&folder) {
            for e in rd.flatten() {
                let p = e.path();
                let is_page_md = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("page-") && n.ends_with(".md"))
                    .unwrap_or(false);
                if is_page_md {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
        // Zero-pad to the largest page number's width (min 2) so a listing sorts
        // page-01 before page-10. Width defaults to 2 when there are no pages
        // (defensive: ocr_pages errors on zero pages before we get here).
        let width = output
            .pages
            .last()
            .map(|(n, _)| n.to_string().len())
            .unwrap_or(2)
            .max(2);
        for (page_num, text) in &output.pages {
            let path = folder.join(format!("page-{page_num:0width$}.md"));
            std::fs::write(&path, text)?;
            written.push(path);
        }
    }

    Ok(written)
}

/// Stems shared by more than one input (sorted, deduped). Same-stem inputs from
/// different folders collide on a shared out dir, the `{stem}.md` file (single)
/// or the `{stem}/` pages folder (pages/both), so a later input silently
/// overwrites an earlier one. A batch caller warns on these before running.
pub fn duplicate_stems(inputs: &[PathBuf]) -> Vec<String> {
    use std::collections::HashMap;
    let mut counts: HashMap<String, usize> = HashMap::new();
    for input in inputs {
        let stem = input
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output")
            .to_string();
        *counts.entry(stem).or_insert(0) += 1;
    }
    let mut dups: Vec<String> = counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(s, _)| s)
        .collect();
    dups.sort();
    dups
}

/// Append one page's text with a `<!-- page N -->` delimiter (1-based).
/// Canonical implementation: ocr_pages (lib) and the CLI path (via run_pdf's
/// delegation) both route through this, so page-delimited markdown is identical
/// across the CLI and GUI callers (covered by the lib test below).
pub fn push_page(md: &mut String, idx: usize, text: &str) {
    md.push_str(&format!("\n\n<!-- page {} -->\n\n", idx + 1));
    md.push_str(text);
}
