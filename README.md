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
- Rust toolchain (only needed to build from source / `cargo install`).

### System requirements by model

The default path runs a **quantized GGUF** on `llama.cpp` (CPU, or GPU with a
CUDA/Metal/Vulkan `llama-server` build). Optionally you can run the **full,
unquantized** DeepSeek-OCR model on a GPU via **vLLM** (see
[Run the full model on GPU](#run-the-full-model-on-gpu-vllm)).

| Mode | Variant | Download | Memory | Engine |
|------|---------|----------|--------|--------|
| GGUF (default) | `Q4_K_M` (`--quality less`) | 1.82 GB | ~4 GB RAM | llama.cpp |
| GGUF | `Q8_0` (`--quality good`, default) | 2.91 GB | ~6 GB RAM | llama.cpp |
| GGUF | `BF16` (`--quality best`) | 5.47 GB | ~8 GB RAM | llama.cpp |
| Full model (GPU) | [`deepseek-ai/DeepSeek-OCR`](https://huggingface.co/deepseek-ai/DeepSeek-OCR) | ~6.7 GB | **16 GB VRAM min, 24 GB+ recommended** | vLLM |

GGUF memory figures are rough working-set estimates (model + projector + KV
cache); a GGUF build of `llama-server` with GPU offload uses VRAM in place of
RAM. The full-model VRAM figures and supported GPUs (L4 / A100 / H100) are from
the [DeepSeek-OCR model card](https://huggingface.co/deepseek-ai/DeepSeek-OCR).

> **License:** the `unlocr` code is MIT (see [LICENSE](LICENSE)). The model weights
> it downloads ([Unlimited-OCR](https://huggingface.co/sahilchachra/Unlimited-OCR-GGUF),
> DeepSeek-OCR architecture) carry their **own** license; see the model card. MIT
> covers this tool, not the weights.

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

There are two products: the **CLI** (`unlocr`) and a **desktop GUI** app. Most
users want the GUI; the CLI is for scripting and batch jobs.

### Homebrew (macOS / Linux)

```bash
brew install whit3rabbit/tap/unlocr          # CLI
brew install --cask whit3rabbit/tap/unlocr   # GUI app (macOS)
```

The formula installs `poppler` as a dependency and recommends `brew install llama.cpp`.

### cargo (CLI)

```bash
cargo install unlocr
```

### GitHub Releases

Each release attaches **CLI** binaries (macOS arm64/x64, Linux x64 musl, Windows
x64; download, extract, put `unlocr` on your `PATH`) and **GUI** installers
(`.dmg`, `.msi`, `.AppImage`, `.deb`) on the
[Releases](../../releases) page.

> **Unsigned macOS GUI:** the `.app`/`.dmg` are not signed or notarized yet. If
> macOS blocks the app on first launch, run
> `xattr -dr com.apple.quarantine "/Applications/unlocr.app"` or right-click the
> app and choose Open. (The Homebrew cask handles this for you.)

### From source

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
| `--task PRESET`   | `markdown`                                        | Prompt preset: `markdown` (grounded markdown), `free` (plain text), `figure` (parse a chart/figure). Ignored when `--prompt` is set |
| `--prompt TEXT`   | (from `--task`)                                   | OCR prompt, overrides `--task`. `<\|grounding\|>` makes the model emit layout bbox coords; drop it for plain markdown |
| `--dpi N`         | `144`                                            | pdftoppm rasterization DPI (pixel size of the PNG handed to the model) |
| `--image-max-tokens N` | model default                               | Vision-token budget per image (`llama-server --image-max-tokens`). DeepSeek-OCR's base/large detail knob: higher = finer recognition, slower + more VRAM. Independent of `--dpi` |
| `--chat-template NAME` | model default                               | Forwarded to `llama-server --chat-template` (e.g. `deepseek-ocr`) |
| `--repeat-penalty F` | server default                                | Sampling penalty (e.g. `1.1`); helps break the infinite-loop output some quants (notably Q4_K_M) hit on dense pages |
| `--llama-bin P`   | auto                                             | Path to `llama-server` |
| `--model-dir P`   | per-OS cache                                      | Where GGUFs are stored |
| `--port N`        | `0` (auto)                                        | llama-server port |
| `--keep-images`   | off                                              | Keep the intermediate page PNGs |
| `--gpu`           | off                                              | Run the full DeepSeek-OCR model on a local vLLM server (see [below](#run-the-full-model-on-gpu-vllm)). Shortcut for `--endpoint http://localhost:8000 --endpoint-model deepseek-ai/DeepSeek-OCR` |
| `--endpoint URL`  | (local spawn)                                     | OCR against a remote OpenAI-compatible server (vLLM/SGLang/remote llama.cpp) instead of spawning a local one. Skips the GGUF download |
| `--endpoint-key K`| `UNLOCR_API_KEY`                                  | Bearer token for `--endpoint` (prefer the env var to keep it out of the process list) |
| `--endpoint-model M` | (none)                                         | Model name sent in the request body; required by vLLM / litellm gateways |

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

## MLX on Apple Silicon (via LM Studio)

On Apple Silicon you can run inference on the MLX engine instead of llama.cpp.
unlocr does not bundle MLX: it talks to LM Studio's local OpenAI-compatible
server, which uses MLX (`mlx-vlm`) under the hood. The GUI has a one-click
**LM Studio (MLX)** preset; the CLI reaches it with `--endpoint http://localhost:1234`.

1. Install LM Studio (>= 0.3.4, which ships the MLX engine).
2. Download an MLX DeepSeek-OCR build, e.g. `mlx-community/DeepSeek-OCR-8bit`
   (or `-4bit` for less RAM).
3. Load it in LM Studio, then start the Local Server (binds `localhost:1234`).
4. In the unlocr GUI, click the **LM Studio (MLX)** preset (this flips to the
   Remote engine and fills `http://localhost:1234`; unlocr appends
   `/v1/chat/completions` itself), then Load and Run. The API key field can
   stay empty.

Leave the default `<|grounding|>Convert the document to markdown.` prompt: it is
the grounding format DeepSeek-OCR expects.

## Run the full model on GPU (vLLM)

The GGUF path runs on `llama.cpp`. To run the **full, unquantized** DeepSeek-OCR
model on a GPU (16 GB+ VRAM, see [requirements](#system-requirements-by-model)),
serve it with **vLLM** and point unlocr at that server. unlocr does not bundle
vLLM or a Python/CUDA runtime; it only sends OpenAI `/v1/chat/completions`
requests, so any vLLM serving DeepSeek-OCR works.

1. Install vLLM (the dev build; stock 0.11 does not yet support DeepSeek-OCR):

   ```bash
   pip install -U vllm --pre --extra-index-url https://wheels.vllm.ai/nightly
   ```

2. Serve the model (vLLM downloads the ~6.7 GB weights on first run):

   ```bash
   vllm serve deepseek-ai/DeepSeek-OCR \
     --no-enable-prefix-caching \
     --mm-processor-cache-gb 0 \
     --logits-processors vllm.model_executor.models.deepseek_ocr:NGramPerReqLogitsProcessor
   ```

3. Run unlocr against it. The `--gpu` shortcut fills in the local vLLM URL and the
   model name:

   ```bash
   unlocr report.pdf --gpu
   # equivalent to:
   unlocr report.pdf --endpoint http://localhost:8000 --endpoint-model deepseek-ai/DeepSeek-OCR
   ```

   In the GUI, pick the **GPU full model (vLLM · DeepSeek-OCR)** engine preset,
   then Load and Run.

**No GPU? Use Google Colab.** The [`colab/unlocr-deepseek-ocr-gpu.ipynb`](colab/unlocr-deepseek-ocr-gpu.ipynb)
notebook installs the prerequisites, serves the model, and runs the unlocr binary
end to end on a free/cheap Colab GPU (T4/L4/A100). Open it in Colab, pick a GPU
runtime, and run the cells.

## Limitations

- Ctrl-C (SIGINT) does not run cleanup, so it may orphan `llama-server`.
- Free-port selection has a small race; pass `--port` to pin it.
- The spawned `llama-server` binds `127.0.0.1` with no authentication. On a
  shared multi-user machine, any local user can reach the port while a run is in
  progress. Single-user desktop use is the intended case.
