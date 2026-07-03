use crate::Res;
use std::path::{Path, PathBuf};

/// Returns true if the file extension is case-insensitively "pdf".
pub fn is_pdf(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Case-insensitive whitelist of image extensions the OCR pipeline accepts
/// directly (no rasterize step). Single source of truth for the CLI glob
/// filter, directory scan, and GUI file-picker filters.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "tiff", "tif", "webp", "bmp"];

/// Returns true if the file extension is case-insensitively one of
/// `IMAGE_EXTENSIONS`. Extension-only check (fast path for filtering/globbing);
/// `sniff_image_mime` is the content-based check run once the file is actually
/// read, right before it is handed to the model.
pub fn is_image(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        IMAGE_EXTENSIONS
            .iter()
            .any(|ext| e.eq_ignore_ascii_case(ext))
    })
}

/// Sniff `bytes` for a known image magic number and return the MIME type to
/// use in a `data:` URI. Stdlib-only (no `image` crate): this is pure
/// byte-prefix matching, not full image decoding. Returns `None` for anything
/// unrecognized -- callers treat that as "reject", not "guess".
pub fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(&[0x49, 0x49, 0x2A, 0x00])
        || bytes.starts_with(&[0x4D, 0x4D, 0x00, 0x2A])
    {
        Some("image/tiff")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(&[0x42, 0x4D]) {
        Some("image/bmp")
    } else {
        None
    }
}

/// Generalized directory walker, parameterized over the match predicate and an
/// optional recursion depth cap. `collect_pdfs`/`collect_pdfs_and_images` are
/// thin wrappers over this so there is one symlink-cycle guard and one
/// recursion policy, not multiple copies that can drift apart.
///
/// `max_depth`: `None` = unbounded (today's CLI `--recursive` behavior,
/// preserved exactly for `collect_pdfs`). `Some(0)` = this dir only, no
/// recursion regardless of `recursive`. `Some(n)` = recurse at most `n` levels
/// below `dir` (depth 0 is `dir` itself, so `Some(1)` still descends one level
/// into subdirectories of `dir`).
pub fn collect_matching(
    dir: &Path,
    recursive: bool,
    max_depth: Option<usize>,
    matches: &impl Fn(&Path) -> bool,
    out: &mut Vec<PathBuf>,
) -> Res<()> {
    collect_matching_at(dir, recursive, max_depth, 0, matches, out)
}

fn collect_matching_at(
    dir: &Path,
    recursive: bool,
    max_depth: Option<usize>,
    depth: usize,
    matches: &impl Fn(&Path) -> bool,
    out: &mut Vec<PathBuf>,
) -> Res<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        // file_type() from the dir entry does NOT follow symlinks (unlike
        // Path::is_dir). Skip only symlinked *directories* so a cycle (e.g. a/ -> ..)
        // cannot recurse into a stack overflow, which `panic = "abort"` turns into a
        // hard abort. A symlinked file is legitimate and must NOT be dropped, so
        // it falls through to the `matches` branch. ponytail: skips symlinked dirs
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
            let can_descend = recursive && max_depth.is_none_or(|max| depth < max);
            if can_descend {
                collect_matching_at(&path, recursive, max_depth, depth + 1, matches, out)?;
            }
        } else if matches(&path) {
            out.push(path);
        }
    }
    Ok(())
}

/// Collect *.pdf under `dir`, one level deep or recursively, depth-unbounded.
/// Thin wrapper over `collect_matching` (`max_depth: None`) so existing
/// callers' behavior is unchanged.
pub fn collect_pdfs(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) -> Res<()> {
    collect_matching(dir, recursive, None, &is_pdf, out)
}

/// Collect PDFs AND recognized image files under `dir`. Additive superset of
/// `collect_pdfs`: a folder scan that previously found only `.pdf` now also
/// finds images, matching the pipeline's ability to OCR a single image
/// directly. `max_depth` lets a caller (e.g. a GUI-triggered scan) cap runaway
/// recursion explicitly.
pub fn collect_pdfs_and_images(
    dir: &Path,
    recursive: bool,
    max_depth: Option<usize>,
    out: &mut Vec<PathBuf>,
) -> Res<()> {
    collect_matching(
        dir,
        recursive,
        max_depth,
        &|p| is_pdf(p) || is_image(p),
        out,
    )
}

