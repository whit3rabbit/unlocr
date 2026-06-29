//! Persisted OCR job store.
//!
//! Records each `run_ocr` job to the SQLite store (`unlocr.db`, see `db.rs`) so
//! the Library grid and Workflow board can render past runs across app restarts.
//! One row per run; the `JobOptions` snapshot is flattened into columns so the
//! Library's newest-first (`created_at`) sort and the Board's `status` grouping
//! are index-friendly.
//!
//! Scope: persistence + typed accessors only. The Library/Board UI views and the
//! drag-drop importer live in the frontend. This module is additive over `db.rs`.
//!
//! Uncapped by design: history grows with the user's runs (the old 500-row cap is
//! gone). The DB layer (`jobs::*`) takes a `&Connection` so tests drive it against
//! an in-memory DB; the public fns (`load_jobs` etc.) wrap it in `db::with_db`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// One OCR run as the Library/Board UI renders it. Field names are camelCase on
/// the wire so the JS side reads `job.inputPath`, `job.outputPath`, etc. without
/// a rename layer. `options` mirrors the `OcrOptions` the run actually used.
///
/// Status is a coarse string (queued/running/done/failed) rather than an enum on
/// the wire so a future status value does not break older frontends. The UI groups
/// by this string into Board columns.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    /// Stable id. `<unix_secs>-<input-stem>-<path_hash>-<seq>`: the path hash
    /// disambiguates same-stem inputs from different folders, and the per-process
    /// seq makes two runs of the same file in the same second distinct.
    pub id: String,
    /// Absolute or relative path of the source PDF, exactly as passed to run_ocr.
    pub input_path: String,
    /// The effective OcrOptions the run used (echoed from the run_ocr payload).
    pub options: JobOptions,
    /// queued | running | done | failed.
    pub status: String,
    /// Path to the written `{stem}.md`, empty when the run was in-memory only.
    pub output_path: String,
    /// Error text when status == "failed", empty otherwise.
    pub error: String,
    /// Unix epoch seconds the record was written. The frontend records a job once,
    /// after run_ocr returns/throws, so this is effectively the terminal time.
    pub created_at: u64,
    /// Unix epoch seconds of the last write. Equals `created_at` today (records are
    /// written once at terminal state; there is no separate queued-time insert). A
    /// future queued -> running -> done state machine would advance this on update.
    pub updated_at: u64,
}

/// Snapshot of the OcrOptions a job ran with. Kept as its own struct (not a
/// re-export of `unlocr::OcrOptions`) so the on-disk schema is stable even if the
/// backend options struct grows new fields later. Mirrors the fields the run_ocr
/// command accepts today.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobOptions {
    pub quant: String,
    pub max_tokens: u32,
    pub dpi: u32,
    pub prompt: String,
    pub keep_images: bool,
}

impl JobOptions {
    /// Build from the same-shaped params the `run_ocr` command receives. Lets the
    /// record command echo exactly what the run used without re-parsing strings.
    pub fn from_opts(
        quant: &str,
        max_tokens: u32,
        dpi: u32,
        prompt: &str,
        keep_images: bool,
    ) -> Self {
        Self {
            quant: quant.to_string(),
            max_tokens,
            dpi,
            prompt: prompt.to_string(),
            keep_images,
        }
    }
}

/// Path of the SQLite store, for surfacing to the UI (the `jobs_store_path`
/// command). Delegates to `db::db_path` (`<app-data>/unlocr/unlocr.db`).
pub fn store_path() -> Result<PathBuf, String> {
    crate::db::db_path()
}

// --- DB layer ---------------------------------------------------------------
//
// Pure functions over a `&Connection`, split out from the `with_db` wrappers so a
// unit test can drive them against `Connection::open_in_memory()` + `init_conn`
// (no app-data dir, no global lock). The public fns below stitch them onto the
// one warm process-wide connection.

