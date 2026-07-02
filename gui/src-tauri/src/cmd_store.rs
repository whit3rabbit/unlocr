// Thin persistence-command wrappers over the data modules (store.rs, settings.rs,
// notifications.rs). The frontend's only access to the job store, GUI settings,
// and the notification panel. No OCR logic here; these just read/write the SQLite
// store in db.rs.

use crate::state::AppState;
use crate::store::Job;
use crate::{notifications, settings, store};
use tauri::State;

// --- job store commands (EH-0006 bite 1) -----------------------------------
//
// The store itself lives in `store.rs`; these thin commands are the frontend's
// only access. `list_jobs` (Library/Board reads) + `jobs_store_path` (on-disk path
// for acceptance) are read-only here. The job LIFECYCLE (running -> done/failed) is
// owned by the backend `run_ocr` loop (cmd_run.rs) via `store::start_job` /
// `finish_job`, which emit `jobs://changed` so the views reload live; the frontend
// no longer writes job rows itself.

/// Return every persisted job in insertion order. The frontend renders this into
/// the Library grid (all jobs) and the Board (grouped by `status`). An empty vec
/// on a first launch or a missing/corrupt store; never throws.
#[tauri::command]
pub(crate) fn list_jobs() -> Vec<Job> {
    store::load_jobs()
}

/// Absolute path of the SQLite store (`unlocr.db` under the app-data dir), as a
/// string. Surfaces the path-resolution error (if any) so the UI/acceptance can
/// tell "no jobs yet" apart from "could not even locate the store".
#[tauri::command]
pub(crate) fn jobs_store_path() -> Result<String, String> {
    store::store_path().map(|p| p.display().to_string())
}

/// Remove one job from the Library by id. With `delete_file == Some(true)`, also
/// delete the run's `.md` output from disk (guarded to `.md` only in
/// `store::delete_output_file`); otherwise the record is dropped but the file is
/// left in place. A missing id is a no-op success. The frontend confirms the
/// file-deleting variant with a native dialog before invoking.
///
/// Order: the file is deleted BEFORE the record is dropped, so a file-delete
/// failure leaves the record in place (the user can retry or remove the file
/// manually) instead of orphaning the file with no Library entry to clean it.
#[tauri::command]
pub(crate) fn delete_job(
    id: String,
    delete_file: Option<bool>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let output_path = store::peek_job_output(&id)?;
    if delete_file == Some(true) {
        if let Some(path) = &output_path {
            store::delete_output_file(path)?;
        }
    }
    store::remove_job(&id)?;
    state.invalidate_job_outputs();
    Ok(())
}

/// Remove several jobs from the Library by id (the multi-select batch path). With
/// `delete_file == Some(true)`, also delete each selected run's `.md` output from
/// disk (guarded to `.md` only in `store::delete_output_file`); otherwise the
/// records are dropped but the files are left in place. Jobs with no output path
/// (queued/failed) have nothing to delete, so a mixed selection never errors on
/// them. An empty id list is a no-op success.
///
/// Files are deleted per-id: a job whose file was requested AND failed to delete
/// (locked, permission-denied) KEEPS its record so the user can retry once the
/// lock clears; every other selected record is removed. This never strands a
/// record pointing at an already-deleted file (a broken review link) AND never
/// orphans a file that still needs deleting. Any failures are aggregated into the
/// returned Err so the UI can surface them after the removable records are gone.
#[tauri::command]
pub(crate) fn delete_jobs(
    ids: Vec<String>,
    delete_file: Option<bool>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    // ids whose output file was requested but failed to delete: keep their records.
    let mut keep = std::collections::HashSet::new();
    let mut file_errors: Vec<String> = Vec::new();
    if delete_file == Some(true) {
        for (id, path) in store::peek_job_outputs_for(&ids)? {
            if let Err(e) = store::delete_output_file(&path) {
                file_errors.push(format!("{id}: {e}"));
                keep.insert(id);
            }
        }
    }
    let removable: Vec<String> = ids
        .iter()
        .filter(|id| !keep.contains(*id))
        .cloned()
        .collect();
    if !removable.is_empty() {
        store::remove_jobs(&removable)?;
        state.invalidate_job_outputs();
    }
    if !file_errors.is_empty() {
        return Err(format!(
            "some output files could not be deleted (their records were kept): {}",
            file_errors.join("; ")
        ));
    }
    Ok(())
}

// --- notification store commands -------------------------------------------
//
// Thin wrappers over `notifications.rs`. The frontend records a notification on
// terminal events (a PDF finished, a run failed, a download completed) and the
// bell panel reads/clears them. Transient download progress is NOT stored here;
// it rides the `ocr://progress` event into a live toast only.

/// All stored notifications, newest last (insertion order). Empty on first launch
/// or a missing/corrupt store; never throws.
#[tauri::command]
pub(crate) fn list_notifications() -> Vec<notifications::Notification> {
    notifications::load()
}

/// Append a notification. `kind` is one of done|error|download|info (the panel
/// maps it to an icon); unknown kinds render as info. Returns the stored record.
#[tauri::command]
pub(crate) fn add_notification(
    kind: String,
    title: String,
    body: Option<String>,
) -> Result<notifications::Notification, String> {
    notifications::add(&kind, &title, body.as_deref().unwrap_or(""))
}

/// Remove one notification by id. Missing id is a no-op success.
#[tauri::command]
pub(crate) fn clear_notification(id: String) -> Result<(), String> {
    notifications::clear(&id)
}

/// Mark every notification read (called when the panel opens) and return the
/// updated list so the bell's unread badge clears without a reload.
#[tauri::command]
pub(crate) fn mark_notifications_read() -> Result<Vec<notifications::Notification>, String> {
    notifications::mark_all_read()
}

/// Drop every notification (Clear all button).
#[tauri::command]
pub(crate) fn clear_all_notifications() -> Result<(), String> {
    notifications::clear_all()
}

// --- settings commands ------------------------------------------------------

/// Read persisted GUI settings (provider mode + engine defaults), falling back to
/// defaults on a missing/corrupt file.
#[tauri::command]
pub(crate) fn get_settings() -> settings::Settings {
    settings::load_settings()
}

/// Persist GUI settings. Param is `newSettings` on the wire (camelCase) to avoid
/// shadowing the `settings` module inside the body.
#[tauri::command]
pub(crate) fn save_settings(new_settings: settings::Settings) -> Result<(), String> {
    settings::save_settings(&new_settings)
}
