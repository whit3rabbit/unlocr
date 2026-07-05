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

use std::path::PathBuf;

/// Low-level database operations on the SQLite store.
pub mod db;
/// General helper utilities for store operations.
pub mod helpers;
/// Store-related structures and definitions.
pub mod types;

pub(crate) use helpers::is_md;
pub use helpers::{delete_output_file, make_id, now_secs};
pub use types::{Job, JobMetrics, JobOptions};

/// Path of the SQLite store, for surfacing to the UI (the `jobs_store_path`
/// command). Delegates to `db::db_path` (`<app-data>/unlocr/unlocr.db`).
pub fn store_path() -> Result<PathBuf, String> {
    crate::db::db_path()
}

// --- public accessors (the command-facing surface) --------------------------

/// Load all jobs. A DB error is logged and treated as empty so a bad state never
/// wedges the UI (mirrors the old "corrupt file -> empty" contract).
pub fn load_jobs() -> Vec<Job> {
    crate::db::with_db(db::list).unwrap_or_else(|e| {
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
        // Filled by finish_job; a running row has no results yet.
        page_count: None,
        duration_ms: None,
        backend: String::new(),
        output_mode: String::new(),
    };
    crate::db::with_db(|c| db::insert(c, &job))?;
    Ok(job)
}

/// Move a started job to its terminal state (`done` or `failed`), advancing
/// `updated_at` and preserving `created_at`. Pair with `start_job`: the caller
/// passes back the id `start_job` returned.
pub fn finish_job(
    id: &str,
    status: &str,
    output_path: &str,
    error: &str,
    metrics: &JobMetrics,
) -> Result<(), String> {
    let updated_at = now_secs();
    crate::db::with_db(|c| db::update_status(c, id, status, output_path, error, updated_at, metrics))
}

/// Flip any rows left `running` by a previous session that crashed mid-run to
/// `failed`. Called once from `.setup()` so the Board is accurate on launch.
/// Returns the number reconciled.
pub fn reconcile_interrupted() -> Result<usize, String> {
    crate::db::with_db(|c| db::reconcile_interrupted(c, now_secs()))
}

/// Remove one job by id and persist the rest. Returns the removed job's
/// `output_path` so the command can optionally delete the file. `Ok(None)` when
/// the id is not present (a no-op success).
pub fn remove_job(id: &str) -> Result<Option<String>, String> {
    crate::db::with_db(|c| db::delete(c, id))
}

/// Read one job's `output_path` without removing the record. Lets `delete_job`
/// delete the file first, then the record (so a delete failure keeps the record).
pub fn peek_job_output(id: &str) -> Result<Option<String>, String> {
    crate::db::with_db(|c| db::output_path(c, id))
}

/// Read every non-empty `output_path` without hydrating full rows. Backs the
/// read-allowlist cache and the file-delete-first `clear_jobs` flow.
pub fn peek_job_outputs() -> Result<Vec<String>, String> {
    crate::db::with_db(db::output_paths)
}

/// Read the `(id, output_path)` pairs for the given ids (non-empty paths only)
/// without removing anything. Lets the multi-select `delete_jobs` command delete
/// files per-id and keep only the failed-file records, mirroring `peek_job_output`
/// (one) and `peek_job_outputs` (all).
pub fn peek_job_outputs_for(ids: &[String]) -> Result<Vec<(String, String)>, String> {
    crate::db::with_db(|c| db::output_paths_for(c, ids))
}

/// Remove several jobs by id in one statement. The caller has already (optionally)
/// deleted their output files via `peek_job_outputs_for` + `delete_output_file`.
pub fn remove_jobs(ids: &[String]) -> Result<(), String> {
    crate::db::with_db(|c| db::delete_many(c, ids))
}

#[cfg(test)]
mod tests;