mod jobs {
    use super::{Job, JobOptions};
    use rusqlite::{params, Connection};

    /// Upsert by id (insert-or-replace). Used by both the "record a fresh run"
    /// path and a future "mark a queued job done" update.
    pub(crate) fn insert(conn: &Connection, job: &Job) -> Result<(), String> {
        conn.execute(
            "INSERT OR REPLACE INTO jobs
               (id, input_path, quant, max_tokens, dpi, prompt, keep_images,
                status, output_path, error, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                job.id,
                job.input_path,
                job.options.quant,
                job.options.max_tokens,
                job.options.dpi,
                job.options.prompt,
                job.options.keep_images,
                job.status,
                job.output_path,
                job.error,
                job.created_at as i64,
                job.updated_at as i64,
            ],
        )
        .map_err(|e| format!("could not insert job {}: {e}", job.id))?;
        Ok(())
    }

    /// Move an existing job to a new state, advancing `updated_at` and preserving
    /// `created_at` (unlike `insert`'s INSERT OR REPLACE, which rewrites the whole
    /// row). The start/finish lifecycle: `start_job` inserts a `running` row, this
    /// flips it to done/failed when the run ends. A missing id is a silent no-op
    /// (0 rows updated), matching the "missing is fine" theme.
    pub(crate) fn update_status(
        conn: &Connection,
        id: &str,
        status: &str,
        output_path: &str,
        error: &str,
        updated_at: u64,
    ) -> Result<(), String> {
        conn.execute(
            "UPDATE jobs SET status = ?2, output_path = ?3, error = ?4, updated_at = ?5
             WHERE id = ?1",
            params![id, status, output_path, error, updated_at as i64],
        )
        .map_err(|e| format!("could not update job {id}: {e}"))?;
        Ok(())
    }

    /// Flip every row stuck in `running` to `failed` ("interrupted"). No run can
    /// survive a process restart, so a `running` row at startup is a crash artifact.
    /// Returns the number of rows reconciled. Called once from `.setup()`.
    pub(crate) fn reconcile_interrupted(conn: &Connection, now: u64) -> Result<usize, String> {
        let n = conn
            .execute(
                "UPDATE jobs SET status = 'failed',
                        error = 'interrupted (app restarted)', updated_at = ?1
                 WHERE status = 'running'",
                params![now as i64],
            )
            .map_err(|e| format!("could not reconcile interrupted jobs: {e}"))?;
        Ok(n)
    }

