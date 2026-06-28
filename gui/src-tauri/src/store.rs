//! Persisted OCR job store (EH-0006 bite 1).
//!
//! Records each `run_ocr` job to a JSON file under the model cache dir so the
//! Library grid and Workflow board can render past runs across app restarts. The
//! card explicitly allows "a JSON file under the model cache dir" as the backing
//! store, which keeps this dependency-free (no `tauri-plugin-store` dep to add)
//! and co-located with the model cache the backend already resolves.
//!
//! Scope: persistence + typed accessors only. The Library/Board UI views and the
//! drag-drop importer land in later bites on this card. This module is purely
//! additive: it touches no existing command and changes no OCR behavior.
//!
//! Schema is append-only by job id; writes re-serialize the whole file (the job
//! list is small, one record per run, so a full rewrite is cheap and avoids a
//! hand-rolled append-only JSONL parser). A corrupt/missing file is treated as
//! "no jobs yet" so a first launch or a user-deleted file never blocks the app.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use unlocr::model::cache_dir;

/// Filename of the job store inside the model cache dir. Co-located with the
/// GGUFs so a single cache dir holds everything the app persists.
const STORE_FILE: &str = "jobs.json";

/// Cap on retained jobs. The store is rewritten in full on every record and the
/// Library/Board render every job as a DOM node, so an uncapped history grows both
/// the file-rewrite cost and the webview node count without bound. Keep the most
/// recent N (insertion-ordered, so the tail is newest).
const MAX_JOBS: usize = 500;

/// One OCR run as the Library/Board UI renders it. Field names are camelCase on
/// the wire so the JS side reads `job.inputPath`, `job.outputPath`, etc. without
/// a rename layer. `options` mirrors the `OcrOptions` the run actually used.
///
/// Status is a coarse string (queued/running/done/failed) rather than an enum on
/// the wire so a future status value does not break older frontends parsing the
/// file. The UI groups by this string into Board columns.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    /// Stable id. `<unix_secs>-<input-stem>` is unique enough for a local store
    /// (collisions only if two identical-stem runs start in the same second).
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

/// On-disk envelope. `version` lets a future migration detect an older schema and
/// transform it instead of silently dropping records.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct JobsFile {
    version: u32,
    jobs: Vec<Job>,
}

impl Default for JobsFile {
    fn default() -> Self {
        Self {
            version: 1,
            jobs: Vec::new(),
        }
    }
}

/// Resolve the store path: `<model cache dir>/jobs.json`. The cache dir is the
/// same one the backend resolves for the GGUFs (`unlocr::model::cache_dir`), so
/// the store rides along wherever the user's cache lives. Public so a command can
/// report the path to the UI (e.g. for an acceptance "cat the file" check).
///
/// Returns Ok even though `cache_dir` is fallible: we surface its error to the
/// caller so a command can decide whether to fail loud or degrade.
pub fn store_path() -> Result<PathBuf, String> {
    let cache = cache_dir(None).map_err(|e| format!("could not resolve model cache dir: {e}"))?;
    Ok(cache.join(STORE_FILE))
}

/// Load all jobs from the store. A missing file means "no jobs yet" (first run);
/// a corrupt file is logged and treated as empty so a bad state never wedges the
/// UI. The caller owns ordering; jobs are returned in file order (insertion order
/// on the happy path).
pub fn load_jobs() -> Vec<Job> {
    let path = match store_path() {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<JobsFile>(&bytes) {
            Ok(file) => file.jobs,
            Err(e) => {
                // Corrupt store: do not panic, do not delete the user's file. Log
                // to stderr and present as empty so the app stays usable; a future
                // repair command could migrate or recover it.
                eprintln!("[store] jobs.json parse failed, treating as empty: {e}");
                Vec::new()
            }
        },
        Err(_) => Vec::new(),
    }
}

/// Persist the full job list, creating the cache dir if needed. Atomic-ish: write
/// to `<file>.tmp` then rename, so a crash mid-write cannot truncate the store.
pub fn save_jobs(jobs: &[Job]) -> Result<(), String> {
    let path = store_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create store dir {}: {e}", parent.display()))?;
    }
    let file = JobsFile {
        version: 1,
        jobs: crate::jsonstore::cap_to_recent(jobs.to_vec(), MAX_JOBS),
    };
    let bytes =
        serde_json::to_vec_pretty(&file).map_err(|e| format!("could not serialize jobs: {e}"))?;
    crate::jsonstore::write_atomic(&path, &bytes)
}

/// Append-or-update by id, then persist. Used by both the "record a fresh run"
/// command (insert) and a future "mark a queued job done" update. Returns the
/// updated job so the caller can echo it to the frontend.
fn upsert(job: Job) -> Result<Job, String> {
    let mut jobs = load_jobs();
    if let Some(existing) = jobs.iter_mut().find(|j| j.id == job.id) {
        *existing = job.clone();
    } else {
        jobs.push(job.clone());
    }
    save_jobs(&jobs)?;
    Ok(job)
}

