use crate::Res;
use std::time::Duration;

mod local;
mod remote;
#[cfg(test)]
mod tests;

pub use local::Server;
pub use remote::RemoteEndpoint;

// First model load (incl. mmproj) can be slow; give it room.
pub(crate) const HEALTH_TIMEOUT: Duration = Duration::from_secs(180);

/// Helper to block on a future. If we're inside a tokio runtime (like in Tauri),
/// we use `Handle::block_on`. Otherwise (like in the CLI main thread), we build
/// a single-threaded Runtime and run the future on it.
pub(crate) fn block_on<F: std::future::Future>(future: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(future),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to build tokio runtime");
            rt.block_on(future)
        }
    }
}

/// Grab a free port by binding to :0 and immediately releasing it.
pub fn free_port() -> Res<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// Result of OCR'ing one page: the assembled text plus whether the server
/// stopped because it hit `max_tokens` rather than a natural stop
/// (`finish_reason == "length"`). A page that loops until the cap and a page
/// that finished cleanly are otherwise indistinguishable to the caller, so
/// `truncated` lets `ocr_pages` (src/job.rs) flag likely repetition loops
/// instead of silently writing the garbage out as if it were real text.
///
/// `pub`, not `pub(crate)`: it's the return type of the `pub trait ImageOcr`
/// methods, and `ImageOcr` is reachable through the crate's public API
/// (`pub fn ocr_pages<S: ImageOcr>`), so the trait's own visibility requires
/// this to be at least as public (`private_interfaces` lint under `-D warnings`).
#[derive(Clone, Debug)]
pub struct OcrResult {
    pub text: String,
    pub truncated: bool,
}

/// Anything that can OCR one image given a prompt. Lets the page loop in
/// `lib::ocr_pages` drive either a local `Server` or a `RemoteEndpoint` without
/// caring which. The body it sends is provider-agnostic (OpenAI chat-completions).
pub trait ImageOcr {
    #[allow(clippy::too_many_arguments)]
    fn ocr_image(
        &self,
        prompt: &str,
        data_uri: &str,
        max_tokens: u32,
        repeat_penalty: Option<f32>,
        dry_multiplier: Option<f32>,
        dry_base: Option<f32>,
    ) -> Res<OcrResult>;

    /// Stream completions: like `ocr_image` but calls `on_token` with each partial
    /// text chunk as it arrives (SSE delta), and returns the full assembled text.
    /// The default falls back to the non-streaming path (safe for stub servers
    /// and callers that do not need partial-token events).
    #[allow(clippy::too_many_arguments)]
    fn ocr_image_stream(
        &self,
        prompt: &str,
        data_uri: &str,
        max_tokens: u32,
        repeat_penalty: Option<f32>,
        dry_multiplier: Option<f32>,
        dry_base: Option<f32>,
        on_token: &mut dyn FnMut(&str) -> bool,
        should_cancel: &dyn Fn() -> bool,
    ) -> Res<OcrResult> {
        let _ = on_token; // default: ignore the sink
        let _ = should_cancel;
        self.ocr_image(
            prompt,
            data_uri,
            max_tokens,
            repeat_penalty,
            dry_multiplier,
            dry_base,
        )
    }
}

/// Attach the optional llama.cpp sampling extensions to a request body. No-op
/// when both are None so the baseline body is byte-for-byte unchanged (a remote
/// vLLM/SGLang endpoint never sees the extra fields). Shared by the streaming
/// and non-streaming request builders (repo rule: one helper, both paths).
///
/// `dry_multiplier` enables llama.cpp's DRY sampler, the closest analog of the
/// sliding-window no-repeat-ngram logits processor the upstream Python wrapper
/// uses for loop prevention (that processor doesn't ship in the GGUF). When set,
/// `dry_allowed_length` is raised from the server default 2 to 4: OCR output
/// legitimately repeats short in-line runs (table separators, repeated units),
/// while runaway loops repeat far longer sequences, so 4 keeps the loop-killing
/// power at zero cost to real document structure. `dry_penalty_last_n` and the
/// sequence breakers (which include `\n`, protecting row-by-row tables) stay at
/// server defaults. `dry_base` (growth rate of the penalty past
/// `dry_allowed_length`, server default 1.75) is an opt-in override: only sent
/// when `dry_multiplier` is also set (it is inert in llama.cpp without DRY
/// enabled), and no local default is injected for it (unlike repeat_penalty/
/// dry_multiplier) since it is newer/less battle-tested.
pub(crate) fn apply_sampling(
    body: &mut serde_json::Value,
    repeat_penalty: Option<f32>,
    dry_multiplier: Option<f32>,
    dry_base: Option<f32>,
) {
    if let Some(rp) = repeat_penalty {
        body["repeat_penalty"] = serde_json::json!(rp);
    }
    if let Some(dm) = dry_multiplier {
        body["dry_multiplier"] = serde_json::json!(dm);
        body["dry_allowed_length"] = serde_json::json!(4);
        if let Some(db) = dry_base {
            body["dry_base"] = serde_json::json!(db);
        }
    }
}

