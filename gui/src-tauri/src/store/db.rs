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
