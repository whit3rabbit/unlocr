// Shared app state for the unlocr GUI backend. The command handlers live in the
// `cmd_*` modules; this module holds the managed state they all touch (the warm
// model, the resolved pdftoppm, the cancel flag, and the held server pid).

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
}
