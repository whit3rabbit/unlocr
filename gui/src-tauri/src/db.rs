//! SQLite backing for the persisted stores (`store.rs`, `settings.rs`,
//! `notifications.rs`). Replaces the three JSON files that used to live under the
//! GGUF cache dir with a single `unlocr.db` in the app-DATA dir (user data, not
//! regenerable cache).
//!
//! One process-wide `Connection` behind a `Mutex`, reached via `with_db`. This
//! keeps the existing handle-free access pattern: the idle-unload watcher thread
//! (`lib.rs`) and `allowed_output_paths` (`cmd_run.rs`) call the store fns
//! directly with no Tauri `AppHandle`, so the connection cannot live behind
//! `tauri::State`. Writes are rare (one per OCR run / settings save /
//! notification) and tiny, so a single coarse connection is plenty.
//!
//! `init()` runs once from `run()`'s `.setup()`; a failure to open the DB is a
//! hard startup error (returns `Err`, aborting setup) rather than the old
//! silent-degrade-to-empty, because a GUI that cannot open its store is broken,
//! not merely empty. `with_db` still falls back to `init()` defensively and
//! propagates any open error as a `String`; the store load fns then map that to
//! empty/default so the UI never wedges.
//!
//! Schema versioning uses `PRAGMA user_version` (replaces the per-JSON-file
//! `version:1` envelope). Bump it + add a migration step on the next schema change.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use rusqlite::Connection;

/// The one warm DB connection. `OnceLock` (not `LazyLock`) so `init()` can return
/// a `Result` for open failures instead of panicking inside a lazy closure. First
/// `init()` wins; later calls are no-ops.
static DB: OnceLock<Mutex<Connection>> = OnceLock::new();

/// Authoritative schema: three tables + their indexes. Idempotent
/// (`IF NOT EXISTS`), so re-running on an existing DB is a no-op. `PRAGMA
/// user_version` and the connection-mode PRAGMAs are applied separately (see
/// `init_conn` / `open_and_configure`) so the same schema text drives both the
/// file DB and in-memory test DBs.
const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS jobs (
    id           TEXT    PRIMARY KEY,
    input_path   TEXT    NOT NULL,
    quant        TEXT    NOT NULL,
    max_tokens   INTEGER NOT NULL,
    dpi          INTEGER NOT NULL,
    prompt       TEXT    NOT NULL,
    keep_images  INTEGER NOT NULL DEFAULT 0 CHECK(keep_images IN (0,1)),
    status       TEXT    NOT NULL,
    output_path  TEXT    NOT NULL DEFAULT '',
    error        TEXT    NOT NULL DEFAULT '',
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_jobs_created_at ON jobs(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_jobs_status      ON jobs(status);

CREATE TABLE IF NOT EXISTS settings (
    id                  INTEGER PRIMARY KEY CHECK(id = 1),
    mode                TEXT    NOT NULL DEFAULT 'local',
    remote_base_url     TEXT    NOT NULL DEFAULT 'http://127.0.0.1:8080',
    remote_api_key      TEXT    NOT NULL DEFAULT '',
    remote_model        TEXT    NOT NULL DEFAULT '',
    default_quant       TEXT    NOT NULL,
    llama_bin           TEXT    NOT NULL DEFAULT '',
    default_dpi         INTEGER NOT NULL,
    default_max_tokens  INTEGER NOT NULL,
    default_prompt      TEXT    NOT NULL,
    idle_unload_minutes INTEGER NOT NULL DEFAULT 15
);

CREATE TABLE IF NOT EXISTS notifications (
    id          TEXT    PRIMARY KEY,
    kind        TEXT    NOT NULL,
    title       TEXT    NOT NULL,
    body        TEXT    NOT NULL DEFAULT '',
    created_at  INTEGER NOT NULL,
    read        INTEGER NOT NULL DEFAULT 0 CHECK(read IN (0,1))
);
CREATE INDEX IF NOT EXISTS idx_notifications_created_at ON notifications(created_at DESC);
"#;

/// Per-OS app-data base dir. Delegates to the root crate's `model::base_data_dir`
/// (mirrors `base_cache_dir`) so the XDG + OS-switch ladder lives in one place;
/// kept as a plain resolver (not `app.path().app_data_dir()`) so the DB module is
/// handle-free, unit-testable, and resolvable from the watcher thread.
fn base_data_dir() -> Result<PathBuf, String> {
    unlocr::model::base_data_dir().map_err(|e| format!("could not resolve data dir: {e}"))
}

/// `<app-data base>/unlocr`, creating it. Where the DB (and its WAL sidecars)
/// live. Public so a command can report the location to the UI.
pub(crate) fn app_data_dir() -> Result<PathBuf, String> {
    let dir = base_data_dir()?.join("unlocr");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create app-data dir {}: {e}", dir.display()))?;
    Ok(dir)
}

/// `<app-data>/unlocr/unlocr.db`.
pub(crate) fn db_path() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join("unlocr.db"))
}

