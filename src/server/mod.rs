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

/// Anything that can OCR one image given a prompt. Lets the page loop in
/// `lib::ocr_pages` drive either a local `Server` or a `RemoteEndpoint` without
/// caring which. The body it sends is provider-agnostic (OpenAI chat-completions).
pub trait ImageOcr {
    fn ocr_image(
        &self,
        prompt: &str,
        data_uri: &str,
        max_tokens: u32,
        repeat_penalty: Option<f32>,
    ) -> Res<String>;

    /// Stream completions: like `ocr_image` but calls `on_token` with each partial
    /// text chunk as it arrives (SSE delta), and returns the full assembled text.
    /// The default falls back to the non-streaming path (safe for stub servers
    /// and callers that do not need partial-token events).
    fn ocr_image_stream(
        &self,
        prompt: &str,
        data_uri: &str,
        max_tokens: u32,
        repeat_penalty: Option<f32>,
        on_token: &mut dyn FnMut(&str) -> bool,
        should_cancel: &dyn Fn() -> bool,
    ) -> Res<String> {
        let _ = on_token; // default: ignore the sink
        let _ = should_cancel;
        self.ocr_image(prompt, data_uri, max_tokens, repeat_penalty)
    }
}

/// Attach the optional llama.cpp `repeat_penalty` extension to a request body.
/// No-op when None so the baseline body is byte-for-byte unchanged. Shared by the
/// streaming and non-streaming request builders.
pub(crate) fn apply_repeat_penalty(body: &mut serde_json::Value, repeat_penalty: Option<f32>) {
    if let Some(rp) = repeat_penalty {
        body["repeat_penalty"] = serde_json::json!(rp);
    }
}

/// POST one image + prompt to an OpenAI-compatible `{base_url}/v1/chat/completions`
/// and return the assistant text. Shared by the local `Server` and `RemoteEndpoint`;
/// the only difference is the base URL and an optional bearer token.
pub(crate) fn ocr_via(
    base_url: &str,
    api_key: Option<&str>,
    model: Option<&str>,
    prompt: &str,
    data_uri: &str,
    max_tokens: u32,
    repeat_penalty: Option<f32>,
) -> Res<String> {
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
    apply_repeat_penalty(&mut body, repeat_penalty);
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
        parse_completion(&resp_json)
    })
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
    on_token: &mut dyn FnMut(&str) -> bool,
    should_cancel: &dyn Fn() -> bool,
) -> Res<String> {
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
    apply_repeat_penalty(&mut body, repeat_penalty);
    let api_key = api_key.map(|s| s.to_string());

    let mut full = String::new();
    let mut saw_sse = false;
    let mut raw = String::new();

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
                // A 2xx stream can still carry a per-chunk provider error, e.g.
                // {"error":{"message":"Context length exceeded"}}. Surface it
                // instead of silently finishing with whatever partial text we have.
                if let Some(msg) = provider_error(&chunk_val) {
                    return Err(format!("provider error: {msg}").into());
                }
                if let Some(token) = chunk_val["choices"][0]["delta"]["content"].as_str() {
                    if !on_token(token) {
                        return Err(Box::<dyn std::error::Error>::from("stopped"));
                    }
                    full.push_str(token);
                }
            }
        }

        // Process leftover buffer
        let line = line_buffer.trim();
        if !line.is_empty() {
            if let Some(json_str) = line.strip_prefix("data: ") {
                if json_str.trim() != "[DONE]" {
                    if let Ok(chunk_val) = serde_json::from_str::<serde_json::Value>(json_str) {
                        if let Some(msg) = provider_error(&chunk_val) {
                            return Err(format!("provider error: {msg}").into());
                        }
                        if let Some(token) = chunk_val["choices"][0]["delta"]["content"].as_str() {
                            if !on_token(token) {
                                return Err(Box::<dyn std::error::Error>::from("stopped"));
                            }
                            full.push_str(token);
                        }
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
            // Honor cancel on the non-SSE fallback too: a server that ignores
            // stream:true and returns one JSON body must not deliver a page the
            // caller already asked to stop.
            if !on_token(&text) {
                return Err("stopped".into());
            }
            return Ok(text);
        }
    }
    Ok(full)
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
