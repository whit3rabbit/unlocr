# unlocr CLI 参考指南

`unlocr` 是一个快速、本地运行的命令行工具（CLI）和桌面 GUI，旨在将 PDF 文件和图像 OCR 转换为干净的 Markdown。它由 **Unlimited-OCR** (DeepSeek-OCR 3B VLM) 模型提供动力。

本篇文档详细介绍了如何通过命令行界面（CLI）安装、配置和运行 `unlocr`。

---

## 快速开始

### 1. 前提条件

在使用 CLI 之前，请确保您的系统上已安装 `pdftoppm` 栅格化工具（它是 `poppler` 的一部分）。

*   **macOS**：`brew install poppler`
*   **Linux (Ubuntu/Debian)**：`sudo apt install poppler-utils`
*   **Windows**：安装程序中已包含它，或者您可以通过 [poppler-windows](https://github.com/oschwartz10612/poppler-windows/releases) 进行下载并将其添加到您的 `PATH` 中。

> [!NOTE]
> `llama-server` **无需**手动安装。在首次运行时，`unlocr` 会自动下载包含 Unlimited-OCR R-SWA 支持修补程序的预编译、经过 sha256 验证的构建版本。

### 2. 基础 OCR 运行

通过传入 PDF 文件路径运行 `unlocr`：

```bash
# OCR 单个 PDF 文件（在当前目录下输出为 input_file.md）
unlocr report.pdf

# OCR 并将输出保存到自定义目录
unlocr report.pdf --out ./markdown_outputs

# 将输出保存为特定文件名
unlocr report.pdf -o final_report.md
```

### 3. 选择质量预设

您可以通过 `--quality` 标志来控制识别准确度、下载大小和内存占用：

```bash
# 高保真度 (BF16 模型 - ~5.47 GB 下载)
unlocr report.pdf --quality best

# 标准预设 (Q8_0 模型 - ~2.91 GB 下载，默认值)
unlocr report.pdf --quality good

# 快速 / 轻量级 (Q4_K_M 模型 - ~1.82 GB 下载)
unlocr report.pdf --quality less
```

---

## 子命令

### `doctor` (别名 `preflight`)
验证系统环境要求、检查可用内存（RAM）和磁盘空间，并验证模型/二进制组件，不执行 OCR 转换。

```bash
unlocr doctor
```

**`doctor` 的选项：**
*   `--llama-bin <PATH>`：要验证的自定义 `llama-server` 路径。
*   `--model-dir <PATH>`：要检查的缓存目录。
*   `--quant <TAG>`：要检查的模型量化格式（默认值为 `Q8_0`）。

---

## CLI 选项参考

### 输入与输出选项

| 选项 | 类型 | 默认值 | 描述 |
|:---|:---|:---|:---|
| `<inputs>...` | 位置参数 | 无 | 一个或多个输入 PDF/图像文件、目录或 glob 匹配模式。*提示：请对 glob 模式加引号（例如 `"*.pdf"`），以便 `unlocr` 直接对其展开，这在 Windows PowerShell 下非常有用。* |
| `--recursive` | 标志 | `false` | 当输入是文件夹时，递归进入子目录。 |
| `--from-list <FILE>` | 路径 | 无 | 从文本文件中读取额外的 PDF/图像路径（每行一个路径；空白行和以 `#` 开头的行会被忽略）。 |
| `--out <DIR>` | 路径 | `.` | 转换后的 Markdown 文件的输出目录。 |
| `-o, --output <FILE>` | 路径 | 无 | 自定义输出文件路径（仅适用于单个输入文件）。绝对路径将原样使用；相对路径会在 `--out` 目录下拼接。如果不提供扩展名，默认追加 `.md`。 |
| `--output-mode <MODE>` | 枚举 | `single` | 输出的排版样式。选项：<br>• `single`：单个 `{stem}.md` 文件，页面间通过分隔符拼接。<br>• `pages`：一个名为 `{stem}/` 的文件夹，其中包含单独的 `page-N.md` 文件。<br>• `both`：同时保存拼接文件和分页文件夹。 |

### 模型与质量参数

| 选项 | 类型 | 默认值 | 描述 |
|:---|:---|:---|:---|
| `--quality <TIER>` | 枚举 | `good` | 质量预设标签：`best` (BF16), `good` (Q8_0), 或 `less` (Q4_K_M)。 |
| `--quant <TAG>` | 字符串 | 无 | 选择精确的 Hugging Face 量化标签（例如 `Q6_K`, `IQ4_XS`）。会覆盖 `--quality`。 |
| `--model <PATH>` | 路径 | 无 | 跳过 HF 下载，直接运行本地的模型 GGUF 文件。这会禁用 `--quant` 和 `--quality`。 |
| `--mmproj <PATH>` | 路径 | 无 | 自定义多模态投影器（`mmproj`）GGUF 的路径。*需要配合 `--model` 使用。* |
| `--model-dir <PATH>` | 路径 | *系统缓存* | 存放下载的 GGUF 模型文件的自定义缓存目录。 |

### OCR 与提示词设置

| 选项 | 类型 | 默认值 | 描述 |
|:---|:---|:---|:---|
| `--task <PRESET>` | 枚举 | `markdown` | 提示词预设：<br>• `markdown`：干净、结构化的 Markdown 布局。<br>• `grounding`：带有页面布局边界框坐标的 Markdown。<br>• `free`：原始纯文本 OCR。<br>• `figure`：解析并总结图表、示意图或插图。 |
| `--prompt <TEXT>` | 字符串 | 无 | 每页发送的自定义 OCR 提示词。会覆盖 `--task`。 |
| `--pages <RANGE>` | 字符串 | 无 | 选择特定单页（例如 `--pages 5`）或范围（例如 `--pages 3-7`）。省略此项则对整篇文档进行 OCR。 |
| `--dpi <N>` | 整数 | `144` | PDF 栅格化 DPI。更高的 DPI 会为模型生成更清晰的图像，但需要消耗更多资源。 |

### 生成和采样参数（本地 GGUF 模式）

| 选项 | 类型 | 默认值 | 描述 |
|:---|:---|:---|:---|
| `--max-tokens <N>` | 整数 | `4096` | 每页生成的最大 Token 数。有助于终止生成失控。 |
| `--temperature <F>` | 浮点数 | `0.0` | 采样温度。`0.0` 会强制输出确定性的 OCR 结果（推荐）。 |
| `--image-max-tokens <N>` | 整数 | 无 | `llama-server` 的视觉 Token 预算。较高的值会保留更精细的文本细节，但会增加 VRAM 和计算开销。 |
| `--chat-template <NAME>` | 字符串 | 无 | 向 `llama-server` 传递自定义聊天模板名称（例如 `deepseek-ocr`）。 |
| `--repeat-penalty <F>` | 浮点数 | `1.3` | Token 重复惩罚系数。在本地模式下默认值为 `1.3`，以避免陷入输出死循环。 |
| `--dry-multiplier <F>` | 浮点数 | `1.0` | DRY 重复惩罚乘数（llama.cpp `dry_multiplier`）。在本地默认值为 `1.0`。设置为 `0` 可以禁用。 |
| `--dry-base <F>` | 浮点数 | 无 | DRY 惩罚的指数增长底数（`dry_base`）。默认采用服务器的标准底数（1.75）。 |
| `--dry-allowed-length <N>` | 整数 | `4` | 在 DRY 开始施加惩罚之前，允许重复序列的 Token 数量。 |
| `--dry-penalty-last-n <N>` | 整数 | 无 | DRY 扫描窗口大小。设置为 `-1` 可以扫描整个上下文窗口。 |

### 密码管理

| 选项 | 类型 | 默认值 | 描述 |
|:---|:---|:---|:---|
| `--password <PW>` | 字符串 | 无 | 加密 PDF 的打开密码。会覆盖 `UNLOCR_PDF_PASSWORD` 环境变量。 |
| `--password-file <FILE>` | 路径 | 无 | 包含候选密码的文本文件（每行一个）。`unlocr` 将依次测试它们，直到 PDF 解锁。 |

### 远程端点与 GPU 服务

| 选项 | 类型 | 默认值 | 描述 |
|:---|:---|:---|:---|
| `--gpu` | 标志 | `false` | 针对本地 vLLM 服务器运行 OCR 的快捷方式（等同于 `--endpoint http://localhost:8000 --endpoint-model baidu/Unlimited-OCR`）。 |
| `--endpoint <URL>` | 字符串 | 无 | 兼容 OpenAI 格式的服务器 API 基础 URL（例如 `http://localhost:8080/v1`）。跳过本地模型启动。 |
| `--endpoint-key <KEY>` | 字符串 | 无 | 远程端点的 Bearer Token。会覆盖 `UNLOCR_API_KEY` 环境变量。 |
| `--endpoint-model <M>` | 字符串 | 无 | 请求体中发送的目标模型标识符（vLLM 或 SGLang 需要此项）。 |

### 系统与调试

| 选项 | 类型 | 默认值 | 描述 |
|:---|:---|:---|:---|
| `--llama-bin <PATH>` | 路径 | *自动* | 覆盖 `llama-server` 二进制文件的路径。 |
| `--port <N>` | 整数 | `0` | `llama-server` 绑定的端口（默认值 `0` 会自动选择可用端口）。 |
| `--keep-images` | 标志 | `false` | 不删除 OCR 过程中生成的临时页面 PNG 图像。适合排查问题。 |

---

## 高级使用案例

### 1. 防止重复死循环（高密/带下划线页面）
在读取高密表格、数学公式或带有深色网格边界的页面时，运行在量化 GGUF 上的 DeepSeek-OCR 模型偶尔会陷入无限重复的死循环。

为了缓解这种情况，`unlocr` 默认在本地设置了 `1.3` 的重复惩罚系数，并启用了 llama.cpp 的 **DRY (Don't Repeat Yourself) 采样器**（乘数为 `1.0`）。对于极易出现死循环的特殊页面，建议使用以下防循环预设参数：

```bash
unlocr report.pdf \
  --dry-multiplier 1.0 \
  --dry-allowed-length 2 \
  --dry-penalty-last-n -1 \
  --repeat-penalty 1.3
```
*   `--dry-allowed-length 2`：只要有 2 个 Token 重复，立即触发 DRY 惩罚。
*   `--dry-penalty-last-n -1`：惩罚作用于整个上下文窗口。

### 2. 批量处理与加密 PDF 的混合解析
如果您的文件夹包含部分加密和未加密的 PDF，您可以配合密码候选文件以批量形式运行：

```bash
# 步骤 1：创建一个包含待处理文件路径的文本文件 (inputs.txt)
# /data/docs/invoice_a.pdf
# /data/docs/invoice_b.pdf

# 步骤 2：创建一个包含候选密码的文本文件 (passwords.txt)
# password_abc
# Admin123

# 步骤 3：运行 unlocr 批量处理
unlocr --from-list inputs.txt --password-file passwords.txt --out ./output_md
```

### 3. 调整扫描件与数字 PDF 的 DPI 以优化质量
*   **原生电子版 PDF（排版清晰）**：可以适当降低 `--dpi` 以加快处理速度，并使用更轻量的量化（quality）以节省内存。
    ```bash
    unlocr digital_doc.pdf --dpi 100 --quality less
    ```
*   **扫描件或模糊文档**：增加 DPI 以使导出的图像更加清晰，方便模型识读。
    ```bash
    unlocr scanned_receipt.pdf --dpi 200 --quality best
    ```

### 4. 远程调用 GPU 上的 vLLM 服务
要将 OCR 任务分流到部署了未量化完整 `DeepSeek-OCR` 模型的 GPU 服务器上：

1.  **在 GPU 主机上启动 vLLM 服务**：
    ```bash
    vllm serve baidu/Unlimited-OCR \
      --no-enable-prefix-caching \
      --mm-processor-cache-gb 0 \
      --logits-processors vllm.model_executor.models.deepseek_ocr:NGramPerReqLogitsProcessor
    ```
2.  **让本地 `unlocr` 调用该远程服务**：
    ```bash
    # 本地 vLLM localhost:8000 快捷方式
    unlocr document.pdf --gpu
    
    # 或者配置显式远程地址
    unlocr document.pdf \
      --endpoint http://your-gpu-server:8000 \
      --endpoint-model baidu/Unlimited-OCR
    ```

---

## 环境变量

*   `UNLOCR_PDF_PASSWORD`：打开加密 PDF 文件的默认密码。
*   `UNLOCR_API_KEY`：用于 `--endpoint` 远程请求的默认 API 密钥/Bearer Token。
*   `UNLOCR_ALLOW_EXTERNAL_LLAMA`：设置为 `1` 时，使用未经 `unlocr` 验证的第三方 `llama-server` 二进制文件不会弹出警告。
*   `XDG_CACHE_HOME`：自定义模型和二进制程序下载存储的根缓存路径。
