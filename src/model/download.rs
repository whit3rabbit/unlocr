use super::{
    check_digest, file_sha256, model_filename, validate_quant, DigestCheck, ModelFiles, MMPROJ,
    REPO, REV,
};
use crate::Res;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

/// Ensures the model files for the specified quant are cached locally, downloading them if necessary.
/// Writes progress messages directly to stdout.
pub fn ensure(cache: &Path, quant: &str) -> Res<ModelFiles> {
    let mut cli = |name: &str, pct: Option<u8>, total: u64, done: u64| match pct {
        None => println!("downloading {name} ..."),
        Some(pct) => {
            print!("\r  {pct:>3}%  ({} / {} MiB)", done >> 20, total >> 20);
            let _ = std::io::stdout().flush();
        }
    };
    ensure_inner(cache, quant, None, None, &mut cli)
}

/// Ensures the model files for the specified quant are cached locally, invoking a progress callback.
pub fn ensure_with_progress<P>(cache: &Path, quant: &str, on_progress: &mut P) -> Res<ModelFiles>
where
    P: FnMut(crate::Progress),
{
    let mut sink = |name: &str, pct: Option<u8>, total: u64, done: u64| {
        let pct = pct.unwrap_or(0);
        on_progress(crate::Progress::Download {
            name: name.to_string(),
            pct,
            done,
            total,
        });
    };
    ensure_inner(cache, quant, None, None, &mut sink)
}

/// Ensures model files are present, allowing local overrides and emitting download progress.
pub fn ensure_with_overrides<P>(
    cache: &Path,
    quant: &str,
    model_override: Option<&Path>,
    mmproj_override: Option<&Path>,
    on_progress: &mut P,
) -> Res<ModelFiles>
where
    P: FnMut(crate::Progress),
{
    let mut sink = |name: &str, pct: Option<u8>, total: u64, done: u64| {
        let pct = pct.unwrap_or(0);
        on_progress(crate::Progress::Download {
            name: name.to_string(),
            pct,
            done,
            total,
        });
    };
    ensure_inner(cache, quant, model_override, mmproj_override, &mut sink)
}

fn require_file(path: &Path, kind: &str) -> Res<()> {
    if !path.is_file() {
        return Err(format!("{kind} file not found: {}", path.display()).into());
    }
    Ok(())
}

fn ensure_inner<F>(
    cache: &Path,
    quant: &str,
    model_override: Option<&Path>,
    mmproj_override: Option<&Path>,
    progress: &mut F,
) -> Res<ModelFiles>
where
    F: FnMut(&str, Option<u8>, u64, u64),
{
    let model = match model_override {
        Some(p) => {
            require_file(p, "model")?;
            p.to_path_buf()
        }
        None => {
            validate_quant(quant)?;
            let model_name = model_filename(quant);
            let model = cache.join(&model_name);
            ensure_file(&model, &model_name, progress)?;
            model
        }
    };

    let mmproj = match mmproj_override {
        Some(p) => {
            require_file(p, "mmproj")?;
            p.to_path_buf()
        }
        None => {
            let mmproj = cache.join(MMPROJ);
            ensure_file(&mmproj, MMPROJ, progress)?;
            mmproj
        }
    };

    Ok(ModelFiles { model, mmproj })
}

fn ensure_file<F>(path: &Path, name: &str, progress: &mut F) -> Res<()>
where
    F: FnMut(&str, Option<u8>, u64, u64),
{
    if path.is_file() {
        return Ok(());
    }
    let url = format!("https://huggingface.co/{REPO}/resolve/{REV}/{name}");
    download(&url, path, name, progress)?;
    Ok(())
}

fn download<F>(url: &str, dest: &Path, name: &str, progress: &mut F) -> Res<()>
where
    F: FnMut(&str, Option<u8>, u64, u64),
{
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(120))
        .build();

    let part = dest.with_extension("part");
    let have: u64 = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);

    let mut req = agent.get(url);
    if have > 0 {
        req = req.set("Range", &format!("bytes={have}-"));
    }

    let resp = match req.call() {
        Ok(r) => r,
        Err(ureq::Error::Status(416, _)) => {
            let _ = fs::remove_file(&part);
            return download(url, dest, name, progress);
        }
        Err(e) => return Err(e.into()),
    };

    let (start, total) = if have > 0 && resp.status() == 206 {
        let remaining = header_u64(&resp, "Content-Length").unwrap_or(0);
        let total = content_range_total(&resp).unwrap_or(have + remaining);
        (have, total)
    } else {
        if have > 0 {
            let _ = fs::remove_file(&part);
        }
        (0, header_u64(&resp, "Content-Length").unwrap_or(0))
    };

    let reader = resp.into_reader();

    match stream_to_part(&part, name, total, start, reader, progress) {
        Ok(()) => {}
        Err(e) => {
            let _ = fs::remove_file(&part);
            return Err(e);
        }
    }

    match check_digest(name, &file_sha256(&part)?) {
        DigestCheck::Match => {}
        DigestCheck::Unpinned => {
            eprintln!(
                "warning: {name} downloaded without an integrity check \
                 (no pinned digest for this quant); the revision pin still applies"
            );
        }
        DigestCheck::Mismatch { expected } => {
            let _ = fs::remove_file(&part);
            return Err(format!(
                "integrity check failed for {name}: its sha256 does not match the pinned \
                 digest (expected {expected}). The download was rejected and deleted."
            )
            .into());
        }
    }

    fs::rename(&part, dest)?;
    Ok(())
}

fn header_u64(resp: &ureq::Response, name: &str) -> Option<u64> {
    resp.header(name).and_then(|s| s.trim().parse().ok())
}

fn content_range_total(resp: &ureq::Response) -> Option<u64> {
    resp.header("Content-Range")?
        .rsplit('/')
        .next()
        .and_then(|t| t.trim().parse().ok())
}

pub(crate) fn stream_to_part<F, R>(
    part: &Path,
    name: &str,
    total: u64,
    start: u64,
    mut reader: R,
    progress: &mut F,
) -> Res<()>
where
    F: FnMut(&str, Option<u8>, u64, u64),
    R: Read,
{
    let mut out = if start > 0 {
        fs::OpenOptions::new().append(true).open(part)?
    } else {
        fs::File::create(part)?
    };

    progress(name, None, total, start);

    let mut buf = vec![0u8; 1 << 20]; // 1 MiB
    let mut done: u64 = start;
    let mut last_pct = u64::MAX;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        if let Some(pct) = (done * 100).checked_div(total) {
            if pct != last_pct {
                progress(name, Some(pct as u8), total, done);
                last_pct = pct;
            }
        }
    }
    if total > 0 {
        println!();
        if done != total {
            return Err(format!(
                "truncated download of {name}: got {done} of {total} bytes (connection dropped?)"
            )
            .into());
        }
    } else {
        return Err(format!(
            "download of {name} reported no Content-Length; cannot verify it is complete \
             (got {done} bytes). Retry, or check for a proxy stripping the header."
        )
        .into());
    }
    out.sync_all()?;
    Ok(())
}
