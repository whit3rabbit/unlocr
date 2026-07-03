// OCR run + file IO commands entry point.
// Coordinates batch OCR and delegates specific file, tool, and render logic to submodules.

mod fs;
mod ocr;
mod render;
mod scan;
mod tools;

pub(crate) use fs::{export_markdown, read_text_file, write_text_file};
pub(crate) use ocr::run_ocr;
pub(crate) use render::{pdf_info, render_page, render_pages};
pub(crate) use scan::scan_input_paths;
pub(crate) use tools::{brew_available, brew_install, download_tool, host_os, list_tools};
