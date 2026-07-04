# Changelog

## v0.1.0 (2026-07-04)

Initial public release of unlocr -- OCR PDFs to markdown via the Unlimited-OCR (DeepSeek-OCR) model running locally through llama.cpp.

### CLI

- Pure Rust CLI with clap argument parsing, batch PDF/image input (folders, globs, --from-list), and recursive directory scanning.
- Persistent llama-server background process: model loads once per session, no reload overhead across pages.
- Managed local GGUF path: auto-downloads the quantized model (BF16/Q8_0/Q4_K_M) from Hugging Face, cached per-OS.
- Remote GPU path: OpenAI-compatible endpoint for vLLM/SGLang serving the full-precision model.
- Auto-download of unlocr's patched R-SWA llama-server build (PR #24975, unmerged upstream), with external binary fallback.
- Streaming token output as pages are processed, with repetition-loop detection (finish_reason, DRY sampler).
- Preflight checks and `doctor` diagnostics (per-platform installation hints).
- Secure input validation: quant path traversal and injection guards.

### GUI (Tauri 2 Desktop App)

- Native desktop frontend wrapping the full OCR pipeline via Tauri commands.
- Workspace with Library grid, Kanban board, and file rail views.
- PDF preview pane with page-by-page navigation and zoom controls.
- Markdown review pane (EasyMDE editor) with diff between consecutive runs.
- Export to DOCX/ODT/RTF/HTML/TXT via pandoc.
- Settings panel: engine/provider/privacy/routing configuration, dependency management.
- Streaming per-token transcript rendered live during OCR.
- Batch sequential processing for multi-file imports.
- Quick-settings popup, library multi-select, folder-scan dialog.
- Internationalization: English, Chinese (zh), Japanese (ja), Korean (ko).
- System Requirements panel: RAM/CPU/disk/GPU probes with localized metrics.
- On-demand tool downloader for managecd llama-server + pandoc (per-platform).

### Backend

- Modular Rust architecture: unlocr library crate with clap-free run_ocr_job/OcrOptions/Progress API.
- SQLite-backed jobs store (rusqlite) replacing ad-hoc JSON files.
- Keyring integration for remote endpoint API keys.
- Image file support (PNG/JPG/WEBP/BMP) as direct OCR input alongside PDF.
- Per-page rasterization cache for PDF preview.

### CI & Packaging

- Automated release pipeline: CLI binaries (4 targets), GUI bundles (dmg/msi/AppImage/deb/rpm), CLI deb/rpm.
- Build-llama workflow: cross-platform patched R-SWA llama-server builds (macOS/Windows/Linux x86_64+arm64).
- Homebrew formula (CLI) and cask (GUI) with auto-update workflow.
- crates.io publishing, cargo-deny + cargo-audit gates.
- Makefile targets: build, test, install, deb, rpm, dist.
- Cross-platform install scripts (install.sh, install.ps1) with Homebrew recommendations.