/// Current unix epoch seconds. Factored out so the `record_job` command and any
/// status-update path share one clock. Falls back to 0 if the system clock is
/// before the epoch (essentially impossible; preserves determinism).
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Derive a stable job id from the start time, the input file stem, and a short
/// hash of the FULL input path. The path hash disambiguates two same-stem inputs
/// from different folders recorded in the same second (e.g. two `report.pdf`),
/// which the old `<secs>-<stem>` scheme collided on, silently overwriting one
/// record via upsert. A genuine same-file re-run in the same second still collides
/// by design (the worst case is a re-run overwriting itself, never corruption).
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
    format!("{created_at}-{clean}-{:04x}", h.finish() & 0xffff)
}

// --- command-facing helpers ------------------------------------------------

/// Record a job in its terminal state (done or failed). This is the shape the
/// frontend calls right after a `run_ocr` invocation returns or throws: it builds
/// a `Job` with the run's outcome and persists it. Returns the stored job so the
/// caller can push it into the in-memory list without a reload.
///
/// Bites 2/3 (Library/Board views) call `load_jobs` to read; bite 4 (drag-drop)
/// calls this after enqueueing a run.
pub fn record_outcome(
    input_path: &str,
    options: JobOptions,
    status: &str,
    output_path: &str,
    error: &str,
) -> Result<Job, String> {
    let created_at = now_secs();
    let updated_at = created_at;
    let job = Job {
        id: make_id(input_path, created_at),
        input_path: input_path.to_string(),
        options,
        status: status.to_string(),
        output_path: output_path.to_string(),
        error: error.to_string(),
        created_at,
        updated_at,
    };
    upsert(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip the envelope through serde so a schema change that breaks
    /// (de)serialization is caught here, not at first app launch.
    #[test]
    fn job_file_roundtrips() {
        let job = Job {
            id: "1-sample".into(),
            input_path: "/tmp/sample.pdf".into(),
            options: JobOptions::from_opts("Q8_0", 4096, 144, "prompt", false),
            status: "done".into(),
            output_path: "/tmp/sample.md".into(),
            error: "".into(),
            created_at: 100,
            updated_at: 200,
        };
        let file = JobsFile {
            version: 1,
            jobs: vec![job.clone()],
        };
        let json = serde_json::to_string(&file).unwrap();
        let back: JobsFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, 1);
        assert_eq!(back.jobs.len(), 1);
        assert_eq!(back.jobs[0].id, "1-sample");
        assert_eq!(back.jobs[0].input_path, "/tmp/sample.pdf");
        assert_eq!(back.jobs[0].status, "done");
        // camelCase on the wire (regression guard for the serde rename).
        assert!(
            json.contains("\"inputPath\""),
            "expected camelCase inputPath"
        );
        assert!(json.contains("\"outputPath\""));
        assert!(json.contains("\"maxTokens\""));
        assert!(json.contains("\"keepImages\""));
        assert!(!json.contains("\"input_path\""));
    }

    /// `make_id` must be filesystem-safe and stable for the same input/time.
    #[test]
    fn make_id_is_safe_and_stable() {
        let a = make_id("/some path/My Report #1.pdf", 12345);
        let b = make_id("/some path/My Report #1.pdf", 12345);
        assert_eq!(a, b, "same input+time must be stable");
        // ASCII-only fs-safe: is_ascii_alphanumeric, not the Unicode is_alphanumeric.
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

    /// The cap keeps exactly MAX_JOBS, drops the OLDEST, and keeps the newest tail.
    /// Pure helper, so no cache dir is touched.
    #[test]
    fn cap_to_recent_keeps_newest_tail() {
        let mk = |i: u64| Job {
            id: format!("{i}-x"),
            input_path: format!("/tmp/{i}.pdf"),
            options: JobOptions::from_opts("Q8_0", 4096, 144, "p", false),
            status: "done".into(),
            output_path: String::new(),
            error: String::new(),
            created_at: i,
            updated_at: i,
        };
        // Under the cap: unchanged.
        let few: Vec<Job> = (0..10).map(mk).collect();
        assert_eq!(crate::jsonstore::cap_to_recent(few, MAX_JOBS).len(), 10);

        // Over the cap: trimmed to MAX_JOBS, oldest dropped, newest kept. (The cap
        // logic itself is covered in jsonstore; this asserts MAX_JOBS is wired.)
        let many: Vec<Job> = (0..(MAX_JOBS as u64 + 50)).map(mk).collect();
        let capped = crate::jsonstore::cap_to_recent(many, MAX_JOBS);
        assert_eq!(capped.len(), MAX_JOBS, "must trim to the cap");
        assert_eq!(capped.first().unwrap().created_at, 50, "oldest 50 dropped");
        assert_eq!(
            capped.last().unwrap().created_at,
            MAX_JOBS as u64 + 49,
            "newest kept at the tail"
        );
    }

    /// now_secs is monotonic-ish and non-zero on a normal clock.
    #[test]
    fn now_secs_is_positive() {
        let n = now_secs();
        assert!(n > 1_700_000_000, "epoch seconds implausibly small: {n}");
    }
}