/// Apply the schema + connection PRAGMAs to an already-open connection. Shared by
/// the file connection (`open_and_configure`) and in-memory test connections.
/// `user_version` pins the schema at 1; bump + migrate here when the schema grows.
pub(crate) fn init_conn(conn: &Connection) -> Result<(), String> {
    conn.pragma_update(None, "foreign_keys", true)
        .map_err(|e| format!("could not enable foreign_keys: {e}"))?;
    conn.pragma_update(None, "busy_timeout", 5000u32)
        .map_err(|e| format!("could not set busy_timeout: {e}"))?;
    conn.execute_batch(SCHEMA_SQL)
        .map_err(|e| format!("could not apply DB schema: {e}"))?;
    let v: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap_or(0);
    if v == 0 {
        conn.pragma_update(None, "user_version", 1u32)
            .map_err(|e| format!("could not set user_version: {e}"))?;
    }
    Ok(())
}

/// Open the file DB, switch to WAL (faster reads; the file is not rewritten on
/// every commit), then apply schema + PRAGMAs via `init_conn`. WAL sidecar files
/// (`-wal`, `-shm`) land next to `unlocr.db` and are SQLite-internal.
fn open_and_configure() -> Result<Connection, String> {
    let path = db_path()?;
    let conn = Connection::open(&path)
        .map_err(|e| format!("could not open DB at {}: {e}", path.display()))?;
    // WAL is meaningful only on a file DB (in-memory stays "memory"); ignore the
    // returned mode. This is the file connection, so it takes effect.
    let _ = conn.pragma_update(None, "journal_mode", "WAL");
    init_conn(&conn)?;
    Ok(conn)
}

/// Open + configure the DB and stash the connection in the global slot. Called
/// first thing from `.setup()`; a failure aborts startup with a clear message.
/// Idempotent: a second call is a no-op (the first open wins).
pub(crate) fn init() -> Result<(), String> {
    if DB.get().is_some() {
        return Ok(());
    }
    let conn = open_and_configure()?;
    // First init wins; a concurrent second `set` is a harmless `Err`.
    let _ = DB.set(Mutex::new(conn));
    Ok(())
}

/// Run `f` against the one warm connection under its lock. Lazily `init()`s if
/// `.setup()` somehow did not (defensive; propagates the open error as `String`
/// so store load fns can degrade to empty/default). Recovers from a poisoned
/// mutex: the state lives on disk, not behind the mutex, so a panic in a prior
/// critical section does not wedge the whole store.
pub(crate) fn with_db<R>(f: impl FnOnce(&Connection) -> Result<R, String>) -> Result<R, String> {
    if DB.get().is_none() {
        init()?;
    }
    // Safe after init(): the slot is populated; the closure above ran init().
    let mutex = DB.get().expect("DB initialized");
    let conn = mutex.lock().unwrap_or_else(|p| p.into_inner());
    f(&conn)
}

/// Fresh in-memory DB with the schema + PRAGMAs applied. The single source for the
/// `mem_db()` test helper so store/settings/notifications tests share one schema
/// initializer instead of each open-coding `open_in_memory` + `init_conn`.
#[cfg(test)]
pub(crate) fn mem_db() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    init_conn(&conn).unwrap();
    conn
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh in-memory DB reports `user_version = 1` after `init_conn`, the
    /// schema-version hook that replaces the old per-JSON `version` envelope.
    #[test]
    fn schema_sets_user_version_to_1() {
        let conn = Connection::open_in_memory().unwrap();
        init_conn(&conn).unwrap();
        let v: u32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(v, 1);
    }

    /// `init_conn` is idempotent: running it twice on the same DB does not error
    /// (every statement is `IF NOT EXISTS`), so re-init on an existing install is
    /// safe.
    #[test]
    fn init_conn_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init_conn(&conn).unwrap();
        init_conn(&conn).unwrap();
        // Three tables present.
        let n: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('jobs','settings','notifications')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(n, 3);
    }
}
