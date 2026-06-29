//! Persisted notification store.
//!
//! Backs the notification panel (the bell dropdown): terminal/notable events
//! (a PDF finished, a run failed, a model download completed) are recorded here
//! so they survive across app restarts and can be cleared individually or all at
//! once. Mirrors `store.rs`/`settings.rs`: a table in the SQLite store (`db.rs`),
//! accessed via `db::with_db`. Uncapped by design (the old 200-row cap is gone).
//!
//! Scope: persistence + typed accessors only; the toast UI and panel live in the
//! frontend. Transient progress (download percent/speed) is NOT stored here, only
//! terminal events worth surfacing after the fact. Purely additive module.

use serde::{Deserialize, Serialize};

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
    /// One-line headline, e.g. "report.pdf: OCR complete".
    pub title: String,
    /// Optional detail, e.g. the output path or an error message. May be empty.
    pub body: String,
    /// Unix epoch seconds the notification was recorded.
    pub created_at: u64,
    /// Whether the user has seen it. New notifications start unread so the bell
    /// can show an unread count; the frontend flips this when the panel opens.
    pub read: bool,
}

// --- DB layer ---------------------------------------------------------------
//
// Pure functions over a `&Connection`, namespaced under `rows` so they do not
// collide with the public accessors of the same intent (add/clear/mark_all_read).
// Tests drive these against an in-memory DB; the public fns wrap them in with_db.

mod rows {
    use super::Notification;
    use rusqlite::{params, Connection};

    /// Upsert by id.
    pub(crate) fn insert(conn: &Connection, n: &Notification) -> Result<(), String> {
        conn.execute(
            "INSERT OR REPLACE INTO notifications
               (id, kind, title, body, created_at, read)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![n.id, n.kind, n.title, n.body, n.created_at as i64, n.read,],
        )
        .map_err(|e| format!("could not insert notification: {e}"))?;
        Ok(())
    }

    /// All notifications in insertion order (newest last), matching the old
    /// file order the panel relied on (the frontend reverses for display). The
    /// table is a normal rowid table, so `ORDER BY rowid` is stable insertion
    /// order regardless of the string id's lexicographic quirks.
    pub(crate) fn list(conn: &Connection) -> Result<Vec<Notification>, String> {
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, title, body, created_at, read
                 FROM notifications ORDER BY rowid ASC",
            )
            .map_err(|e| format!("could not prepare notifications select: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Notification {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    title: row.get(2)?,
                    body: row.get(3)?,
                    created_at: row.get::<_, i64>(4)? as u64,
                    read: row.get(5)?,
                })
            })
            .map_err(|e| format!("could not query notifications: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("could not read notification row: {e}"))?);
        }
        Ok(out)
    }

    /// Remove one notification by id. A missing id is a no-op (DELETE matches nothing).
    pub(crate) fn delete(conn: &Connection, id: &str) -> Result<(), String> {
        conn.execute("DELETE FROM notifications WHERE id = ?1", params![id])
            .map_err(|e| format!("could not delete notification {id}: {e}"))?;
        Ok(())
    }

    /// Set every unread notification read. Single atomic UPDATE.
    pub(crate) fn mark_all_read(conn: &Connection) -> Result<(), String> {
        conn.execute("UPDATE notifications SET read = 1 WHERE read = 0", [])
            .map_err(|e| format!("could not mark notifications read: {e}"))?;
        Ok(())
    }

    /// Drop every notification.
    pub(crate) fn clear_all(conn: &Connection) -> Result<(), String> {
        conn.execute("DELETE FROM notifications", [])
            .map_err(|e| format!("could not clear notifications: {e}"))?;
        Ok(())
    }
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

// --- public accessors (the command-facing surface) --------------------------

/// All stored notifications, newest last (insertion order). Empty on a DB error
/// (logged) so the panel never wedges.
pub fn load() -> Vec<Notification> {
    crate::db::with_db(rows::list).unwrap_or_else(|e| {
        eprintln!("[notifications] load failed, treating as empty: {e}");
        Vec::new()
    })
}

