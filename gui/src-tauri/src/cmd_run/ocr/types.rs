use serde::Serialize;

/// Serializable payload for the `ocr://page` event.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PageProgress {
    pub(crate) page: usize,
    pub(crate) total: usize,
}

/// Serializable payload for the `ocr://rasterizing` event. `total` is `None`
/// when the run's page count wasn't known upfront (whole-doc run with no
/// resolvable `pdfinfo`); the frontend shows a running count with no
/// denominator in that case.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RasterizeProgress {
    pub(crate) page: usize,
    pub(crate) total: Option<usize>,
}

/// Payload for the `ocr://partial-text` event.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PartialText {
    pub(crate) page: usize,
    pub(crate) chunk: String,
}

/// Payload for the terminal `ocr://done` event.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OcrDone {
    pub(crate) markdown: String,
}

/// Payload for `ocr://images-kept`.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImagesKept {
    pub(crate) dir: String,
}

/// Payload for `ocr://status`: a one-line message for a long, event-less phase.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatusMsg {
    pub(crate) message: String,
}

/// Return value of `run_ocr`: every written file path (combined file first in
/// single/both; first page file in pages) plus the in-memory combined markdown.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunResult {
    pub(crate) paths: Vec<String>,
    pub(crate) combined: String,
}