/// POST one image + prompt to an OpenAI-compatible `{base_url}/v1/chat/completions`
/// and return the assistant text. Shared by the local `Server` and `RemoteEndpoint`;
/// the only difference is the base URL and an optional bearer token.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ocr_via(
    base_url: &str,
    api_key: Option<&str>,
    model: Option<&str>,
    prompt: &str,
    data_uri: &str,
    max_tokens: u32,
    repeat_penalty: Option<f32>,
    dry_multiplier: Option<f32>,
    dry_base: Option<f32>,
) -> Res<OcrResult> {
    let url = format!("{base_url}/v1/chat/completions");
    let mut body = serde_json::json!({
        "temperature": 0,
        "max_tokens": max_tokens,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": prompt },
                { "type": "image_url", "image_url": { "url": data_uri } }
            ]
        }]
    });
    if let Some(m) = model {
        body["model"] = serde_json::Value::String(m.to_string());
    }
    apply_sampling(&mut body, repeat_penalty, dry_multiplier, dry_base);
    let api_key = api_key.map(|s| s.to_string());

    block_on(async move {
        let client = reqwest::Client::new();
        let mut req = client
            .post(&url)
            .timeout(Duration::from_secs(600))
            .json(&body);
        if let Some(key) = &api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let err_text = resp.text().await.unwrap_or_default();
            return Err(format!("HTTP error {}: {}", status, err_text).into());
        }
        let resp_json: serde_json::Value = resp.json().await?;
        let text = parse_completion(&resp_json)?;
        let truncated = finish_reason(&resp_json).as_deref() == Some("length");
        Ok(OcrResult { text, truncated })
    })
}

/// Handle one parsed SSE chunk: surface a per-chunk provider error (a 2xx
/// stream can still carry one, e.g. `{"error":{"message":"Context length
/// exceeded"}}`), forward delta content to `on_token`, and fold
/// `finish_reason` into `truncated`. Shared by `ocr_via_stream`'s main per-line
/// loop and its leftover-buffer tail so the two spots can't drift apart.
fn handle_stream_chunk(
    chunk_val: &serde_json::Value,
    full: &mut String,
    truncated: &mut bool,
    on_token: &mut dyn FnMut(&str) -> bool,
) -> Res<()> {
    if let Some(msg) = provider_error(chunk_val) {
        return Err(format!("provider error: {msg}").into());
    }
    if let Some(token) = chunk_val["choices"][0]["delta"]["content"].as_str() {
        if !on_token(token) {
            return Err("stopped".into());
        }
        full.push_str(token);
    }
    if finish_reason(chunk_val).as_deref() == Some("length") {
        *truncated = true;
    }
    Ok(())
}

