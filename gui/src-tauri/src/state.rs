// Shared app state for the unlocr GUI backend. The command handlers live in the
// `cmd_*` modules; this module holds the managed state they all touch (the warm
// model, the resolved pdftoppm, the cancel flag, and the held server pid).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

use unlocr::server::{RemoteEndpoint, Server};

/// A loaded inference backend: a long-lived local llama-server (held so its model
/// stays warm in RAM until unloaded) or a remote OpenAI-compatible endpoint.
/// `Backend` is the litellm-style "loaded model" the Run gate checks for.
pub(crate) enum Backend {
    Local(Server),
    Remote(RemoteEndpoint),
}

/// The currently loaded model plus a human label for the status badge.
pub(crate) struct LoadedModel {
    pub(crate) backend: Backend,
    /// "Unlimited-OCR Q8_0" for local, the base URL for remote.
    pub(crate) label: String,
    /// "local" | "remote", echoed to the frontend for the badge.
    pub(crate) mode: String,
}

/// App-wide state managed by Tauri. `model` is None until Load succeeds (Run is
/// gated on it); dropping the `Server` inside it kills llama-server and frees RAM.
/// `pdftoppm` is resolved at load time because rasterization is always local, even
/// when inference is remote.
#[derive(Default)]
pub(crate) struct AppState {
    // ponytail: one Mutex held across a whole run serializes runs. Fine for a
    // single-user desktop app; split into a server pool if concurrent runs matter.
    pub(crate) model: Mutex<Option<LoadedModel>>,
    pub(crate) pdftoppm: Mutex<Option<PathBuf>>,
    // Set true by `stop_ocr` to abort an in-flight run; reset at the start of each
    // run (and at load). The run loop checks it at page boundaries; `stop_ocr` also
    // kills the local server below so an in-flight stream read aborts immediately.
    pub(crate) cancel: AtomicBool,
    // PID of the held local llama-server, stashed at load so `stop_ocr` can kill it
    // WITHOUT taking the `model` lock (the run loop holds that lock for the whole
    // batch). None for the remote backend (nothing local to kill).
    pub(crate) server_pid: Mutex<Option<u32>>,
    // Last time the model was loaded or a run finished, for the idle-unload watcher
    // (lib.rs): it drops the warm model after the configured idle window to reclaim
    // the GGUF RAM. None until first load (treated as "no model / not idle"). Option
    // keeps the Default derive (Instant has no Default). Never held across a run.
    pub(crate) last_used: Mutex<Option<std::time::Instant>>,
    // Canonicalized output paths this session's runs wrote. `read_text_file` only
    // serves files in this set (plus those recorded in the job store), so the
    // review pane's read scope is backend-derived, not supplied by the renderer:
    // a compromised webview cannot point it at an arbitrary .md on disk.
    pub(crate) read_allow: Mutex<HashSet<PathBuf>>,
    // Cached canonical job-store `output_path`s. Building it needs a DB query plus
    // one `canonicalize` per output, so it is computed once and invalidated
    // (`invalidate_job_outputs`) on every job insert/update/delete/clear. `None`
    // means "stale, rebuild". Without this, every review-pane read re-scanned the
    // whole jobs table and re-stat'd every output.
    pub(crate) job_output_cache: Mutex<Option<HashSet<PathBuf>>>,
    // Sibling files `export_markdown` wrote this session (e.g. report.docx). An
    // export may overwrite a file in THIS set (a re-export of the same format) but
    // must refuse a pre-existing file NOT in it, so exporting cannot silently
    // clobber an unrelated same-named file the user owns.
    pub(crate) exported_paths: Mutex<HashSet<PathBuf>>,
}

impl AppState {
    /// Mark the cached job-output allowlist stale. Call after any change to the
    /// jobs table (start/finish/delete/clear/reconcile) so the next read rebuilds.
    pub(crate) fn invalidate_job_outputs(&self) {
        if let Ok(mut g) = self.job_output_cache.lock() {
            *g = None;
        }
    }
}
