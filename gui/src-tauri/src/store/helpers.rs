use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Current unix epoch seconds. Factored out so the `record_job` command, the
/// notifications store, and any status-update path share one clock. Falls back to
/// 0 if the system clock is before the epoch (essentially impossible; preserves
/// determinism).
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Derive a job id from the start time, the input file stem, a hash of the FULL
/// input path, and a process-wide monotonic seq. The path hash disambiguates two
/// same-stem inputs from different folders recorded in the same second (e.g. two
/// `report.pdf`); the seq guarantees two runs of the SAME file in the same second
/// get distinct ids, so the second run no longer upserts over the first (which
/// orphaned the first run's `.md`, since records now drive file deletion).
pub fn make_id(input_path: &str, created_at: u64) -> String {
    let stem = Path::new(input_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("job");
    // Sanitize the stem so it cannot contain a path separator or space that would
    // confuse any future log/file naming derived from the id. `is_ascii_alphanumeric`
    // (not `is_alphanumeric`) so non-ASCII stems (CJK/accented) also map to `_`,
    // honoring the "fs-safe" contract.
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut h = DefaultHasher::new();
    input_path.hash(&mut h);
    // Process-wide seq: every call is unique within a run. The 32-bit path hash
    // widens the old 16-bit disambiguator so cross-path collisions are negligible.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    // The pid disambiguates across processes: SEQ restarts at 0 each launch, so
    // without it a relaunch (or a 2nd instance) recording the same file in the
    // same second would re-derive an identical id and `INSERT OR REPLACE` would
    // clobber the earlier run's row (orphaning its `.md`).
    let pid = std::process::id();
    format!(
        "{created_at}-{clean}-{:08x}-{pid:x}-{seq}",
        h.finish() & 0xffff_ffff
    )
}

/// Returns true iff the path's extension is `md` (case-insensitive). Shared with
/// `cmd_run::check_readable` so read and delete apply the SAME `.md` rule (a `.MD`
/// output must be both readable and deletable, not one or the other).
pub(crate) fn is_md(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("md"))
        .unwrap_or(false)
}

/// Guarded file removal: the single sink for deleting a run's output. Only ever
/// deletes a `.md` file, mirroring the `.md`-only invariant `read_text_file`
/// enforces, and checks the extension on BOTH the raw and the canonicalized path
/// so a `.md` symlink cannot redirect the delete at a non-`.md` target. An empty
/// path or a missing file is a no-op success (idempotent).
///
/// ponytail: deletes the `.md` only; `keep_images` page-PNG dirs are NOT cleaned
/// up here. Add that path-pruning later if kept-image clutter becomes a problem.
pub fn delete_output_file(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Ok(());
    }
    let p = Path::new(path);
    if !is_md(p) {
        return Err(format!("refusing to delete non-.md path: {path}"));
    }
    if !p.exists() {
        return Ok(()); // already gone
    }
    // Resolve symlinks/.. before removal, then re-check it is a regular .md file.
    let canon = p
        .canonicalize()
        .map_err(|e| format!("could not resolve {path}: {e}"))?;
    if !is_md(&canon) {
        return Err(format!(
            "refusing to delete: {} resolves to a non-.md file {}",
            path,
            canon.display()
        ));
    }
    if !canon.is_file() {
        return Err(format!("not a regular file: {}", canon.display()));
    }
    std::fs::remove_file(&canon).map_err(|e| format!("could not delete {}: {e}", canon.display()))
}
