use serde::{Deserialize, Serialize};

/// One OCR run as the Library/Board UI renders it. Field names are camelCase on
/// the wire so the JS side reads `job.inputPath`, `job.outputPath`, etc. without
/// a rename layer. `options` mirrors the `OcrOptions` the run actually used.
///
/// Status is a coarse string (queued/running/done/failed) rather than an enum on
/// the wire so a future status value does not break older frontends. The UI groups
/// by this string into Board columns.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    /// Stable id. `<unix_secs>-<input-stem>-<path_hash>-<seq>`: the path hash
    /// disambiguates same-stem inputs from different folders, and the per-process
    /// seq makes two runs of the same file in the same second distinct.
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
