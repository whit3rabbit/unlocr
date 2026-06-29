# unlocr (unlimited-OCR)

**Not affiliated with Unlimited OCR but a wrapper/GUI around Unlimited OCR**

A fast, lightweight tool to OCR PDFs into clean Markdown. It is powered by the **[Unlimited-OCR](https://huggingface.co/sahilchachra/Unlimited-OCR-GGUF)** model (DeepSeek-OCR 3B VLM) running locally via **`llama.cpp`** (GGUF).

Currently, a WIP.

### Key Features

*   **Local & Secure**: Runs entirely offline on your CPU or GPU.
*   **Auto-cached**: Model weights download automatically from Hugging Face on first run.
*   **High Performance**: Uses a single persistent `llama-server` background process to avoid reloading overhead across pages.
*   **Flexible Engines**: Supports GGUF (default, via llama.cpp) or the full unquantized model on GPU (via vLLM/SGLang).
*   **Multi-Platform**: Available for **macOS, Linux, and Windows** as both a **CLI** and a **Desktop GUI** application.

---

## Requirements

To run `unlocr`, you need:
1.  **poppler** (provides `pdftoppm` for rasterizing PDF pages to images).
2.  **llama.cpp** build `>= b8530` (PR [#17400](https://github.com/ggml-org/llama.cpp/pull/17400) merged 2026-03-25 is required to support DeepSeek-OCR).
3.  **pandoc** *(optional)* — only for the GUI's "export to DOCX/ODT/RTF/HTML/TXT" feature. OCR works without it.
4.  **Rust Toolchain** (only if building from source or installing via cargo).

#### How prerequisites are provided (per platform)

The **CLI** always expects `pdftoppm` and `llama-server` already on your `PATH`. The **Desktop GUI** makes this easier, differently per OS:

| OS | poppler (`pdftoppm`) | llama.cpp (`llama-server`) | pandoc (export) |
|----|----------------------|----------------------------|-----------------|
| **Windows** | GUI downloads it for you | GUI downloads a **CPU** build for you | GUI downloads it for you |
| **Linux** | installed by the `.deb`/`.rpm` (declared dep) | **install manually** (no apt/dnf package) | installed by the `.deb`/`.rpm` (recommended dep) |
| **macOS** | `brew install poppler` (cask dep) | **`brew install llama.cpp`** (Homebrew required) | GUI downloads it, or `brew install pandoc` |

> [!NOTE]
> In the GUI, open **Settings → Dependencies** to see what is found/missing and to fetch what's available for your platform (a sha256-pinned download on Windows / pandoc on macOS, or a one-click `brew install` on macOS). Windows GPU users should install their own CUDA/Vulkan `llama.cpp` build instead of the bundled CPU one.

### Model Variants & System Specs
By default, `unlocr` runs a quantized GGUF on `llama.cpp` (CPU or GPU-offloaded). You can also run the full, unquantized model on a dedicated GPU via `vLLM`.

| Mode | Variant | Download Size | RAM/VRAM Required | Engine | Quality Flag |
|------|---------|---------------|-------------------|--------|--------------|
| **GGUF** | `Q4_K_M` | 1.82 GB | ~4 GB RAM | llama.cpp | `--quality less` |
| **GGUF** | `Q8_0` *(Default)* | 2.91 GB | ~6 GB RAM | llama.cpp | `--quality good` |
| **GGUF** | `BF16` | 5.47 GB | ~8 GB RAM | llama.cpp | `--quality best` |
| **Full Model** | `DeepSeek-OCR` | ~6.7 GB | 16 GB+ VRAM | vLLM | `--gpu` |

### How to Install by OS

Select your operating system below for quick setup instructions.

#### macOS
Homebrew is required (mainly for `llama.cpp`, which has no standalone macOS binary). Install the prerequisites:
```bash
brew install llama.cpp poppler   # pandoc optional, only for GUI export: brew install pandoc
```
Then install `unlocr` using Homebrew:
```bash
# Install the CLI tool
brew install whit3rabbit/tap/unlocr

# Or install the GUI Desktop App
brew install --cask whit3rabbit/tap/unlocr
```
> [!NOTE]
> For the unsigned macOS GUI app, Homebrew handles quarantine flags automatically. If downloading manually, run:
> `xattr -dr com.apple.quarantine "/Applications/unlocr.app"`

#### Linux
1. Download the pre-built CLI binary or GUI installer (`.AppImage`, `.deb`, `.rpm`) from [GitHub Releases](../../releases).
2. The `.deb`/`.rpm` declares **poppler** (and **pandoc** for GUI export) as dependencies, so your package manager pulls them in automatically.
3. **`llama.cpp` must be installed manually** — it is not in apt/dnf. Build it (`>= b8530`) or fetch a release binary and put `llama-server` on your `PATH`. The package's post-install step warns if it is missing.

#### Windows
1. Download the CLI executable or the Windows GUI Installer (`.msi`) from [GitHub Releases](../../releases).
2. **GUI**: no manual setup — open **Settings → Dependencies** and click Download for any missing tool. The GUI fetches sha256-pinned `pdftoppm`, a CPU `llama-server`, and `pandoc` into its cache (GPU users should install their own `llama.cpp` build).
3. **CLI**: ensure `llama-server` and `pdftoppm` are installed and on your `PATH`, then run the installer or script:
   ```powershell
   powershell -ExecutionPolicy Bypass -File packaging\windows\install.ps1
   ```

#### Alternative: Install via Cargo (CLI)
If you have the Rust toolchain installed:
```bash
cargo install unlocr
```

#### Alternative: Build from Source
```bash
# Clone the repository and run the install script (macOS/Linux)
./install.sh
```
*(For Windows source installation details, see [packaging/README.md](packaging/README.md).)*

---

## How to Run

### Desktop GUI App
Simply launch the installed **unlocr** application from your OS Applications folder (or Start Menu). The GUI provides an intuitive interface to load PDFs, select quality presets, customize prompts, and start the OCR process.

### CLI Tool
Run `unlocr` by passing one or more PDFs:
```bash
unlocr <input.pdf> [more.pdf ...] [options]
```

**Example**:
```bash
unlocr report.pdf --out ./out --quality best
# Generates ./out/report.md with page-delimited <!-- page N --> markers
```

---

## Developer Reference & CLI Arguments

### CLI Arguments & Options

| Option | Default | Description |
|--------|---------|-------------|
| `--out DIR` | `.` | Output directory for the converted Markdown files. |
| `-o, --output FILE` | *(from input name)* | Single output file path (single-input only). `.md` appended when no extension; joined under `--out` when relative. |
| `--recursive` | `false` | Recurse into subdirectories when an input is a folder. |
| `--from-list FILE` | *(none)* | Read extra PDF paths from a text file (one per line; `#` comments and blank lines skipped). |
| `--quality TIER` | `good` | Quality preset. Options: `best` (BF16), `good` (Q8_0), `less` (Q4_K_M). |
| `--quant TAG` | *(from quality)* | Exact Hugging Face model quant tag (e.g. `Q6_K`, `IQ4_XS`). Overrides `--quality`. |
| `--model PATH` | *(HF download)* | Use this GGUF directly, skipping the HF download and naming convention. Disables `--quant`/`--quality`/`--model-dir` selection. |
| `--mmproj PATH` | *(cached projector)* | Projector (mmproj) GGUF override. Requires `--model`. |
| `--max-tokens N` | `4096` | Max tokens generated per page (prevents infinite loop/runaways on dense pages). |
| `--pages RANGE` | *(all)* | Pages to OCR: a single page (`5`) or inclusive 1-based range (`5-9`). Applies to every input. |
| `--task PRESET` | `markdown` | Prompt preset: `markdown` (formatted md), `free` (plain text), `figure` (parses charts/figures). |
| `--prompt TEXT` | *(from task)* | Custom OCR prompt. Overrides `--task`. Use `<|grounding|>` for layout coordinates. |
| `--dpi N` | `144` | PDF page rendering DPI (higher DPI gives larger/clearer source images). |
| `--image-max-tokens N` | *(model default)* | Vision-token budget for `llama-server` (local mode only). Higher means finer detail recognition but slower/more VRAM. |
| `--chat-template NAME` | *(model default)* | Forwarded to `llama-server --chat-template` (e.g., `deepseek-ocr`); local mode only. |
| `--repeat-penalty F` | *(server default)*| Sampling repeat penalty (e.g., `1.1`); helps break generation loops in smaller quants. |
| `--llama-bin PATH` | *Auto-detected* | Path to the `llama-server` binary. |
| `--model-dir PATH` | *OS Cache* | Custom cache directory for GGUF model downloads. |
| `--port N` | `0` (Auto) | Port for the spawned `llama-server`. |
| `--keep-images` | `false` | Retain the intermediate page PNGs generated during processing. |
| `--gpu` | `false` | Run the full DeepSeek-OCR model via local vLLM. Shortcut for `--endpoint http://localhost:8000 --endpoint-model deepseek-ai/DeepSeek-OCR`. |
| `--endpoint URL` | *Local spawn* | Route requests to a remote OpenAI-compatible server (vLLM, SGLang, etc.) and skip local spawn. |
| `--endpoint-key KEY`| `UNLOCR_API_KEY` | Bearer API token for `--endpoint` (or set via `UNLOCR_API_KEY` env var). |
| `--endpoint-model M`| *(none)* | Model name sent in endpoint request body (required by vLLM/LiteLLM gateways). |

**Subcommands**: `unlocr doctor` (alias `preflight`) validates system deps, model files, RAM, and disk space without running OCR. Accepts `--llama-bin`, `--model-dir`, `--quant`.

### How It Works Under the Hood
1.  **Preflight**: Locates `llama-server` and `pdftoppm`, checks versions, and warns if `llama-server` is below b8530.
2.  **Model Cache**: Checks for `Unlimited-OCR-<quant>.gguf` and `mmproj-Unlimited-OCR-F16.gguf` in the cache directory, downloading them from HF if missing.
3.  **Spawn Server**: Starts a single background `llama-server` instance and polls `/health` until active.
4.  **OCR Processing**: For each PDF, runs `pdftoppm` to extract pages as PNGs, POSTs them sequentially (base64 encoded) to `/v1/chat/completions`, and appends the markdown output.

### Model Caching
GGUFs are cached locally. They can be quite large (~1.8 GB to ~8.0 GB total depending on quants downloaded).

| OS | Default Cache Path |
|----|--------------------|
| **macOS** | `~/Library/Caches/unlocr` |
| **Linux** | `$XDG_CACHE_HOME/unlocr` (or `~/.cache/unlocr`) |
| **Windows**| `%LOCALAPPDATA%\unlocr` |

*Override the path using `--model-dir PATH` or the `$XDG_CACHE_HOME` env var.*

### Uninstalling & Reclaiming Space
To remove the application binary and purge downloaded model weights, run:
*   **macOS/Linux**: `./uninstall.sh`
*   **Windows**: `powershell -ExecutionPolicy Bypass -File packaging\windows\uninstall.ps1`

### Running the Full Model on GPU (vLLM)
For high-performance GPU serving of the unquantized model:
1.  Install vLLM (pre-release recommended for DeepSeek-OCR support):
    ```bash
    pip install -U vllm --pre --extra-index-url https://wheels.vllm.ai/nightly
    ```
2.  Serve the model:
    ```bash
    vllm serve deepseek-ai/DeepSeek-OCR \
      --no-enable-prefix-caching \
      --mm-processor-cache-gb 0 \
      --logits-processors vllm.model_executor.models.deepseek_ocr:NGramPerReqLogitsProcessor
    ```
3.  Run `unlocr` targeting this endpoint:
    ```bash
    unlocr report.pdf --gpu
    ```
    *(In the GUI, select the **GPU full model (vLLM · DeepSeek-OCR)** engine preset.)*

> [!TIP]
> **Google Colab**: Check out the [`colab/unlocr-deepseek-ocr-gpu.ipynb`](colab/unlocr-deepseek-ocr-gpu.ipynb) notebook to run the full GPU pipeline on a free or cheap Colab cloud instance.

### Unofficial Benchmark
*   **Hardware**: macOS (Apple Silicon, Metal)
*   **llama.cpp**: build b9770
*   **Model**: BF16 (`--quality best`, 5.47GB)
*   **Document**: 355-page book PDF (~12.4MB), 144 DPI

| Metric | Result |
|--------|--------|
| **Cold Start (Model Loaded)** | ~15 seconds |
| **Total Processing Time** | 42 min 44 s (~7.2s/page) |
| **Output size** | 1.2 MB markdown (~192k words) |

*Smaller quants (`--quality good` / `less`) trade off accuracy for much faster speeds and smaller downloads.*

### Limitations & Security
*   **Ctrl-C (SIGINT)**: Interrupting CLI does not clean up the background server process, which might orphan `llama-server`.
*   **Port Race**: Free-port allocation may occasionally conflict; pin using `--port N`.
*   **Authentication**: The local `llama-server` binds to `127.0.0.1` without auth. On multi-user machines, other local users could access the server port during execution. Single-user environments are recommended.

### License
The `unlocr` codebase is released under the [MIT License](LICENSE). Note that model weights downloaded automatically from Hugging Face are governed by their respective licenses (see HF model card).
