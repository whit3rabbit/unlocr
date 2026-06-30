// unlocr library: the OCR backend as callable functions, independent of the
// clap-based CLI. The binary crate (main.rs) and the Tauri host both build on
// top of this. Keeping clap out of here is load-bearing: the GUI needs to drive
// OCR with plain typed params and a progress sink, no Args/argv in sight.

/// Model management and caching utilities.
pub mod model;
/// PDF rendering and processing utilities.
pub mod pdf;
/// System check and preflight diagnostics.
pub mod preflight;
/// OCR server management.
pub mod server;
/// Tool resolution and downloading utilities.
pub mod tools;

mod job;
mod options;
mod output;
mod preview;

pub use job::*;
pub use options::*;
pub use output::*;
pub use preview::*;

// Note: ocr.rs is intentionally NOT a lib module here. It is bin-only CLI glue
// (`run_pdf(&Args)`) that converts the clap Args into the clap-free OcrOptions
// below and delegates the rasterize+OCR loop to `ocr_pages`. With bite 2 done,
// ocr.rs no longer has its own push_page; the lib's `push_page` below is the
// single canonical page-assembly implementation used by both paths.

/// Result type alias with a dynamic error type.
pub type Res<T> = Result<T, Box<dyn std::error::Error>>;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
