//! Shared helpers for the JSON-under-cache stores (`store.rs`, `notifications.rs`,
//! `settings.rs`). All three persist a small JSON blob to the model cache dir with
//! the same atomic write, and the two list stores cap their history the same way.
//! Kept in one place so a durability or retention fix lands once, not three times.

use std::path::Path;
use std::sync::Mutex;

/// One process-wide lock for ALL json-store read-modify-write sequences. `write_atomic`
/// stops a *torn* file, but not a *lost update*: two writers that each load N records,
/// mutate, and save N+1 would clobber one. Serializing the whole load-mutate-save
/// through this lock closes that window. ponytail: a single coarse lock across all
/// three stores; fine because writes are rare (one per OCR run / settings save /
/// notification) and the blobs are tiny. Switch to per-file locks only if write
/// throughput ever matters.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Run `f` while holding the global store write lock. Wrap each store's full
/// load-mutate-save in this. Recovers from a poisoned lock (a panic in a prior
/// critical section) because the state lives on disk, not behind the mutex.
pub(crate) fn with_write_lock<R>(f: impl FnOnce() -> R) -> R {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    f()
}

/// Atomically replace `path` with `bytes`: write a sibling `.json.tmp` then rename
/// over the target (rename is atomic on the same filesystem). A partial write can
/// never leave a half-written store the next read would choke on.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| format!("could not finalize store at {}: {e}", path.display()))
}

/// Drop all but the most recent `max` items, preserving insertion order (the tail is
/// newest). Pure (no IO) so callers can unit-test their cap without a real cache dir.
pub(crate) fn cap_to_recent<T>(mut items: Vec<T>, max: usize) -> Vec<T> {
    if items.len() > max {
        items.drain(0..items.len() - max);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::cap_to_recent;

    #[test]
    fn cap_to_recent_keeps_newest_tail() {
        // Under the cap: unchanged.
        let few: Vec<u64> = (0..5).collect();
        assert_eq!(cap_to_recent(few, 10), (0..5).collect::<Vec<_>>());

        // Over the cap: trimmed to `max`, oldest dropped, newest kept (tail).
        let many: Vec<u64> = (0..30).collect();
        let capped = cap_to_recent(many, 10);
        assert_eq!(capped.len(), 10, "must trim to the cap");
        assert_eq!(*capped.first().unwrap(), 20, "oldest 20 dropped");
        assert_eq!(*capped.last().unwrap(), 29, "newest kept");
    }
}
