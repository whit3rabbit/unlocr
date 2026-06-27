# unlocr

OCR PDFs to markdown using the [Unlimited-OCR](https://huggingface.co/sahilchachra/Unlimited-OCR-GGUF)
model (DeepSeek-OCR architecture, 3B VLM) via [llama.cpp](https://github.com/ggml-org/llama.cpp).

A thin Rust wrapper: it rasterizes each PDF page to PNG with `pdftoppm`, runs a
single persistent `llama-server` (model loaded once), and asks it to convert
each page to markdown. The model and projector GGUFs are downloaded from Hugging
Face on first run and cached.

## Requirements

- **llama.cpp build >= b8530** (PR [#17400](https://github.com/ggml-org/llama.cpp/pull/17400),
  "mtmd: Add DeepSeekOCR Support", merged 2026-03-25). Older builds cannot load
  the model.
- **poppler** (provides `pdftoppm`).
- Rust toolchain to build.

### macOS

```bash
brew install llama.cpp poppler
```

`unlocr` finds `llama-server` and `pdftoppm` on `PATH` or in the Homebrew prefixes
(`/opt/homebrew/bin`, `/usr/local/bin`). Override the server with `--llama-bin`.

> Linux/Windows: the same two binaries must be installed and on `PATH`. Install
> hints in the tool are macOS-specific for now and the non-macOS paths are
> unverified.

## Install

Prebuilt binaries for each release are attached to the
[GitHub Releases](../../releases) page (macOS arm64/x64, Linux x64 musl, Windows
x64). Download, extract, put `unlocr` on your `PATH`.

Build and install from source instead:

```bash
./install.sh                 # macOS/Linux: build + install to /usr/local/bin + dep check
PREFIX=$HOME/.local ./install.sh
```

Windows: `powershell -ExecutionPolicy Bypass -File packaging\windows\install.ps1`.
Distro packages (.deb/.rpm): see [packaging/README.md](packaging/README.md).

## Uninstall

```bash
./uninstall.sh               # removes the binary AND the model cache (see below)
```

Windows: `powershell -ExecutionPolicy Bypass -File packaging\windows\uninstall.ps1`.
Neither touches llama.cpp or poppler. (`make uninstall` and removing a .deb/.rpm
delete the binary only, never the cache.)

## Build

```bash
cargo build --release
# binary: target/release/unlocr  (~1.4 MB)
```

## Usage

```bash
unlocr <input.pdf> [more.pdf ...] [options]
```

| Option            | Default                                          | Notes |
|-------------------|--------------------------------------------------|-------|
| `--out DIR`       | `.`                                              | Output dir for `<stem>.md` |
| `--quality TIER`  | `good`                                           | Alias for a quant: `best`=BF16 (5.47GB), `good`=Q8_0 (2.91GB), `less`=Q4_K_M (1.82GB) |
| `--quant TAG`     | (from quality)                                   | Exact quant, e.g. `Q6_K`, `IQ4_XS`. Picks `Unlimited-OCR-<TAG>.gguf`. Overrides `--quality` |
| `--max-tokens N`  | `4096`                                           | Cap generated tokens per page (bounds runaway generation on dense pages) |
| `--prompt TEXT`   | `<\|grounding\|>Convert the document to markdown.` | `<\|grounding\|>` makes the model emit layout bbox coords; drop it for plain markdown |
| `--dpi N`         | `144`                                            | pdftoppm rasterization DPI |
| `--llama-bin P`   | auto                                             | Path to `llama-server` |
| `--model-dir P`   | per-OS cache                                      | Where GGUFs are stored |
| `--port N`        | `0` (auto)                                        | llama-server port |
| `--keep-images`   | off                                              | Keep the intermediate page PNGs |

### Example

```bash
unlocr report.pdf --out ./out --quality best
# -> ./out/report.md, page-delimited with <!-- page N --> markers
```

## Cache location

GGUFs are downloaded from Hugging Face on first run and cached. The set is large
(BF16 alone ~5.5G; all three quants + projector up to ~8G), so know where it goes:

| OS | Default path |
|----|--------------|
| macOS | `~/Library/Caches/unlocr` |
| Linux | `$XDG_CACHE_HOME/unlocr`, else `~/.cache/unlocr` |
| Windows | `%LOCALAPPDATA%\unlocr` |

`$XDG_CACHE_HOME` (if set) wins on every OS. Override entirely with `--model-dir P`.
To reclaim the space, run `./uninstall.sh` (or `uninstall.ps1` on Windows), or
just delete the directory above.

## Benchmark (unofficial)

> Single run, one machine, one document. Not a rigorous benchmark, just a rough
> sense of throughput. Numbers will vary with hardware, page density, and DPI.

- **Machine:** macOS, Apple Silicon, Metal backend
- **llama.cpp:** build b9770
- **Model:** BF16 (`--quality best`, 5.47GB)
- **Document:** 355-page book PDF (~12.4MB), 144 DPI

| Phase | Result |
|-------|--------|
| One-time model download | ~9 min (Hugging Face throttled, 5-30 MB/s, highly variable) |
| Cold start (load + 5 pages, model cached) | ~15 s |
| Full 355-page run | **42 min 44 s** |
| Average throughput | ~7.2 s/page (~8.3 pages/min) |
| Output | 1.2 MB markdown, ~192k words, all 355 pages |

Mid-document steady state was ~4 s/page; a handful of very dense pages (index /
references) ran 1-3 min each and pulled the average up. `--max-tokens` (default
4096) bounds that worst case. Smaller quants (`--quality good`/`less`) trade some
fidelity for speed and a smaller download.

See [Cache location](#cache-location) for where the GGUFs land and how to
reclaim the space.

## How it works

1. **Preflight** — locate `llama-server` + `pdftoppm`; parse `llama-server --version`
   and warn if the build is below b8530.
2. **Model cache** — download `Unlimited-OCR-<quant>.gguf` and
   `mmproj-Unlimited-OCR-F16.gguf` if missing.
3. **Server** — start one `llama-server`, wait for `/health`. If the model fails
   to load (e.g. too-old build) the captured stderr is surfaced with an upgrade
   hint.
4. **OCR** — per PDF: `pdftoppm -png` the pages, POST each as a base64 image to
   `/v1/chat/completions`, concatenate the markdown. Pages run sequentially
   (one image in memory at a time); the model stays loaded the whole time.

## Limitations

- Ctrl-C (SIGINT) does not run cleanup, so it may orphan `llama-server`.
- Free-port selection has a small race; pass `--port` to pin it.