/// Expand positional inputs (files, folders, glob patterns) plus an optional
/// --from-list file into a concrete, sorted, deduped list of paths. Matches
/// PDFs AND recognized images: a batch can mix PDF and image inputs under one
/// invocation.
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
            collect_pdfs_and_images(input, recursive, None, &mut out)?;
        } else if let Some(pat) = input.to_str().filter(|s| s.contains(['*', '?', '['])) {
            // Glob only when the path isn't a literal that exists. The shell
            // usually expands these already; this covers quoted globs and
            // PowerShell, which does not.
            if input.exists() {
                out.push(input.clone());
            } else {
                for m in glob::glob(pat).map_err(|e| format!("bad glob {pat}: {e}"))? {
                    let p = m?;
                    if is_pdf(&p) || is_image(&p) {
                        out.push(p);
                    }
                }
            }
        } else {
            out.push(input.clone()); // literal; run_ocr_job validates existence + content
        }
    }

    out.sort();
    out.dedup();
    if out.is_empty() {
        return Err("No input PDFs or images found. Run: unlocr --help for usage.".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_image_recognizes_whitelisted_extensions() {
        for ext in IMAGE_EXTENSIONS {
            assert!(
                is_image(Path::new(&format!("file.{ext}"))),
                "{ext} should be recognized"
            );
            let upper = ext.to_uppercase();
            assert!(
                is_image(Path::new(&format!("file.{upper}"))),
                "{upper} should be recognized"
            );
        }
        assert!(!is_image(Path::new("file.pdf")));
        assert!(!is_image(Path::new("file.txt")));
        assert!(!is_image(Path::new("file.gif")));
        assert!(!is_image(Path::new("file")));
    }

    #[test]
    fn sniff_image_mime_recognizes_each_format() {
        assert_eq!(
            sniff_image_mime(&[0x89, 0x50, 0x4E, 0x47, 0, 0]),
            Some("image/png")
        );
        assert_eq!(
            sniff_image_mime(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some("image/jpeg")
        );
        assert_eq!(
            sniff_image_mime(&[0x49, 0x49, 0x2A, 0x00]),
            Some("image/tiff")
        );
        assert_eq!(
            sniff_image_mime(&[0x4D, 0x4D, 0x00, 0x2A]),
            Some("image/tiff")
        );
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(sniff_image_mime(&webp), Some("image/webp"));
        assert_eq!(sniff_image_mime(&[0x42, 0x4D]), Some("image/bmp"));
        assert_eq!(sniff_image_mime(b"not an image"), None);
        assert_eq!(sniff_image_mime(&[]), None);
    }

    #[test]
    fn collect_matching_respects_max_depth() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("depth0.pdf"), b"").unwrap();
        let d1 = root.join("sub1");
        std::fs::create_dir(&d1).unwrap();
        std::fs::write(d1.join("depth1.pdf"), b"").unwrap();
        let d2 = d1.join("sub2");
        std::fs::create_dir(&d2).unwrap();
        std::fs::write(d2.join("depth2.pdf"), b"").unwrap();

        let mut out = Vec::new();
        collect_matching(root, true, Some(0), &is_pdf, &mut out).unwrap();
        assert_eq!(out.len(), 1);

        let mut out = Vec::new();
        collect_matching(root, true, Some(1), &is_pdf, &mut out).unwrap();
        assert_eq!(out.len(), 2);

        let mut out = Vec::new();
        collect_matching(root, true, None, &is_pdf, &mut out).unwrap();
        assert_eq!(out.len(), 3);

        let mut out = Vec::new();
        collect_matching(root, false, None, &is_pdf, &mut out).unwrap();
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn collect_pdfs_and_images_finds_both_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("a.pdf"), b"").unwrap();
        std::fs::write(root.join("b.png"), b"").unwrap();
        std::fs::write(root.join("c.txt"), b"").unwrap();

        let mut both = Vec::new();
        collect_pdfs_and_images(root, false, None, &mut both).unwrap();
        assert_eq!(both.len(), 2);

        let mut pdf_only = Vec::new();
        collect_pdfs(root, false, &mut pdf_only).unwrap();
        assert_eq!(pdf_only.len(), 1);
    }

    #[test]
    fn expand_inputs_mixes_pdf_and_image_literals() {
        let tmp = tempfile::tempdir().unwrap();
        let pdf = tmp.path().join("a.pdf");
        let img = tmp.path().join("b.png");
        std::fs::write(&pdf, b"").unwrap();
        std::fs::write(&img, b"").unwrap();

        let out = expand_inputs(&[pdf.clone(), img.clone()], None, false).unwrap();
        assert_eq!(out, {
            let mut v = vec![pdf, img];
            v.sort();
            v
        });
    }
}
