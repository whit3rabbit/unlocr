# unlocr CLI Reference

`unlocr` is a fast, local CLI tool and desktop GUI designed to OCR PDF files and images into clean Markdown. It is powered by the **Unlimited-OCR** (DeepSeek-OCR 3B VLM) model.

This document details how to install, configure, and run `unlocr` using the command-line interface.

---

## Quick Start

### 1. Prerequisites

Before using the CLI, ensure you have the `pdftoppm` rasterizer (part of `poppler`) installed on your system. 

*   **macOS**: `brew install poppler`
*   **Linux (Ubuntu/Debian)**: `sudo apt install poppler-utils`
*   **Windows**: The installer includes it, or you can download it via [poppler-windows](https://github.com/oschwartz10612/poppler-windows/releases) and add it to your `PATH`.

> [!NOTE]
> `llama-server` does **not** need to be installed manually. On its first run, `unlocr` will automatically download a pre-compiled, sha256-verified build containing necessary Unlimited-OCR R-SWA support patches.

### 2. Basic OCR Execution

Run `unlocr` by passing a PDF file path:

```bash
# OCR a single PDF file (outputs to the current directory as input_file.md)
unlocr report.pdf

# OCR and save output in a custom directory
unlocr report.pdf --out ./markdown_outputs

# Save the output to a specific filename
unlocr report.pdf -o final_report.md
```

### 3. Choose a Quality Preset

You can control accuracy, download size, and memory usage using the `--quality` flag:

```bash
# High fidelity (BF16 model - ~5.47 GB download)
unlocr report.pdf --quality best

# Standard preset (Q8_0 model - ~2.91 GB download, Default)
unlocr report.pdf --quality good

# Fast / Lightweight (Q4_K_M model - ~1.82 GB download)
unlocr report.pdf --quality less
```

---

## Subcommands

### `doctor` (alias `preflight`)
Validates system requirements, checks available RAM/disk space, and verifies model/binary components without performing OCR.

```bash
unlocr doctor
```

**Options for `doctor`:**
*   `--llama-bin <PATH>`: Custom `llama-server` path to validate.
*   `--model-dir <PATH>`: Cache directory to inspect.
*   `--quant <TAG>`: The model quantization format to check (defaults to `Q8_0`).

---

## CLI Options Reference

### Input and Output Options

| Option | Type | Default | Description |
|:---|:---|:---|:---|
| `<inputs>...` | Positional | None | One or more input PDF/image files, directories, or glob patterns. *Tip: Quote globs (e.g. `"*.pdf"`) so `unlocr` expands them directly, which is especially useful on Windows PowerShell.* |
| `--recursive` | Flag | `false` | Recurse into subdirectories when an input is a folder. |
| `--from-list <FILE>` | Path | None | Read additional PDF/image paths from a text file (one path per line; empty lines and lines starting with `#` are ignored). |
| `--out <DIR>` | Path | `.` | Output directory for the generated Markdown files. |
| `-o, --output <FILE>` | Path | None | Custom output file path (valid only for a single input file). Absolute paths are used as-is; relative paths are joined under `--out`. Appends `.md` if no file extension is supplied. |
| `--output-mode <MODE>` | Enum | `single` | Layout style for output. Options:<br>• `single`: A single `{stem}.md` with pages separated by delimiters.<br>• `pages`: A folder `{stem}/` containing individual `page-N.md` files.<br>• `both`: Saves both the concatenated file and the folder. |

### Model & Quality Parameters

| Option | Type | Default | Description |
|:---|:---|:---|:---|
| `--quality <TIER>` | Enum | `good` | Quality preset tag: `best` (BF16), `good` (Q8_0), or `less` (Q4_K_M). |
| `--quant <TAG>` | String | None | Select an exact Hugging Face quant tag (e.g., `Q6_K`, `IQ4_XS`). Overrides `--quality`. |
| `--model <PATH>` | Path | None | Skip HF download and run a local model GGUF file directly. Disables `--quant` and `--quality`. |
| `--mmproj <PATH>` | Path | None | Path to a custom projector (`mmproj`) GGUF. *Requires `--model`.* |
| `--model-dir <PATH>` | Path | *OS Cache* | Custom cache directory for GGUF model files. |

### OCR & Prompt Settings

| Option | Type | Default | Description |
|:---|:---|:---|:---|
| `--task <PRESET>` | Enum | `markdown` | Prompt preset:<br>• `markdown`: Clean structured markdown layout.<br>• `grounding`: Markdown accompanied by layout bounding box coordinates.<br>• `free`: Raw plain-text OCR.<br>• `figure`: Summarize/parse charts, diagrams, or figures. |
| `--prompt <TEXT>` | String | None | Custom OCR prompt sent with each page. Overrides `--task`. |
| `--pages <RANGE>` | String | None | Select a single page (e.g., `--pages 5`) or a range (e.g., `--pages 3-7`). Omit to OCR the entire document. |
| `--dpi <N>` | Integer | `144` | PDF rasterization DPI. Higher DPI produces clearer images for the model but requires more resources. |

### Generation & Sampler Parameters (Local GGUF Mode)

| Option | Type | Default | Description |
|:---|:---|:---|:---|
| `--max-tokens <N>` | Integer | `4096` | Max tokens generated per page. Helps terminate runaways. |
| `--temperature <F>` | Float | `0.0` | Sampling temperature. `0.0` forces deterministic OCR output (recommended). |
| `--image-max-tokens <N>` | Integer | None | Vision-token budget for `llama-server`. Higher values yield finer text details but increase VRAM/computation. |
| `--chat-template <NAME>` | String | None | Pass a custom chat template name (e.g. `deepseek-ocr`) to `llama-server`. |
| `--repeat-penalty <F>` | Float | `1.3` | Token repetition penalty. Defaults to `1.3` in local mode to avoid output loops. |
| `--dry-multiplier <F>` | Float | `1.0` | DRY repetition penalty multiplier (llama.cpp `dry_multiplier`). Defaults to `1.0` locally. Set to `0` to disable. |
| `--dry-base <F>` | Float | None | Exponential growth base for DRY penalty (`dry_base`). Defaults to server standard (1.75). |
| `--dry-allowed-length <N>` | Integer | `4` | Number of tokens allowed in repeated sequences before DRY starts penalizing. |
| `--dry-penalty-last-n <N>` | Integer | None | DRY window size. Set to `-1` to scan the whole context window. |

### Password Management

| Option | Type | Default | Description |
|:---|:---|:---|:---|
| `--password <PW>` | String | None | Password for encrypted PDFs. Overrides `UNLOCR_PDF_PASSWORD` environment variable. |
| `--password-file <FILE>` | Path | None | File containing candidate passwords (one per line). `unlocr` will test them sequentially until the PDF unlocks. |

### Remote Endpoint & GPU Serving

| Option | Type | Default | Description |
|:---|:---|:---|:---|
| `--gpu` | Flag | `false` | Shortcut to run OCR against a local vLLM server (`--endpoint http://localhost:8000 --endpoint-model baidu/Unlimited-OCR`). |
| `--endpoint <URL>` | String | None | OpenAI-compatible server API base URL (e.g. `http://localhost:8080/v1`). Skips local model spawning. |
| `--endpoint-key <KEY>` | String | None | Bearer token for the endpoint. Overrides `UNLOCR_API_KEY` environment variable. |
| `--endpoint-model <M>` | String | None | Target model identifier in the JSON request body (required by vLLM or SGLang). |

### System & Debugging

| Option | Type | Default | Description |
|:---|:---|:---|:---|
| `--llama-bin <PATH>` | Path | *Auto* | Override path to the `llama-server` binary. |
| `--port <N>` | Integer | `0` | Port for `llama-server` (default `0` picks a random open port). |
| `--keep-images` | Flag | `false` | Do not delete the temporary page PNGs generated during OCR. Useful for troubleshooting. |

---

## Advanced Use Cases

### 1. Preventing Repetition Loops (Dense/Ruled Pages)
DeepSeek-OCR models running on quantized GGUFs can occasionally get stuck in infinite repetition loops when reading dense tables, math equations, or heavily ruled page boundaries. 

To mitigate this, `unlocr` defaults to a repetition penalty of `1.3` and activates llama.cpp's **DRY (Don't Repeat Yourself) Sampler** with a multiplier of `1.0`. For highly problematic pages, use the community anti-loop presets:

