//! Persisted notification store.
//!
//! Backs the notification panel (the bell dropdown): terminal/notable events
//! (a PDF finished, a run failed, a model download completed) are recorded here
//! so they survive across app restarts and can be cleared individually or all at
//! once. Mirrors `store.rs` deliberately: a JSON file under the model cache dir,
//! atomic write, no extra dependency. SQLite would be overkill for an append +
//! read-all + delete list of at most a few hundred rows.
//!
//! Scope: persistence + typed accessors only; the toast UI and panel live in the
//! frontend. Transient progress (download percent/speed) is NOT stored here, only
//! terminal events worth surfacing after the fact. Purely additive module.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use unlocr::model::cache_dir;

/// Co-located with `jobs.json` and the GGUFs so one cache dir holds everything
/// the app persists.
const STORE_FILE: &str = "notifications.json";

/// Cap on retained notifications. The list is rewritten in full on every add and
/// the bell panel renders all of them, so an uncapped store grows unbounded. Keep
/// the most recent N (insertion-ordered, so the tail is newest).
const MAX_NOTIFICATIONS: usize = 200;

/// One stored notification. camelCase on the wire so the JS side reads
/// `n.createdAt` without a rename layer. `kind` is a coarse string
/// (`done` | `error` | `download` | `info`) rather than an enum so a future kind
/// never breaks an older frontend; the UI maps it to an icon/color.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    /// Stable id used by `clear_notification`. `<unix_secs>-<seq>` within the
    /// store, unique because `seq` is the max existing id's suffix + 1.
    pub id: String,
    /// done | error | download | info. Drives the panel icon/severity.
    pub kind: String,
    /// One-line headline, e.g. "report.pdf — OCR complete".
    pub title: String,
    /// Optional detail, e.g. the output path or an error message. May be empty.
    pub body: String,
    /// Unix epoch seconds the notification was recorded.
    pub created_at: u64,
    /// Whether the user has seen it. New notifications start unread so the bell
    /// can show an unread count; the frontend flips this when the panel opens.
    pub read: bool,
}

/// On-disk envelope. `version` lets a future migration detect an older schema.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct NotificationsFile {
    version: u32,
    notifications: Vec<Notification>,
}

impl Default for NotificationsFile {
    fn default() -> Self {
        Self {
            version: 1,
            notifications: Vec::new(),
        }
    }
}

/// Resolve the store path: `<model cache dir>/notifications.json`.
pub fn store_path() -> Result<PathBuf, String> {
    let cache = cache_dir(None).map_err(|e| format!("could not resolve model cache dir: {e}"))?;
    Ok(cache.join(STORE_FILE))
}

/// Load all notifications. A missing file means "none yet"; a corrupt file is
/// logged and treated as empty so a bad state never wedges the panel.
pub fn load() -> Vec<Notification> {
    let path = match store_path() {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<NotificationsFile>(&bytes) {
            Ok(file) => file.notifications,
            Err(e) => {
                eprintln!("[notifications] parse failed, treating as empty: {e}");
                Vec::new()
            }
        },
        Err(_) => Vec::new(),
    }
}

/// Persist the full list, creating the cache dir if needed. Atomic: write to a
/// temp file then rename so a crash mid-write cannot truncate the store.
fn save(items: &[Notification]) -> Result<(), String> {
    let path = store_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create store dir {}: {e}", parent.display()))?;
    }
    let file = NotificationsFile {
        version: 1,
        notifications: crate::jsonstore::cap_to_recent(items.to_vec(), MAX_NOTIFICATIONS),
    };
    let bytes =
        serde_json::to_vec_pretty(&file).map_err(|e| format!("could not serialize: {e}"))?;
    crate::jsonstore::write_atomic(&path, &bytes)
}

