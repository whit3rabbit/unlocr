// Thin persistence-command wrappers over the data modules (store.rs, settings.rs,
// notifications.rs). The frontend's only access to the job store, GUI settings,
// and the notification panel. No OCR logic here; these just read/write JSON.

use unlocr::OcrOptions;

use crate::store::{Job, JobOptions};
use crate::{notifications, settings, store};

// --- job store commands (EH-0006 bite 1) -----------------------------------
//
// The store itself lives in `store.rs`; these thin commands are the frontend's
// only access. Two are required for this bite: `list_jobs` (Library/Board reads)
// and `record_job` (write a run's outcome after run_ocr returns/throws). A third,
// `jobs_store_path`, exposes the on-disk path so an acceptance check can `cat`
// the file and confirm one record per run. All three are additive; run_ocr is
// unchanged (the frontend decides when to record, keeping OCR and persistence
// decoupled so an OCR success is never rolled back by a store write failure).

/// Return every persisted job in insertion order. The frontend renders this into
/// the Library grid (all jobs) and the Board (grouped by `status`). An empty vec
/// on a first launch or a missing/corrupt store; never throws.
#[tauri::command]
pub(crate) fn list_jobs() -> Vec<Job> {
    store::load_jobs()
}

/// Absolute path of the `jobs.json` store under the model cache dir, as a string.
/// Surfaces the cache-dir resolution error (if any) so the UI/acceptance can tell
/// "no jobs yet" apart from "could not even locate the store". Used by the card's
/// "cat the file path and show record count" acceptance check.
#[tauri::command]
pub(crate) fn jobs_store_path() -> Result<String, String> {
    store::store_path().map(|p| p.display().to_string())
}

/// Record one run's outcome to the store. The frontend calls this right after a
/// `run_ocr` invocation completes (status="done", output_path set) or fails
/// (status="failed", error set). Options are echoed as the same-shaped struct the
/// `run_ocr` command received, so the stored record reflects what the run used.
///
/// Returns the stored `Job` (with its generated id) so the caller can append it to
/// an in-memory list without a full reload. A store write failure is surfaced as
/// Err rather than swallowed, but the OCR result it accompanies has already been
/// delivered to the user, so this never rolls back a successful run.
// Each arg is an invoke field (the JS contract); a struct would not reduce them.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(crate) fn record_job(
    input_path: String,
    quant: Option<String>,
    max_tokens: Option<u32>,
    dpi: Option<u32>,
    prompt: Option<String>,
    keep_images: Option<bool>,
    status: Option<String>,
    output_path: Option<String>,
    error: Option<String>,
) -> Result<Job, String> {
    // Defaults mirror OcrOptions::default() so a record with no options sent
    // matches a no-args run (the same convention run_ocr uses).
    let def = OcrOptions::default();
    let options = JobOptions::from_opts(
        quant.as_deref().unwrap_or(&def.quant),
        max_tokens.unwrap_or(def.max_tokens),
        dpi.unwrap_or(def.dpi),
        prompt.as_deref().unwrap_or(&def.prompt),
        keep_images.unwrap_or(def.keep_images),
    );
    // Validate status against the known set the Board buckets on. An unknown value
    // would render unstyled and be bucketed into "queued", hiding a finished run
    // from the Done column. Reject it: the frontend always sends a known value, so
    // an unknown is a bug, and recordRunOutcome swallows the Err (best-effort).
    let status = status.as_deref().unwrap_or("done");
    if !matches!(status, "queued" | "running" | "done" | "failed") {
        return Err(format!(
            "invalid status {status:?}: expected one of queued|running|done|failed"
        ));
    }
    store::record_outcome(
        &input_path,
        options,
        status,
        output_path.as_deref().unwrap_or(""),
        error.as_deref().unwrap_or(""),
    )
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