```bash
unlocr report.pdf \
  --dry-multiplier 1.0 \
  --dry-allowed-length 2 \
  --dry-penalty-last-n -1 \
  --repeat-penalty 1.3
```
*   `--dry-allowed-length 2`: Penalty triggers as soon as 2 tokens repeat.
*   `--dry-penalty-last-n -1`: Penalty applies across the entire context window.

### 2. Processing Bulk & Password-Protected Batches
If you have a folder containing mixed encrypted and unencrypted PDFs, you can use a list input file alongside candidate passwords:

```bash
# Step 1: Create a text file containing the files to process (inputs.txt)
# /data/docs/invoice_a.pdf
# /data/docs/invoice_b.pdf

# Step 2: Create a text file with potential passwords (passwords.txt)
# password_abc
# Admin123

# Step 3: Run unlocr
unlocr --from-list inputs.txt --password-file passwords.txt --out ./output_md
```

### 3. Tuning Quality for Scanned vs. Digital Documents
*   **Digital PDFs (Clear Text)**: You can decrease `--dpi` to speed up processing and use a lighter quant to save memory.
    ```bash
    unlocr digital_doc.pdf --dpi 100 --quality less
    ```
*   **Scanned/Blurry Documents**: Increase DPI to sharpen the text input for the model.
    ```bash
    unlocr scanned_receipt.pdf --dpi 200 --quality best
    ```

### 4. Running a Remote vLLM Server on GPU
To offload OCR tasks to a GPU server running the full unquantized `DeepSeek-OCR` model:

1.  **Launch the vLLM server** (on your GPU host):
    ```bash
    vllm serve baidu/Unlimited-OCR \
      --no-enable-prefix-caching \
      --mm-processor-cache-gb 0 \
      --logits-processors vllm.model_executor.models.deepseek_ocr:NGramPerReqLogitsProcessor
    ```
2.  **Run `unlocr` pointing to this server**:
    ```bash
    # Shortcut to localhost:8000
    unlocr document.pdf --gpu
    
    # Or explicitly configuration
    unlocr document.pdf \
      --endpoint http://your-gpu-server:8000 \
      --endpoint-model baidu/Unlimited-OCR
    ```

---

## Environment Variables

*   `UNLOCR_PDF_PASSWORD`: Default password fallback for opening encrypted PDF files.
*   `UNLOCR_API_KEY`: Default API key/token used for `--endpoint` requests.
*   `UNLOCR_ALLOW_EXTERNAL_LLAMA`: Set to `1` to silence validation warnings when using custom `llama-server` binaries not verified by `unlocr`.
*   `XDG_CACHE_HOME`: Customizes the base cache directory where models and binaries are stored.
