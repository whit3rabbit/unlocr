//! Persisted GUI settings (provider mode + defaults).
//!
//! Backed by the SQLite store (`unlocr.db`, see `db.rs`): a single-row `settings`
//! table (id = 1) holds the Settings panel's state across restarts. Missing row or
//! a DB error falls back to `Settings::default()` so a first launch or a corrupt
//! store never wedges the app. Holds the local/remote provider mode, the remote
//! endpoint, and the engine-option defaults the Workspace controls seed from.
//!
//! ponytail: `remoteApiKey` is stored as plaintext in the DB under the app-data
//! dir (same trust level as the old JSON-under-cache model). Upgrade path if it
//! ever matters: the OS keychain (adds a `keyring`-style dep). The Settings UI
//! shows a one-line warning so the storage location is not a surprise.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use unlocr::OcrOptions;

/// Persisted settings. camelCase on the wire so the JS side reads
/// `settings.remoteBaseUrl` etc. without a rename layer. Every field has a
/// default so an older/partial state still fills in (serde `default`), and a
/// missing DB row returns `Settings::default()` wholesale.
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
// Empty by default: the per-run Prompt box is an optional override, and an empty box
// falls back to the selected Task preset (options.js). A non-empty persistent value here
// seeds the run box. NOT OcrOptions::default().prompt: that would pre-fill the box and
// read like mandatory boilerplate (Unlimited-OCR uses no system prompt).
fn default_prompt() -> String {
    String::new()
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

// --- DB layer ---------------------------------------------------------------
//
// Pure functions over a `&Connection` (split from the `with_db` wrappers so tests
// drive them against an in-memory DB). The settings table is a singleton (id = 1).

/// Read the singleton settings row. A missing row (fresh install, cleared store)
/// yields `Settings::default()` rather than an error, mirroring the old
/// "missing file -> defaults" contract.
fn fetch(conn: &Connection) -> Result<Settings, String> {
    match conn.query_row(
        "SELECT mode, remote_base_url, remote_api_key, remote_model,
                default_quant, llama_bin, default_dpi, default_max_tokens,
                default_prompt, idle_unload_minutes
         FROM settings WHERE id = 1",
        [],
        |row| {
            Ok(Settings {
                mode: row.get(0)?,
                remote_base_url: row.get(1)?,
                remote_api_key: row.get(2)?,
                remote_model: row.get(3)?,
                default_quant: row.get(4)?,
                llama_bin: row.get(5)?,
                default_dpi: row.get(6)?,
                default_max_tokens: row.get(7)?,
                default_prompt: row.get(8)?,
                idle_unload_minutes: row.get(9)?,
            })
        },
    ) {
        Ok(s) => Ok(s),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Settings::default()),
        Err(e) => Err(format!("could not read settings: {e}")),
    }
}

/// Upsert the singleton settings row (INSERT OR REPLACE on id = 1).
fn persist(conn: &Connection, s: &Settings) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO settings
           (id, mode, remote_base_url, remote_api_key, remote_model,
            default_quant, llama_bin, default_dpi, default_max_tokens,
            default_prompt, idle_unload_minutes)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            s.mode,
            s.remote_base_url,
            s.remote_api_key,
            s.remote_model,
            s.default_quant,
            s.llama_bin,
            s.default_dpi,
            s.default_max_tokens,
            s.default_prompt,
            s.idle_unload_minutes,
        ],
    )
    .map_err(|e| format!("could not save settings: {e}"))?;
    Ok(())
}

// --- public accessors (the command-facing surface) --------------------------

/// Load settings, falling back to defaults on a missing row or a DB error.
pub fn load_settings() -> Settings {
    let mut s = crate::db::with_db(fetch).unwrap_or_else(|e| {
        eprintln!("[settings] load failed, using defaults: {e}");
        Settings::default()
    });

    // Check if there is an old plaintext key in the database
    if !s.remote_api_key.is_empty()
        && s.remote_api_key != "__saved__"
        && s.remote_api_key != "••••••••"
    {
        let key = s.remote_api_key.clone();
        if let Ok(entry) = keyring::Entry::new("unlocr", "remote_api_key") {
            if entry.set_password(&key).is_ok() {
                // Clear plaintext from DB, mark as __saved__
                s.remote_api_key = "__saved__".to_string();
                if let Err(e) = crate::db::with_db(|c| persist(c, &s)) {
                    eprintln!("[settings] failed to clear plaintext api key from database: {e}");
                }
            }
        }
    }

    // Return a masked password to the UI if a key is saved, else empty
    if s.remote_api_key == "__saved__" {
        s.remote_api_key = "••••••••".to_string();
    } else {
        s.remote_api_key = String::new();
    }

    s
}

