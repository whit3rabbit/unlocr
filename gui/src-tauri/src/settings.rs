//! Persisted GUI settings (provider mode + defaults).
//!
//! Same dependency-free pattern as `store.rs`: one JSON file (`settings.json`)
//! under the model cache dir, atomic write via temp + rename, missing/corrupt
//! file falls back to defaults so a first launch or a hand-deleted file never
//! wedges the app. Holds what the Settings panel persists: the local/remote
//! provider mode, the remote endpoint, and the engine-option defaults the
//! Workspace controls seed from.
//!
//! ponytail: `remoteApiKey` is stored as plaintext in this JSON under the OS
//! cache dir (same trust level as the GGUF cache). Upgrade path if it ever
//! matters: the OS keychain (adds a `keyring`-style dep). The Settings UI shows
//! a one-line warning so the storage location is not a surprise.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use unlocr::model::cache_dir;
use unlocr::OcrOptions;

const SETTINGS_FILE: &str = "settings.json";

/// Persisted settings. camelCase on the wire so the JS side reads
/// `settings.remoteBaseUrl` etc. without a rename layer. Every field has a
/// default so an older/partial file still deserializes (serde `default`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// "local" | "remote": which provider the Load button targets.
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Remote OpenAI-compatible base URL (no trailing slash needed). Defaults to
    /// llama-server's own default host:port so the field is prefilled; the user
    /// edits it when the server runs on a different port/IP.
    #[serde(default = "default_remote_base_url")]
    pub remote_base_url: String,
    /// Optional bearer token for the remote endpoint. Plaintext (see module note).
    #[serde(default)]
    pub remote_api_key: String,
    /// Optional model name sent in the request body. Required by multi-model
    /// gateways (litellm/vLLM); a bare remote llama-server ignores it.
    #[serde(default)]
    pub remote_model: String,
    /// Default quant the Workspace + local Load use.
    #[serde(default = "default_quant")]
    pub default_quant: String,
    /// Optional explicit llama-server path (empty = resolve from PATH/Homebrew).
    #[serde(default)]
    pub llama_bin: String,
    #[serde(default = "default_dpi")]
    pub default_dpi: u32,
    #[serde(default = "default_max_tokens")]
    pub default_max_tokens: u32,
    #[serde(default = "default_prompt")]
    pub default_prompt: String,
    /// Drop the warm model after this many idle minutes to reclaim the GGUF RAM
    /// (~6-8 GB). 0 disables (model stays warm until explicit unload / app exit).
    /// The watcher in lib.rs reads this each tick; an in-flight run is protected by
    /// the model lock (try_lock fails while a run holds it), so it never unloads
    /// mid-run. Default 15.
    #[serde(default = "default_idle_unload_minutes")]
    pub idle_unload_minutes: u32,
}

fn default_mode() -> String {
    "local".to_string()
}
// llama-server's own defaults: host 127.0.0.1, port 8080. Prefilling this means
// the Remote field is ready to edit instead of blank.
fn default_remote_base_url() -> String {
    "http://127.0.0.1:8080".to_string()
}
fn default_quant() -> String {
    OcrOptions::default().quant
}
fn default_dpi() -> u32 {
    OcrOptions::default().dpi
}
fn default_max_tokens() -> u32 {
    OcrOptions::default().max_tokens
}
fn default_prompt() -> String {
    OcrOptions::default().prompt
}
fn default_idle_unload_minutes() -> u32 {
    15
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: default_mode(),
            remote_base_url: default_remote_base_url(),
            remote_api_key: String::new(),
            remote_model: String::new(),
            default_quant: default_quant(),
            llama_bin: String::new(),
            default_dpi: default_dpi(),
            default_max_tokens: default_max_tokens(),
            default_prompt: default_prompt(),
            idle_unload_minutes: default_idle_unload_minutes(),
        }
    }
}

/// On-disk envelope, versioned so a future migration can detect an older schema.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SettingsFile {
    version: u32,
    settings: Settings,
}

/// Resolve `<model cache dir>/settings.json`. Surfaces the cache-dir error like
/// `store::store_path` does.
pub fn settings_path() -> Result<PathBuf, String> {
    let cache = cache_dir(None).map_err(|e| format!("could not resolve model cache dir: {e}"))?;
    Ok(cache.join(SETTINGS_FILE))
}

/// Load settings, falling back to defaults on a missing or corrupt file.
pub fn load_settings() -> Settings {
    let path = match settings_path() {
        Ok(p) => p,
        Err(_) => return Settings::default(),
    };
    match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<SettingsFile>(&bytes) {
            Ok(file) => file.settings,
            Err(e) => {
                eprintln!("[settings] settings.json parse failed, using defaults: {e}");
                Settings::default()
            }
        },
        Err(_) => Settings::default(),
    }
}

/// Persist settings via temp + rename so a crash mid-write never truncates the file.
pub fn save_settings(settings: &Settings) -> Result<(), String> {
    let path = settings_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create settings dir {}: {e}", parent.display()))?;
    }
    let file = SettingsFile {
        version: 1,
        settings: settings.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&file)
        .map_err(|e| format!("could not serialize settings: {e}"))?;
    crate::jsonstore::write_atomic(&path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A partial file (only `mode`) must fill the rest from defaults, not fail.
    #[test]
    fn partial_file_fills_defaults() {
        let json = r#"{ "version": 1, "settings": { "mode": "remote" } }"#;
        let file: SettingsFile = serde_json::from_str(json).unwrap();
        assert_eq!(file.settings.mode, "remote");
        assert_eq!(file.settings.default_quant, OcrOptions::default().quant);
        assert_eq!(file.settings.default_dpi, OcrOptions::default().dpi);
        // A partial file must also pick up the prefilled remote URL, not "".
        assert_eq!(file.settings.remote_base_url, default_remote_base_url());
    }

    /// The remote base URL prefills to llama-server's default host:port.
    #[test]
    fn default_remote_url_is_llama_default() {
        assert_eq!(Settings::default().remote_base_url, "http://127.0.0.1:8080");
    }

    /// camelCase on the wire (regression guard for the serde rename).
    #[test]
    fn serializes_camel_case() {
        let json = serde_json::to_string(&SettingsFile {
            version: 1,
            settings: Settings::default(),
        })
        .unwrap();
        assert!(json.contains("\"remoteBaseUrl\""));
        assert!(json.contains("\"defaultMaxTokens\""));
        assert!(!json.contains("\"remote_base_url\""));
    }
}