/// Append a notification and persist. Returns the stored record so the frontend
/// can push it into its in-memory list without a reload.
pub fn add(kind: &str, title: &str, body: &str) -> Result<Notification, String> {
    let created_at = crate::store::now_secs();
    crate::db::with_db(|conn| {
        // list first so next_id sees the current set (same-second uniqueness).
        let items = rows::list(conn)?;
        let n = Notification {
            id: next_id(&items, created_at),
            kind: kind.to_string(),
            title: title.to_string(),
            body: body.to_string(),
            created_at,
            read: false,
        };
        rows::insert(conn, &n)?;
        Ok(n)
    })
}

/// Remove one notification by id. A missing id is a no-op success.
pub fn clear(id: &str) -> Result<(), String> {
    crate::db::with_db(|c| rows::delete(c, id))
}

/// Mark every notification read and return the updated list so the bell's unread
/// badge clears without a reload.
pub fn mark_all_read() -> Result<Vec<Notification>, String> {
    crate::db::with_db(|conn| {
        rows::mark_all_read(conn)?;
        rows::list(conn)
    })
}

/// Drop every notification (Clear all button).
pub fn clear_all() -> Result<(), String> {
    crate::db::with_db(rows::clear_all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Fresh in-memory DB with the schema applied.
    fn mem_db() -> Connection {
        crate::db::mem_db()
    }

    fn note(id: &str, read: bool) -> Notification {
        Notification {
            id: id.into(),
            kind: "info".into(),
            title: format!("t-{id}"),
            body: "".into(),
            created_at: 100,
            read,
        }
    }

    /// add inserts a row the list returns, with read=false.
    #[test]
    fn add_then_list() {
        let conn = mem_db();
        // Simulate the add path (next_id over the current set, then insert).
        let items = rows::list(&conn).unwrap();
        let n = Notification {
            id: next_id(&items, 100),
            kind: "done".into(),
            title: "report.pdf".into(),
            body: "/tmp/report.md".into(),
            created_at: 100,
            read: false,
        };
        rows::insert(&conn, &n).unwrap();
        let got = rows::list(&conn).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "100-1");
        assert_eq!(got[0].kind, "done");
        assert_eq!(got[0].title, "report.pdf");
        assert_eq!(got[0].body, "/tmp/report.md");
        assert!(!got[0].read);
    }

    /// mark_all_read flips only the unread rows (and is idempotent on re-run).
    #[test]
    fn mark_all_read_flips_read() {
        let conn = mem_db();
        rows::insert(&conn, &note("100-1", false)).unwrap();
        rows::insert(&conn, &note("100-2", false)).unwrap();
        rows::insert(&conn, &note("100-3", true)).unwrap();
        rows::mark_all_read(&conn).unwrap();
        let got = rows::list(&conn).unwrap();
        assert_eq!(got.len(), 3);
        assert!(got.iter().all(|n| n.read));
        // Re-running does not error and leaves them read.
        rows::mark_all_read(&conn).unwrap();
        assert!(rows::list(&conn).unwrap().iter().all(|n| n.read));
    }

    /// clear removes only the matching id.
    #[test]
    fn clear_one() {
        let conn = mem_db();
        rows::insert(&conn, &note("100-1", false)).unwrap();
        rows::insert(&conn, &note("100-2", false)).unwrap();
        rows::delete(&conn, "100-1").unwrap();
        let got = rows::list(&conn).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "100-2");
        // Clearing an unknown id is a no-op.
        rows::delete(&conn, "zzz").unwrap();
        assert_eq!(rows::list(&conn).unwrap().len(), 1);
    }

    /// clear_all empties the table.
    #[test]
    fn clear_all_empties() {
        let conn = mem_db();
        rows::insert(&conn, &note("100-1", false)).unwrap();
        rows::insert(&conn, &note("100-2", false)).unwrap();
        rows::clear_all(&conn).unwrap();
        assert!(rows::list(&conn).unwrap().is_empty());
    }

    /// next_id must be unique within the same second (batch completions) and
    /// monotonic, since a bare timestamp would collide and break clear-by-id.
    #[test]
    fn next_id_unique_within_same_second() {
        let mut items: Vec<Notification> = Vec::new();
        let a = next_id(&items, 100);
        items.push(note(&a, false));
        let b = next_id(&items, 100);
        assert_ne!(a, b, "same-second ids must differ");
        assert_eq!(a, "100-1");
        assert_eq!(b, "100-2");
    }
}