/// Persist settings (upsert the singleton row).
pub fn save_settings(settings: &Settings) -> Result<(), String> {
    let mut s = settings.clone();
    let key = std::mem::take(&mut s.remote_api_key); // Clear key from settings to save in DB as empty string or placeholder

    if key == "••••••••" {
        // Kept as is! No change to keyring, preserve the "__saved__" marker in DB
        s.remote_api_key = "__saved__".to_string();
    } else if key.is_empty() {
        // Deleted! Remove from keyring, clear marker in DB
        if let Ok(entry) = keyring::Entry::new("unlocr", "remote_api_key") {
            let _ = entry.delete_password();
        }
        s.remote_api_key = String::new();
    } else {
        // Modified/new key! Save to keyring, set marker in DB
        if let Ok(entry) = keyring::Entry::new("unlocr", "remote_api_key") {
            entry
                .set_password(&key)
                .map_err(|e| format!("failed to save API key to OS credential manager: {e}"))?;
        } else {
            return Err("OS credential manager not available".to_string());
        }
        s.remote_api_key = "__saved__".to_string();
    }

    crate::db::with_db(|c| persist(c, &s))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fresh in-memory DB with the schema applied.
    fn mem_db() -> Connection {
        crate::db::mem_db()
    }

    /// A fresh store has no settings row -> fetch returns defaults, including the
    /// prefilled llama-server remote URL.
    #[test]
    fn missing_row_returns_default() {
        let conn = mem_db();
        let s = fetch(&conn).unwrap();
        assert_eq!(s.mode, "local");
        assert_eq!(s.remote_base_url, default_remote_base_url());
        assert_eq!(s.default_quant, OcrOptions::default().quant);
        assert_eq!(s.default_dpi, OcrOptions::default().dpi);
        assert_eq!(s.idle_unload_minutes, 15);
        assert!(s.remote_api_key.is_empty());
    }

    /// Write non-defaults, read them back: every field round-trips through the row.
    #[test]
    fn upsert_then_get_roundtrips() {
        let conn = mem_db();
        let s = Settings {
            mode: "remote".into(),
            remote_base_url: "http://gpu:8000".into(),
            remote_api_key: "sk-secret".into(),
            remote_model: "baidu/Unlimited-OCR".into(),
            default_quant: "Q4_K_M".into(),
            llama_bin: "/opt/llama-server".into(),
            default_dpi: 300,
            default_max_tokens: 8192,
            default_prompt: "<|x|>".into(),
            idle_unload_minutes: 5,
        };
        persist(&conn, &s).unwrap();
        let got = fetch(&conn).unwrap();
        assert_eq!(got.mode, "remote");
        assert_eq!(got.remote_base_url, "http://gpu:8000");
        assert_eq!(got.remote_api_key, "sk-secret");
        assert_eq!(got.remote_model, "baidu/Unlimited-OCR");
        assert_eq!(got.default_quant, "Q4_K_M");
        assert_eq!(got.llama_bin, "/opt/llama-server");
        assert_eq!(got.default_dpi, 300);
        assert_eq!(got.default_max_tokens, 8192);
        assert_eq!(got.default_prompt, "<|x|>");
        assert_eq!(got.idle_unload_minutes, 5);
    }

    /// The settings table is a singleton: a second save replaces, never adds a row.
    #[test]
    fn upsert_is_singleton() {
        let conn = mem_db();
        persist(&conn, &Settings::default()).unwrap();
        persist(&conn, &Settings::default()).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 1, "settings must hold exactly one row (id = 1)");
    }

    /// The remote base URL prefills to llama-server's default host:port.
    #[test]
    fn default_remote_url_is_llama_default() {
        assert_eq!(Settings::default().remote_base_url, "http://127.0.0.1:8080");
    }

    /// camelCase on the wire (regression guard for the serde rename the frontend
    /// depends on; the command return serializes this struct directly).
    #[test]
    fn serializes_camel_case() {
        let json = serde_json::to_string(&Settings::default()).unwrap();
        assert!(json.contains("\"remoteBaseUrl\""));
        assert!(json.contains("\"defaultMaxTokens\""));
        assert!(json.contains("\"idleUnloadMinutes\""));
        assert!(!json.contains("\"remote_base_url\""));
    }
}