/// POST with `stream: true` and consume the SSE response, calling `on_token`
/// for each `choices[0].delta.content` chunk. Returns the full assembled text.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ocr_via_stream(
    base_url: &str,
    api_key: Option<&str>,
    model: Option<&str>,
    prompt: &str,
    data_uri: &str,
    max_tokens: u32,
    repeat_penalty: Option<f32>,
    dry_multiplier: Option<f32>,
    dry_base: Option<f32>,
    on_token: &mut dyn FnMut(&str) -> bool,
    should_cancel: &dyn Fn() -> bool,
) -> Res<OcrResult> {
    let url = format!("{base_url}/v1/chat/completions");
    let mut body = serde_json::json!({
        "temperature": 0,
        "max_tokens": max_tokens,
        "stream": true,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": prompt },
                { "type": "image_url", "image_url": { "url": data_uri } }
            ]
        }]
    });
    if let Some(m) = model {
        body["model"] = serde_json::Value::String(m.to_string());
    }
    apply_sampling(&mut body, repeat_penalty, dry_multiplier, dry_base);
    let api_key = api_key.map(|s| s.to_string());

    let mut full = String::new();
    let mut saw_sse = false;
    let mut raw = String::new();
    let mut truncated = false;

    block_on(async {
        let client = reqwest::Client::new();
        let mut req = client
            .post(&url)
            .timeout(Duration::from_secs(600))
            .json(&body);
        if let Some(key) = &api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }

        // 1. Send the request with cancellation support during connection/TTFT
        let request_fut = async {
            let resp = req.send().await?;
            let status = resp.status();
            if !status.is_success() {
                let err_text = resp.text().await.unwrap_or_default();
                return Err(Box::<dyn std::error::Error>::from(format!(
                    "HTTP error {}: {}",
                    status, err_text
                )));
            }
            Ok::<reqwest::Response, Box<dyn std::error::Error>>(resp)
        };

        let mut resp = tokio::select! {
            res = request_fut => {
                res?
            }
            _ = async {
                let mut interval = tokio::time::interval(Duration::from_millis(100));
                loop {
                    interval.tick().await;
                    if should_cancel() {
                        break;
                    }
                }
            } => {
                return Err(Box::<dyn std::error::Error>::from("stopped"));
            }
        };

        // 2. Stream chunk-by-chunk with cancellation support
        let mut line_buffer = String::new();

        loop {
            let chunk_opt = tokio::select! {
                chunk_res = resp.chunk() => {
                    chunk_res?
                }
                _ = async {
                    let mut interval = tokio::time::interval(Duration::from_millis(100));
                    loop {
                        interval.tick().await;
                        if should_cancel() {
                            break;
                        }
                    }
                } => {
                    return Err(Box::<dyn std::error::Error>::from("stopped"));
                }
            };

            let Some(chunk_bytes) = chunk_opt else {
                break;
            };

            let chunk_str = String::from_utf8_lossy(chunk_bytes.as_ref());
            line_buffer.push_str(&chunk_str);

            while let Some(pos) = line_buffer.find('\n') {
                let line = line_buffer[..pos].to_string();
                line_buffer.drain(..=pos);

                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Some(json_str) = line.strip_prefix("data: ") else {
                    raw.push_str(line);
                    continue;
                };
                saw_sse = true;
                if json_str.trim() == "[DONE]" {
                    return Ok(());
                }
                let Ok(chunk_val) = serde_json::from_str::<serde_json::Value>(json_str) else {
                    continue;
                };
                handle_stream_chunk(&chunk_val, &mut full, &mut truncated, on_token)?;
            }
        }

        // Process leftover buffer
        let line = line_buffer.trim();
        if !line.is_empty() {
            if let Some(json_str) = line.strip_prefix("data: ") {
                if json_str.trim() != "[DONE]" {
                    if let Ok(chunk_val) = serde_json::from_str::<serde_json::Value>(json_str) {
                        handle_stream_chunk(&chunk_val, &mut full, &mut truncated, on_token)?;
                    }
                }
            } else {
                raw.push_str(line);
            }
        }

        Ok(())
    })?;

    if !saw_sse && full.is_empty() {
        if let Ok(resp) = serde_json::from_str::<serde_json::Value>(&raw) {
            let text = parse_completion(&resp)?;
            // Named distinctly from the outer `truncated` accumulator (unused on
            // this fallback path, which returns before that binding is ever read):
            // this is computed from a completely different response (the raw
            // buffer, not any SSE chunk), so it must not be conflated with it.
            let fallback_truncated = finish_reason(&resp).as_deref() == Some("length");
            // Honor cancel on the non-SSE fallback too: a server that ignores
            // stream:true and returns one JSON body must not deliver a page the
            // caller already asked to stop.
            if !on_token(&text) {
                return Err("stopped".into());
            }
            return Ok(OcrResult {
                text,
                truncated: fallback_truncated,
            });
        }
    }
    Ok(OcrResult {
        text: full,
        truncated,
    })
}

/// Pull the assistant message text out of an OpenAI-style chat completion.
pub(crate) fn parse_completion(resp: &serde_json::Value) -> Res<String> {
    if let Some(msg) = provider_error(resp) {
        return Err(format!("provider error: {msg}").into());
    }
    resp["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("unexpected response shape: {resp}").into())
}

fn provider_error(val: &serde_json::Value) -> Option<String> {
    val.get("error")?
        .get("message")?
        .as_str()
        .map(|s| s.to_string())
}

/// Pull `choices[0].finish_reason` out of a chat-completion response or SSE
/// chunk. `"length"` means generation stopped because it hit `max_tokens`
/// rather than a natural stop -- the signal `OcrResult::truncated` is built
/// from.
fn finish_reason(val: &serde_json::Value) -> Option<String> {
    val["choices"][0]["finish_reason"]
        .as_str()
        .map(|s| s.to_string())
}