/// Next id: `<created_at>-<seq>` where `seq` is one past the largest existing
/// `-<seq>` suffix. Guarantees uniqueness even when several notifications land in
/// the same second (a batch of finished PDFs), which a bare timestamp would
/// collide on. Reuses the job store's clock so all stores share one time source.
fn next_id(items: &[Notification], created_at: u64) -> String {
    let max_seq = items
        .iter()
        .filter_map(|n| n.id.rsplit('-').next())
        .filter_map(|s| s.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    format!("{created_at}-{}", max_seq + 1)
}

/// Append a notification and persist. Returns the stored record so the frontend
/// can push it into its in-memory list without a reload.
pub fn add(kind: &str, title: &str, body: &str) -> Result<Notification, String> {
    let created_at = crate::store::now_secs();
    // Serialize load-mutate-save so concurrent adds cannot lose one (see jsonstore).
    crate::jsonstore::with_write_lock(|| {
        let mut items = load();
        let n = Notification {
            id: next_id(&items, created_at),
            kind: kind.to_string(),
            title: title.to_string(),
            body: body.to_string(),
            created_at,
            read: false,
        };
        items.push(n.clone());
        save(&items)?;
        Ok(n)
    })
}

/// Remove one notification by id. A missing id is a no-op success (the UI may
/// clear a stale row); the store is only rewritten when something changed.
pub fn clear(id: &str) -> Result<(), String> {
    crate::jsonstore::with_write_lock(|| {
        let mut items = load();
        let before = items.len();
        items.retain(|n| n.id != id);
        if items.len() != before {
            save(&items)?;
        }
        Ok(())
    })
}

/// Mark every notification read and persist. Returns the updated list so the
/// frontend can re-render without a reload. Cheap full rewrite (small list).
pub fn mark_all_read() -> Result<Vec<Notification>, String> {
    crate::jsonstore::with_write_lock(|| {
        let mut items = load();
        let mut changed = false;
        for n in items.iter_mut() {
            if !n.read {
                n.read = true;
                changed = true;
            }
        }
        if changed {
            save(&items)?;
        }
        Ok(items)
    })
}

/// Drop every notification (Clear all). Writes an empty list.
pub fn clear_all() -> Result<(), String> {
    // Under the lock so it cannot interleave with an in-flight add's load/save.
    crate::jsonstore::with_write_lock(|| save(&[]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// next_id must be unique within the same second (batch completions) and
    /// monotonic, since a bare timestamp would collide and break clear-by-id.
    #[test]
    fn next_id_unique_within_same_second() {
        let mut items: Vec<Notification> = Vec::new();
        let a = next_id(&items, 100);
        items.push(Notification {
            id: a.clone(),
            kind: "info".into(),
            title: "a".into(),
            body: "".into(),
            created_at: 100,
            read: false,
        });
        let b = next_id(&items, 100);
        assert_ne!(a, b, "same-second ids must differ");
        assert_eq!(a, "100-1");
        assert_eq!(b, "100-2");
    }

    /// The cap keeps exactly MAX_NOTIFICATIONS, drops the oldest, keeps the newest
    /// tail. Pure helper, so no cache dir is touched.
    #[test]
    fn cap_to_recent_keeps_newest_tail() {
        let mk = |i: u64| Notification {
            id: format!("{i}-1"),
            kind: "info".into(),
            title: format!("n{i}"),
            body: String::new(),
            created_at: i,
            read: false,
        };
        let few: Vec<Notification> = (0..5).map(mk).collect();
        assert_eq!(
            crate::jsonstore::cap_to_recent(few, MAX_NOTIFICATIONS).len(),
            5
        );

        // Cap logic is covered in jsonstore; this asserts MAX_NOTIFICATIONS is wired.
        let many: Vec<Notification> = (0..(MAX_NOTIFICATIONS as u64 + 30)).map(mk).collect();
        let capped = crate::jsonstore::cap_to_recent(many, MAX_NOTIFICATIONS);
        assert_eq!(capped.len(), MAX_NOTIFICATIONS, "must trim to the cap");
        assert_eq!(capped.first().unwrap().created_at, 30, "oldest 30 dropped");
        assert_eq!(
            capped.last().unwrap().created_at,
            MAX_NOTIFICATIONS as u64 + 29,
            "newest kept at the tail"
        );
    }

    /// Envelope round-trips through serde with camelCase on the wire (regression
    /// guard for the rename the frontend depends on).
    #[test]
    fn file_roundtrips_camelcase() {
        let n = Notification {
            id: "1-1".into(),
            kind: "done".into(),
            title: "report.pdf".into(),
            body: "/tmp/report.md".into(),
            created_at: 1_700_000_001,
            read: false,
        };
        let file = NotificationsFile {
            version: 1,
            notifications: vec![n],
        };
        let json = serde_json::to_string(&file).unwrap();
        assert!(
            json.contains("\"createdAt\""),
            "expected camelCase createdAt"
        );
        assert!(!json.contains("\"created_at\""));
        let back: NotificationsFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.notifications.len(), 1);
        assert_eq!(back.notifications[0].kind, "done");
    }
}
