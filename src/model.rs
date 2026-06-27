// Resolve the model cache directory and ensure the quant + projector GGUFs
// are present, downloading from Hugging Face on first use.

use crate::Res;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const REPO: &str = "sahilchachra/Unlimited-OCR-GGUF";
const MMPROJ: &str = "mmproj-Unlimited-OCR-F16.gguf";

pub struct ModelFiles {
    pub model: PathBuf,
    pub mmproj: PathBuf,
}

/// `--model-dir` override, else the per-OS cache dir + `/unlocr`.
pub fn cache_dir(override_dir: Option<PathBuf>) -> Res<PathBuf> {
    let dir = match override_dir {
        Some(d) => d,
        None => base_cache_dir()?.join("unlocr"),
    };
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn base_cache_dir() -> Res<PathBuf> {
    if let Ok(x) = std::env::var("XDG_CACHE_HOME") {
        if !x.is_empty() {
            return Ok(PathBuf::from(x));
        }
    }
    if cfg!(target_os = "macos") {
        let home = std::env::var("HOME")?;
        Ok(PathBuf::from(home).join("Library/Caches"))
    } else if cfg!(target_os = "windows") {
        let local = std::env::var("LOCALAPPDATA")?;
        Ok(PathBuf::from(local))
    } else {
        let home = std::env::var("HOME")?;
        Ok(PathBuf::from(home).join(".cache"))
    }
}

/// GGUF filename for a quant tag, e.g. "Q8_0" -> "Unlimited-OCR-Q8_0.gguf".
fn model_filename(quant: &str) -> String {
    format!("Unlimited-OCR-{quant}.gguf")
}

pub fn ensure(cache: &Path, quant: &str) -> Res<ModelFiles> {
    let model_name = model_filename(quant);
    let model = cache.join(&model_name);
    let mmproj = cache.join(MMPROJ);

    ensure_file(&model, &model_name)?;
    ensure_file(&mmproj, MMPROJ)?;
    Ok(ModelFiles { model, mmproj })
}

fn ensure_file(path: &Path, name: &str) -> Res<()> {
    if path.is_file() {
        return Ok(());
    }
    let url = format!("https://huggingface.co/{REPO}/resolve/main/{name}");
    println!("downloading {name} ...");
    download(&url, path)?;
    Ok(())
}

/// Stream a URL to `<dest>.part`, then atomically rename. Prints rough progress.
fn download(url: &str, dest: &Path) -> Res<()> {
    let resp = ureq::get(url).call()?;
    let total: u64 = resp
        .header("Content-Length")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let part = dest.with_extension("part");
    let mut out = fs::File::create(&part)?;
    let mut reader = resp.into_reader();

    let mut buf = vec![0u8; 1 << 20]; // 1 MiB
    let mut done: u64 = 0;
    let mut last_pct = u64::MAX;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        if total > 0 {
            let pct = done * 100 / total;
            if pct != last_pct {
                print!("\r  {pct:>3}%  ({} / {} MiB)", done >> 20, total >> 20);
                let _ = std::io::stdout().flush();
                last_pct = pct;
            }
        }
    }
    if total > 0 {
        println!();
    }
    out.sync_all()?;
    fs::rename(&part, dest)?;
    Ok(())
}

pub fn check_presence(cache: &Path, quant: &str) -> (PathBuf, bool, PathBuf, bool) {
    let model = cache.join(model_filename(quant));
    let mmproj = cache.join(MMPROJ);
    (model.clone(), model.is_file(), mmproj.clone(), mmproj.is_file())
}

#[cfg(test)]
mod tests {
    use super::{cache_dir, model_filename};

    #[test]
    fn filename_format() {
        assert_eq!(model_filename("Q8_0"), "Unlimited-OCR-Q8_0.gguf");
        assert_eq!(model_filename("BF16"), "Unlimited-OCR-BF16.gguf");
    }

    #[test]
    fn cache_dir_override_used_and_created() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("nested/cache");
        let got = cache_dir(Some(target.clone())).unwrap();
        assert_eq!(got, target);
        assert!(target.is_dir()); // create_dir_all ran
    }
}
