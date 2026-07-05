# unlocr Agent Guidelines

This document outlines key architectural constraints, gotchas, and parameter mappings for agents working on the `unlocr` repository.

## 1. DeepSeek-OCR Repetition Loop Prevention

The Unlimited-OCR / DeepSeek-OCR models are prone to entering degenerate repetition loops (repeating words, line fragments, or phrase sequences) on low-content, ruled, underscore-heavy, or blank document pages.

### Parameter Mappings: Upstream Python vs. unlocr (GGUF/llama.cpp)
Upstream Python implementations (vLLM / SGLang) use custom logits processors, whereas `unlocr` uses `llama.cpp`'s samplers. Keep these equivalents in mind when configuring parameters:

| Python Model Parameter | llama.cpp/GGUF equivalent in `unlocr` | Description |
|:---|:---|:---|
| `no_repeat_ngram_size` | `--dry-allowed-length` (default: 4) | The maximum run length tolerated before penalty is applied. |
| `ngram_window` | `--dry-penalty-last-n` (default: server-default) | The context token window scan size (e.g., `-1` for whole context). |
| *Logits Processor* | `--dry-multiplier` (default: 1.0) | The multiplier/strength of the DRY sampler penalty. |
| `temperature` | `--temperature` (default: 0.0) | Standard temperature value for deterministic generation. |

### Anti-Loop Presets
For dense math, tabular, or highly structured pages, the recommended parameters to prevent repetition loops are:
* `--dry-allowed-length 2`
* `--dry-penalty-last-n -1` (covers the entire context window)
* `--repeat-penalty 1.3` (or higher)
