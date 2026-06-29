# unlocr.db schema

The SQLite backing for the GUI's persisted state: the OCR job history (Library +
Board views), app settings, and the bell-panel notifications. It replaces the
three hand-rolled JSON files (`jobs.json`, `settings.json`, `notifications.json`)
that previously lived under the GGUF cache dir.

This is a single combined reference: the authoritative DDL lives in
`gui/src-tauri/src/db.rs` (`SCHEMA_SQL`); this document mirrors and explains it.

## Location

`<app-data>/unlocr/unlocr.db`. Resolved by `db::app_data_dir()` (env-var based,
no Tauri handle required so the idle-unload watcher thread can read settings
without an `AppHandle`):

| OS      | Path                                                    |
|---------|---------------------------------------------------------|
| macOS   | `~/Library/Application Support/unlocr/unlocr.db`        |
| Linux   | `$XDG_DATA_HOME/unlocr/unlocr.db` (else `~/.local/share/unlocr/unlocr.db`) |
| Windows | `%APPDATA%\unlocr\unlocr.db`                            |

WAL journal mode produces two SQLite-internal sidecar files next to the DB:
`unlocr.db-wal` and `unlocr.db-shm`. These are auto-managed (checkpointed on a
clean exit, recovered on open) and need no handling.

## Engine + versioning

- **Engine:** SQLite via `rusqlite` with the `bundled` feature, which compiles
  `libsqlite3` into the GUI binary. There is no runtime/system SQLite dependency.
- **Access:** one process-wide `Connection` behind a `Mutex` (`db::DB`), reached
  through `db::with_db(|conn| ...)`. Opened once in `run()`'s `.setup()`; an open
  failure aborts startup (a GUI that cannot open its store is broken, not empty).
- **Schema version:** tracked by `PRAGMA user_version` (currently `1`), replacing
  the old per-JSON-file `version: 1` envelope. Bump it and add a migration step
  in `db::init_conn` when the schema changes.
- **Fresh start:** the legacy `*.json` files in the cache dir are NOT migrated.
  They are orphaned (harmless) and may be deleted.
- **No caps:** history grows unbounded. The old 500-job / 200-notification limits
  are gone.

### Rust → SQLite type map

| Rust              | SQLite                                            |
|-------------------|---------------------------------------------------|
| `String`          | `TEXT NOT NULL` (default `''` for optional fields)|
| `u32`             | `INTEGER NOT NULL`                                |
| `u64` (epoch secs)| `INTEGER NOT NULL` (cast `u64 ↔ i64` at the boundary; rusqlite has no `u64` (To/From)Sql) |
| `bool`            | `INTEGER NOT NULL DEFAULT 0 CHECK(col IN (0,1))`  |

`JobOptions` is flattened into `jobs` columns (index-friendly, no JSON column).

## Tables

### `jobs`

One OCR run. Written by `record_job` after each `run_ocr` outcome; read by the
Library (all jobs) and the Board (grouped by `status`), and by
`allowed_output_paths` to authorize `read_text_file` on past runs' `.md` output.

| Column        | Type    | Notes                                             |
|---------------|---------|---------------------------------------------------|
| `id`          | TEXT PK | `make_id` output: `<secs>-<stem>-<hash16>`        |
| `input_path`  | TEXT    | source PDF path, as passed to `run_ocr`           |
| `quant`       | TEXT    | flattened `JobOptions.quant`                      |
| `max_tokens`  | INTEGER | flattened `JobOptions.max_tokens` (`u32`)         |
| `dpi`         | INTEGER | flattened `JobOptions.dpi` (`u32`)                |
| `prompt`      | TEXT    | flattened `JobOptions.prompt`                     |
| `keep_images` | INTEGER | flattened `JobOptions.keep_images` (`bool`, 0/1)  |
| `status`      | TEXT    | `queued` \| `running` \| `done` \| `failed`       |
| `output_path` | TEXT    | written `{stem}.md`; `''` when in-memory only     |
| `error`       | TEXT    | error text when `failed`; `''` otherwise          |
| `created_at`  | INTEGER | unix epoch seconds (`u64`)                        |
| `updated_at`  | INTEGER | unix epoch seconds; equals `created_at` today     |

Indexes: `idx_jobs_created_at` on `created_at DESC` (the Library's newest-first
sort) and `idx_jobs_status` on `status` (Board grouping).

### `settings`

A singleton row (`id = 1`). Read by the Settings panel and by the idle-unload
watcher (every 60s, for `idle_unload_minutes`). A missing row yields
`Settings::default()`; `save_settings` upserts the single row.

| Column               | Type    | Notes                                              |
|----------------------|---------|----------------------------------------------------|
| `id`                 | INT PK  | `CHECK(id = 1)` — singleton                        |
| `mode`               | TEXT    | `local` \| `remote`; default `local`               |
| `remote_base_url`    | TEXT    | OpenAI-compatible base URL; default `http://127.0.0.1:8080` |
| `remote_api_key`     | TEXT    | bearer token, **plaintext** (same trust model as before; OS keychain is the upgrade path) |
| `remote_model`       | TEXT    | model name for multi-model gateways                |
| `default_quant`      | TEXT    | default quantization tier                          |
| `llama_bin`          | TEXT    | explicit `llama-server` path; `''` = resolve via PATH/Homebrew |
| `default_dpi`        | INTEGER | `u32`                                              |
| `default_max_tokens` | INTEGER | `u32`                                              |
| `default_prompt`     | TEXT    | default OCR prompt                                 |
| `idle_unload_minutes`| INTEGER | `u32`; `0` disables idle-unload; default `15`      |

### `notifications`

Bell-panel events (terminal run outcomes, download completions). Returned in
insertion order (newest last); the frontend reverses for display.

| Column       | Type    | Notes                                              |
|--------------|---------|----------------------------------------------------|
| `id`         | TEXT PK | `<unix_secs>-<seq>` (`next_id`; unique within a second) |
| `kind`       | TEXT    | `done` \| `error` \| `download` \| `info`          |
| `title`      | TEXT    | one-line headline                                  |
| `body`       | TEXT    | detail; `''` when none                             |
| `created_at` | INTEGER | unix epoch seconds (`u64`)                         |
| `read`       | INTEGER | `bool` (0/1); new rows start unread                |

Index: `idx_notifications_created_at` on `created_at DESC`.

## Build notes

The `bundled` feature compiles the SQLite C amalgamation via `cc`, so building
the GUI crate requires a C compiler on the host. The standard GitHub Actions
runners (`ubuntu-latest`, `macos-latest`, `windows-latest`) ship one; on a
minimal image install `build-essential` (Ubuntu) or ensure MSVC build tools
(Windows). No `RUSTFLAGS` or `CC` override is needed on the standard runners,
and there is no runtime SQLite dependency to declare in deb/rpm.