    /// Every job, newest first (the order the Library renders; `idx_jobs_created_at`
    /// serves it). The Board re-groups by `status` client-side.
    pub(crate) fn list(conn: &Connection) -> Result<Vec<Job>, String> {
        let mut stmt = conn
            .prepare(
                "SELECT id, input_path, quant, max_tokens, dpi, prompt, keep_images,
                        status, output_path, error, created_at, updated_at
                 FROM jobs ORDER BY created_at DESC",
            )
            .map_err(|e| format!("could not prepare jobs select: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Job {
                    id: row.get(0)?,
                    input_path: row.get(1)?,
                    options: JobOptions {
                        quant: row.get(2)?,
                        max_tokens: row.get(3)?,
                        dpi: row.get(4)?,
                        prompt: row.get(5)?,
                        keep_images: row.get(6)?,
                    },
                    status: row.get(7)?,
                    output_path: row.get(8)?,
                    error: row.get(9)?,
                    created_at: row.get::<_, i64>(10)? as u64,
                    updated_at: row.get::<_, i64>(11)? as u64,
                })
            })
            .map_err(|e| format!("could not query jobs: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("could not read job row: {e}"))?);
        }
        Ok(out)
    }

    /// The `output_path` of one job by id, without deleting it. Used so the
    /// `delete_job` command can delete the file BEFORE dropping the record (a delete
    /// failure then leaves the record in place instead of orphaning the file).
    /// `Ok(None)` when the id is absent.
    pub(crate) fn output_path(conn: &Connection, id: &str) -> Result<Option<String>, String> {
        match conn.query_row(
            "SELECT output_path FROM jobs WHERE id = ?1",
            params![id],
            |row| row.get::<_, String>(0),
        ) {
            Ok(p) => Ok(Some(p)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("could not read job {id}: {e}")),
        }
    }

    /// Every non-empty `output_path`, without hydrating full `Job` rows (a single
    /// column, no `JobOptions` build). Backs the read-allowlist cache in
    /// `cmd_run::allowed_output_paths` and the file-delete-first `clear_jobs` flow.
    pub(crate) fn output_paths(conn: &Connection) -> Result<Vec<String>, String> {
        let mut stmt = conn
            .prepare("SELECT output_path FROM jobs WHERE output_path != ''")
            .map_err(|e| format!("could not prepare output_paths select: {e}"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("could not query output_paths: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("could not read output_path row: {e}"))?);
        }
        Ok(out)
    }

    /// Remove one job by id. Returns the removed job's `output_path` (if any) so the
    /// caller can optionally delete the file. `Ok(None)` when the id is absent (a
    /// no-op success, matching the "missing is fine" theme). RETURNING needs SQLite
    /// >= 3.35; the bundled build is current, so this works.
    pub(crate) fn delete(conn: &Connection, id: &str) -> Result<Option<String>, String> {
        match conn.query_row(
            "DELETE FROM jobs WHERE id = ?1 RETURNING output_path",
            params![id],
            |row| row.get::<_, String>(0),
        ) {
            Ok(p) => Ok(Some(p)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("could not delete job {id}: {e}")),
        }
    }

    /// Drop every job. Returns the non-empty `output_path`s that were recorded so
    /// the caller can optionally delete those files (the "remove all and delete"
    /// variant).
    pub(crate) fn clear(conn: &Connection) -> Result<Vec<String>, String> {
        let mut stmt = conn
            .prepare("DELETE FROM jobs RETURNING output_path")
            .map_err(|e| format!("could not prepare jobs clear: {e}"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("could not clear jobs: {e}"))?;
        let mut outs = Vec::new();
        for row in rows {
            let p = row.map_err(|e| format!("could not read cleared row: {e}"))?;
            if !p.is_empty() {
                outs.push(p);
            }
        }
        Ok(outs)
    }
}

// --- public accessors (the command-facing surface) --------------------------

/// Load all jobs. A DB error is logged and treated as empty so a bad state never
/// wedges the UI (mirrors the old "corrupt file -> empty" contract).
pub fn load_jobs() -> Vec<Job> {
    crate::db::with_db(jobs::list).unwrap_or_else(|e| {
        eprintln!("[store] jobs load failed, treating as empty: {e}");
        Vec::new()
    })
}

/// Insert a fresh job in the `running` state at the START of a run, returning the
/// stored `Job` so the caller (the `run_ocr` loop) holds its id to finish it later.
/// `created_at`/`updated_at` are both "now"; `updated_at` advances when
/// `finish_job` flips the row to its terminal state. This is what makes the Board's
/// `running` column live (previously no code ever wrote that status).
pub fn start_job(input_path: &str, options: JobOptions) -> Result<Job, String> {
    let now = now_secs();
    let job = Job {
        id: make_id(input_path, now),
        input_path: input_path.to_string(),
        options,
        status: "running".to_string(),
        output_path: String::new(),
        error: String::new(),
        created_at: now,
        updated_at: now,
    };
    crate::db::with_db(|c| jobs::insert(c, &job))?;
    Ok(job)
}

/// Move a started job to its terminal state (`done` or `failed`), advancing
/// `updated_at` and preserving `created_at`. Pair with `start_job`: the caller
/// passes back the id `start_job` returned.
pub fn finish_job(id: &str, status: &str, output_path: &str, error: &str) -> Result<(), String> {
    let updated_at = now_secs();
    crate::db::with_db(|c| jobs::update_status(c, id, status, output_path, error, updated_at))
}

/// Flip any rows left `running` by a previous session that crashed mid-run to
/// `failed`. Called once from `.setup()` so the Board is accurate on launch.
/// Returns the number reconciled.
pub fn reconcile_interrupted() -> Result<usize, String> {
    crate::db::with_db(|c| jobs::reconcile_interrupted(c, now_secs()))
}

/// Remove one job by id and persist the rest. Returns the removed job's
/// `output_path` so the command can optionally delete the file. `Ok(None)` when
/// the id is not present (a no-op success).
pub fn remove_job(id: &str) -> Result<Option<String>, String> {
    crate::db::with_db(|c| jobs::delete(c, id))
}

/// Clear every job. Returns the non-empty `output_path`s that were recorded so the
/// caller can optionally delete those files (the "remove all and delete" variant).
pub fn clear_jobs() -> Result<Vec<String>, String> {
    crate::db::with_db(jobs::clear)
}

/// Read one job's `output_path` without removing the record. Lets `delete_job`
/// delete the file first, then the record (so a delete failure keeps the record).
pub fn peek_job_output(id: &str) -> Result<Option<String>, String> {
    crate::db::with_db(|c| jobs::output_path(c, id))
}

/// Read every non-empty `output_path` without hydrating full rows. Backs the
/// read-allowlist cache and the file-delete-first `clear_jobs` flow.
pub fn peek_job_outputs() -> Result<Vec<String>, String> {
    crate::db::with_db(jobs::output_paths)
}

// --- id / clock / file helpers (storage-independent) ------------------------

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
    format!(
        "{created_at}-{clean}-{:08x}-{seq}",
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

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Fresh in-memory DB with the schema applied; tests drive `jobs::*` against it.
    fn mem_db() -> Connection {
        crate::db::mem_db()
    }

    fn job(id: &str, out: &str) -> Job {
        Job {
            id: id.into(),
            input_path: format!("/tmp/{id}.pdf"),
            options: JobOptions::from_opts("Q8_0", 4096, 144, "prompt", false),
            status: "done".into(),
            output_path: out.into(),
            error: String::new(),
            created_at: 100,
            updated_at: 200,
        }
    }

    /// Insert one job, list returns it, every flattened JobOptions field survives
    /// the round-trip (the row-to-struct mapping the frontend's wire shape relies on).
    #[test]
    fn job_insert_then_list_roundtrips() {
        let conn = mem_db();
        let j = job("1-s", "/tmp/x.md");
        jobs::insert(&conn, &j).unwrap();
        let got = jobs::list(&conn).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "1-s");
        assert_eq!(got[0].input_path, "/tmp/1-s.pdf");
        assert_eq!(got[0].options.quant, "Q8_0");
        assert_eq!(got[0].options.max_tokens, 4096);
        assert_eq!(got[0].options.dpi, 144);
        assert_eq!(got[0].options.prompt, "prompt");
        assert!(!got[0].options.keep_images);
        assert_eq!(got[0].status, "done");
        assert_eq!(got[0].output_path, "/tmp/x.md");
        assert_eq!(got[0].created_at, 100);
        assert_eq!(got[0].updated_at, 200);
    }

    /// INSERT OR REPLACE: a second write with the same id updates, never duplicates.
    #[test]
    fn job_upsert_no_duplicate() {
        let conn = mem_db();
        let mut j = job("x", "/tmp/x.md");
        jobs::insert(&conn, &j).unwrap();
        j.status = "failed".into();
        j.error = "boom".into();
        jobs::insert(&conn, &j).unwrap();
        let got = jobs::list(&conn).unwrap();
        assert_eq!(got.len(), 1, "same id must not duplicate");
        assert_eq!(got[0].status, "failed");
        assert_eq!(got[0].error, "boom");
    }

    /// update_status flips the row to its terminal state, advancing updated_at,
    /// WITHOUT touching created_at (the start->finish lifecycle relies on this).
    #[test]
    fn job_update_status_preserves_created_at() {
        let conn = mem_db();
        let mut j = job("u", "");
        j.status = "running".into();
        j.created_at = 100;
        j.updated_at = 100;
        jobs::insert(&conn, &j).unwrap();
        jobs::update_status(&conn, "u", "done", "/tmp/u.md", "", 200).unwrap();
        let got = jobs::list(&conn).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].status, "done");
        assert_eq!(got[0].output_path, "/tmp/u.md");
        assert_eq!(got[0].updated_at, 200);
        assert_eq!(got[0].created_at, 100, "created_at must survive an update");
    }

    /// update_status on a missing id is a silent no-op (0 rows), not an error.
    #[test]
    fn job_update_status_unknown_is_noop() {
        let conn = mem_db();
        jobs::update_status(&conn, "nope", "done", "", "", 1).unwrap();
        assert!(jobs::list(&conn).unwrap().is_empty());
    }

    /// reconcile_interrupted flips ONLY `running` rows to failed, stamps updated_at,
    /// and leaves done/failed rows untouched (startup crash recovery).
    #[test]
    fn reconcile_flips_only_running() {
        let conn = mem_db();
        let mut r1 = job("r1", "");
        r1.status = "running".into();
        let mut r2 = job("r2", "");
        r2.status = "running".into();
        jobs::insert(&conn, &r1).unwrap();
        jobs::insert(&conn, &r2).unwrap();
        jobs::insert(&conn, &job("d", "/tmp/d.md")).unwrap(); // status "done"
        let n = jobs::reconcile_interrupted(&conn, 999).unwrap();
        assert_eq!(n, 2, "only the two running rows flip");
        let got = jobs::list(&conn).unwrap();
        let failed: Vec<_> = got.iter().filter(|j| j.status == "failed").collect();
        assert_eq!(failed.len(), 2);
        for f in &failed {
            assert_eq!(f.updated_at, 999);
            assert!(f.error.contains("interrupted"));
        }
        assert!(got.iter().any(|j| j.status == "done"), "done row preserved");
    }

    /// delete returns the removed job's output_path and actually drops the row.
    #[test]
    fn job_delete_returns_output_path() {
        let conn = mem_db();
        jobs::insert(&conn, &job("a", "/tmp/a.md")).unwrap();
        jobs::insert(&conn, &job("b", "")).unwrap();
        let out = jobs::delete(&conn, "a").unwrap();
        assert_eq!(out.as_deref(), Some("/tmp/a.md"));
        let got = jobs::list(&conn).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "b");
    }

    /// Unknown id is a no-op: returns None, leaves the row count unchanged.
    #[test]
    fn job_delete_unknown_is_noop() {
        let conn = mem_db();
        jobs::insert(&conn, &job("a", "/tmp/a.md")).unwrap();
        assert_eq!(jobs::delete(&conn, "zzz").unwrap(), None);
        assert_eq!(jobs::list(&conn).unwrap().len(), 1);
    }

    /// clear wipes the table and returns only the non-empty output_paths.
    #[test]
    fn job_clear_returns_outputs() {
        let conn = mem_db();
        jobs::insert(&conn, &job("a", "/tmp/a.md")).unwrap();
        jobs::insert(&conn, &job("b", "")).unwrap();
        jobs::insert(&conn, &job("c", "/tmp/c.md")).unwrap();
        let outs = jobs::clear(&conn).unwrap();
        assert_eq!(outs.len(), 2, "only non-empty output_paths returned");
        assert!(outs.contains(&"/tmp/a.md".to_string()));
        assert!(outs.contains(&"/tmp/c.md".to_string()));
        assert!(jobs::list(&conn).unwrap().is_empty());
    }

    /// The old 500-row cap is GONE: 501 jobs all survive. The explicit proof the
    /// user asked for when dropping the JSON file limits.
    #[test]
    fn over_500_jobs_all_survive() {
        let conn = mem_db();
        for i in 0..501u64 {
            let j = Job {
                id: format!("{i}-x"),
                input_path: format!("/tmp/{i}.pdf"),
                options: JobOptions::from_opts("Q8_0", 4096, 144, "p", false),
                status: "done".into(),
                output_path: String::new(),
                error: String::new(),
                created_at: i,
                updated_at: i,
            };
            jobs::insert(&conn, &j).unwrap();
        }
        assert_eq!(jobs::list(&conn).unwrap().len(), 501);
    }

    /// `make_id` must be filesystem-safe, and two runs of the same file in the same
    /// second must get DISTINCT ids (the seq disambiguator), so the second run no
    /// longer upserts over the first.
    #[test]
    fn make_id_is_safe_and_unique() {
        let a = make_id("/some path/My Report #1.pdf", 12345);
        let b = make_id("/some path/My Report #1.pdf", 12345);
        assert_ne!(a, b, "two runs (same input+time) must get distinct ids");
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "id must be fs-safe (ASCII): {a}"
        );
        assert!(a.starts_with("12345-"));
        assert!(
            a.contains("My_Report"),
            "stem kept, unsafe chars mapped: {a}"
        );
    }

    /// Non-ASCII stems must be sanitized to `_` (the old is_alphanumeric leaked CJK).
    #[test]
    fn make_id_sanitizes_non_ascii() {
        let id = make_id("/docs/報告.pdf", 999);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "non-ASCII must be mapped to '_': {id}"
        );
    }

    /// Two same-stem inputs from different folders in the same second must NOT
    /// collide (the old <secs>-<stem> scheme overwrote one via upsert).
    #[test]
    fn make_id_disambiguates_same_stem_different_path() {
        let a = make_id("/a/report.pdf", 100);
        let b = make_id("/b/report.pdf", 100);
        assert_ne!(
            a, b,
            "different paths, same stem+time must differ: {a} == {b}"
        );
    }

    /// `JobOptions::from_opts` must echo each field verbatim.
    #[test]
    fn from_opts_echoes_fields() {
        let o = JobOptions::from_opts("Q4_K_M", 8192, 300, "<|grounding|>x", true);
        assert_eq!(o.quant, "Q4_K_M");
        assert_eq!(o.max_tokens, 8192);
        assert_eq!(o.dpi, 300);
        assert_eq!(o.prompt, "<|grounding|>x");
        assert!(o.keep_images);
    }

    /// now_secs is monotonic-ish and non-zero on a normal clock.
    #[test]
    fn now_secs_is_positive() {
        let n = now_secs();
        assert!(n > 1_700_000_000, "epoch seconds implausibly small: {n}");
    }

    /// delete_output_file refuses anything that is not a `.md` file and leaves it
    /// on disk; deletes a real `.md`; and is Ok on a missing/empty path.
    #[test]
    fn delete_output_file_guards_and_deletes() {
        // Empty path: no-op success.
        assert!(delete_output_file("").is_ok());
        // Missing .md: idempotent success.
        assert!(delete_output_file("/no/such/file.md").is_ok());

        let dir = tempfile::tempdir().unwrap();

        // Non-.md is rejected and the file survives.
        let txt = dir.path().join("keep.txt");
        std::fs::write(&txt, b"data").unwrap();
        assert!(delete_output_file(txt.to_str().unwrap()).is_err());
        assert!(txt.exists(), "non-.md must not be deleted");

        // Real .md is deleted.
        let md = dir.path().join("out.md");
        std::fs::write(&md, b"# hi").unwrap();
        assert!(delete_output_file(md.to_str().unwrap()).is_ok());
        assert!(!md.exists(), ".md should be removed");
    }
}
