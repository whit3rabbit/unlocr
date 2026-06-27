// Rasterize a PDF to one PNG per page using pdftoppm, returned in page order.

use crate::Res;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run `pdftoppm -png -r <dpi> <pdf> <outdir>/page` and collect the PNGs sorted
/// by page number. pdftoppm zero-pads the suffix based on page count, so we
/// sort by the parsed trailing integer rather than lexically.
pub fn rasterize(pdftoppm: &Path, pdf: &Path, outdir: &Path, dpi: u32) -> Res<Vec<PathBuf>> {
    let prefix = outdir.join("page");
    let status = Command::new(pdftoppm)
        .arg("-png")
        .arg("-r").arg(dpi.to_string())
        .arg(pdf)
        .arg(&prefix)
        .status()?;
    if !status.success() {
        return Err(format!("pdftoppm failed ({status})").into());
    }

    let mut pages: Vec<(u64, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(outdir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("png") {
            if let Some(n) = trailing_number(&path) {
                pages.push((n, path));
            }
        }
    }
    pages.sort_by_key(|(n, _)| *n);
    if pages.is_empty() {
        return Err("pdftoppm produced no pages".into());
    }
    Ok(pages.into_iter().map(|(_, p)| p).collect())
}

/// Pull the trailing integer out of a stem like "page-12" -> 12.
fn trailing_number(path: &Path) -> Option<u64> {
    let stem = path.file_stem()?.to_str()?;
    let digits: String = stem.chars().rev().take_while(|c| c.is_ascii_digit()).collect();
    digits.chars().rev().collect::<String>().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::trailing_number;
    use std::path::Path;

    #[test]
    fn extracts_page_number() {
        assert_eq!(trailing_number(Path::new("/tmp/page-1.png")), Some(1));
        assert_eq!(trailing_number(Path::new("/tmp/page-12.png")), Some(12));
        assert_eq!(trailing_number(Path::new("/tmp/page-007.png")), Some(7));
        assert_eq!(trailing_number(Path::new("/tmp/nope.png")), None);
    }
}
