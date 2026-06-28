// Manage one llama-server process: spawn, wait for /health, send chat
// completions, and kill it on drop.

use crate::Res;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

// First model load (incl. mmproj) can be slow; give it room.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(180);

pub struct Server {
    child: Child,
    #[allow(dead_code)]
    stderr_log: tempfile::NamedTempFile,
    pub port: u16,
}

/// Grab a free port by binding to :0 and immediately releasing it.
// ponytail: tiny TOCTOU window between release and llama-server bind; fine for
// a local single-user CLI. Pass --port to pin if it ever matters.
pub fn free_port() -> Res<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn await_health(child: &mut Child, stderr_log: &tempfile::NamedTempFile, port: u16) -> Res<()> {
    let url = format!("http://127.0.0.1:{}/health", port);
    let deadline = Instant::now() + HEALTH_TIMEOUT;
    loop {
        // If the process already exited, the model failed to load.
        if let Some(status) = child.try_wait()? {
            return Err(format!(
                "llama-server exited ({status}) before becoming healthy. \
                 Likely an old build without DeepSeek-OCR support \
                 (run `brew upgrade llama.cpp`).\n--- llama-server stderr ---\n{}",
                read_stderr_log(stderr_log)
            )
            .into());
        }
        if ureq::get(&url).timeout(Duration::from_secs(2)).call().is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "llama-server did not become healthy within {}s.\n--- llama-server stderr ---\n{}",
                HEALTH_TIMEOUT.as_secs(),
                read_stderr_log(stderr_log)
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn read_stderr_log(stderr_log: &tempfile::NamedTempFile) -> String {
    let mut s = String::new();
    if let Ok(mut f) = stderr_log.reopen() {
        let _ = f.read_to_string(&mut s);
    }
    // keep the tail; startup logs can be long
    let tail: String = s.lines().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    tail
}

impl Server {
    /// OS process id of the spawned llama-server. The GUI stashes this so it can
    /// kill the server out-of-band (Stop) without holding the lock that owns the
    /// `Server`. Unused by the CLI (server is dropped, not killed by pid).
    #[allow(dead_code)]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Spawn llama-server. `image_max_tokens` (Some) adds `--image-max-tokens N`
    /// (DeepSeek-OCR detail knob); `chat_template` (Some) adds `--chat-template
    /// <name>` (e.g. "deepseek-ocr"). Both are inert when None, so the default
    /// command line is byte-for-byte what it was before these knobs existed.
    pub fn start(
        bin: &Path,
        model: &Path,
        mmproj: &Path,
        port: u16,
        image_max_tokens: Option<u32>,
        chat_template: Option<&str>,
    ) -> Res<Server> {
        let max_attempts = if port == 0 { 5 } else { 1 };
        let mut last_err = None;

        for attempt in 1..=max_attempts {
            let current_port = if port == 0 {
                match free_port() {
                    Ok(p) => p,
                    Err(e) => {
                        last_err = Some(e);
                        continue;
                    }
                }
            } else {
                port
            };

            // Capture stderr to a temp file so we can show the real error if the
            // model fails to load (e.g. a llama.cpp build without DeepSeek-OCR).
            let stderr_log = tempfile::NamedTempFile::new()?;
            let stderr_handle = stderr_log.reopen()?;

            let mut cmd = Command::new(bin);
            cmd.args(server_args(model, mmproj, current_port, image_max_tokens, chat_template));
            let mut child = cmd
                .stdout(Stdio::null())
                .stderr(Stdio::from(stderr_handle))
                .spawn()
                .map_err(|e| format!("failed to launch llama-server: {e}"))?;

            match await_health(&mut child, &stderr_log, current_port) {
                Ok(()) => {
                    return Ok(Server {
                        child,
                        stderr_log,
                        port: current_port,
                    });
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();

                    let err_str = e.to_string();
                    let is_bind_error = err_str.contains("address already in use")
                        || err_str.contains("Address already in use")
                        || err_str.contains("bind failed")
                        || err_str.contains("port already in use")
                        || err_str.contains("already in use")
                        || err_str.contains("WSAEADDRINUSE");

                    last_err = Some(e);

                    if !is_bind_error {
                        break;
                    }
                    if attempt < max_attempts && port == 0 {
                        eprintln!(
                            "warning: port {current_port} failed to bind (attempt {attempt}/{max_attempts}). Retrying..."
                        );
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| "Failed to start llama-server after retries".into()))
    }

    #[allow(dead_code)]
    fn read_stderr(&self) -> String {
        read_stderr_log(&self.stderr_log)
    }

    /// Send one image + prompt, return the model's markdown.
    pub fn ocr_image(
        &self,
        prompt: &str,
        data_uri: &str,
        max_tokens: u32,
        repeat_penalty: Option<f32>,
    ) -> Res<String> {
        ocr_via(
            &format!("http://127.0.0.1:{}", self.port),
            None,
            None,
            prompt,
            data_uri,
            max_tokens,
            repeat_penalty,
        )
    }
}

/// Build the llama-server argument vector (everything after the binary path).
/// Split out so the optional-flag wiring (`--image-max-tokens`, `--chat-template`)
/// is unit-testable without spawning a process. Returns `OsString`s so model /
/// mmproj paths are passed losslessly (a non-UTF8 cache path must not be mangled
/// into U+FFFD the way `to_string_lossy` would).
fn server_args(
    model: &Path,
    mmproj: &Path,
    port: u16,
    image_max_tokens: Option<u32>,
    chat_template: Option<&str>,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "-m".into(),
        model.as_os_str().to_owned(),
        "--mmproj".into(),
        mmproj.as_os_str().to_owned(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string().into(),
    ];
    // Optional DeepSeek-OCR knobs; omitted entirely when None so the baseline
    // invocation is byte-for-byte unchanged.
    if let Some(n) = image_max_tokens {
        args.push("--image-max-tokens".into());
        args.push(n.to_string().into());
    }
    if let Some(tmpl) = chat_template {
        args.push("--chat-template".into());
        args.push(tmpl.into());
    }
    args
}

/// Attach the optional llama.cpp `repeat_penalty` extension to a request body.
/// No-op when None so the baseline body is byte-for-byte unchanged. Shared by the
/// streaming and non-streaming request builders.
fn apply_repeat_penalty(body: &mut serde_json::Value, repeat_penalty: Option<f32>) {
    if let Some(rp) = repeat_penalty {
        body["repeat_penalty"] = serde_json::json!(rp);
    }
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
        on_token: &mut dyn FnMut(&str),
    ) -> Res<String> {
        let _ = on_token; // default: ignore the sink
        self.ocr_image(prompt, data_uri, max_tokens, repeat_penalty)
    }
}

impl ImageOcr for Server {
    fn ocr_image(
        &self,
        prompt: &str,
        data_uri: &str,
        max_tokens: u32,
        repeat_penalty: Option<f32>,
    ) -> Res<String> {
        Server::ocr_image(self, prompt, data_uri, max_tokens, repeat_penalty)
    }

    fn ocr_image_stream(
        &self,
        prompt: &str,
        data_uri: &str,
        max_tokens: u32,
        repeat_penalty: Option<f32>,
        on_token: &mut dyn FnMut(&str),
    ) -> Res<String> {
        ocr_via_stream(
            &format!("http://127.0.0.1:{}", self.port),
            None,
            None,
            prompt,
            data_uri,
            max_tokens,
            repeat_penalty,
            on_token,
        )
    }
}

/// A remote OpenAI-compatible chat-completions endpoint (remote llama-server,
/// vLLM, LM Studio, a hosted gateway, ...). Rasterization still happens locally;
/// only the per-image inference call goes here.
pub struct RemoteEndpoint {
    /// Base URL with no trailing slash, e.g. `https://host:8080`. `/v1/chat/completions`
    /// is appended for inference, `/v1/models` for the load-time probe.
    pub base_url: String,
    /// Optional bearer token sent as `Authorization: Bearer <key>`.
    pub api_key: Option<String>,
    /// Optional model name placed in the request body's `"model"` field. Required
    /// by multi-model gateways (litellm, vLLM); a bare remote llama-server ignores
    /// it, so `None` is fine there.
    pub model: Option<String>,
}

impl RemoteEndpoint {
    /// Cheap reachability check used at load time: GET `{base}/v1/models`. Returns
    /// Ok on any HTTP response; the caller treats an Err as "could not reach the
    /// endpoint" (warn, do not hard-fail, since some servers omit /v1/models).
    pub fn probe(&self) -> Res<()> {
        let url = format!("{}/v1/models", self.base_url.trim_end_matches('/'));
        let mut req = ureq::get(&url).timeout(Duration::from_secs(10));
        if let Some(key) = &self.api_key {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
        req.call()?;
        Ok(())
    }
}

impl ImageOcr for RemoteEndpoint {
    fn ocr_image(
        &self,
        prompt: &str,
        data_uri: &str,
        max_tokens: u32,
        repeat_penalty: Option<f32>,
    ) -> Res<String> {
        ocr_via(
            self.base_url.trim_end_matches('/'),
            self.api_key.as_deref(),
            self.model.as_deref(),
            prompt,
            data_uri,
            max_tokens,
            repeat_penalty,
        )
    }

    fn ocr_image_stream(
        &self,
        prompt: &str,
        data_uri: &str,
        max_tokens: u32,
        repeat_penalty: Option<f32>,
        on_token: &mut dyn FnMut(&str),
    ) -> Res<String> {
        ocr_via_stream(
            self.base_url.trim_end_matches('/'),
            self.api_key.as_deref(),
            self.model.as_deref(),
            prompt,
            data_uri,
            max_tokens,
            repeat_penalty,
            on_token,
        )
    }
}

/// POST one image + prompt to an OpenAI-compatible `{base_url}/v1/chat/completions`
/// and return the assistant text. Shared by the local `Server` and `RemoteEndpoint`;
/// the only difference is the base URL and an optional bearer token.
fn ocr_via(
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
    // Only multi-model gateways (litellm, vLLM) need a "model" field; a bare
    // llama-server ignores it. Inject it only when the caller supplied one.
    if let Some(m) = model {
        body["model"] = serde_json::Value::String(m.to_string());
    }
    // Non-OpenAI extension honored by llama.cpp's server; omitted when None so the
    // request body is unchanged unless the caller opts in.
    apply_repeat_penalty(&mut body, repeat_penalty);
    let mut req = ureq::post(&url).timeout(Duration::from_secs(600));
    if let Some(key) = api_key {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }
    let resp: serde_json::Value = req.send_json(body)?.into_json()?;
    parse_completion(&resp)
}

/// POST with `stream: true` and consume the SSE response, calling `on_token`
/// for each `choices[0].delta.content` chunk. Returns the full assembled text.
/// Lines that are not `data: {...}` (keep-alive `:` or the terminal `data: [DONE]`)
/// are silently skipped. On a parse failure for a single chunk, the chunk is
/// skipped (best-effort) and the loop continues so a single corrupt line does not
/// abort a long OCR run.
fn ocr_via_stream(
    base_url: &str,
    api_key: Option<&str>,
    model: Option<&str>,
    prompt: &str,
    data_uri: &str,
    max_tokens: u32,
    repeat_penalty: Option<f32>,
    on_token: &mut dyn FnMut(&str),
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
    let mut req = ureq::post(&url).timeout(Duration::from_secs(600));
    if let Some(key) = api_key {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }
    let resp = req.send_json(body)?;
    let reader = BufReader::new(resp.into_reader());
    let mut full = String::new();
    // Track whether we ever saw an SSE `data:` line. Some OpenAI-compatible
    // servers ignore `stream: true` and reply with one plain JSON completion
    // body; without this flag those replies would be silently dropped (every
    // line fails strip_prefix) and we would return an empty string.
    let mut saw_sse = false;
    let mut raw = String::new();
    for line in reader.lines() {
        let line = line?;
        // SSE lines begin with "data: "; everything else is a comment or blank.
        let Some(json_str) = line.strip_prefix("data: ") else {
            // Keep the raw body around for the non-streaming fallback below.
            raw.push_str(&line);
            continue;
        };
        saw_sse = true;
        // Terminal sentinel sent by llama-server at end-of-stream.
        if json_str.trim() == "[DONE]" {
            break;
        }
        // Best-effort parse: a single corrupt chunk is skipped, not fatal.
        let Ok(chunk) = serde_json::from_str::<serde_json::Value>(json_str) else {
            continue;
        };
        if let Some(token) = chunk["choices"][0]["delta"]["content"].as_str() {
            on_token(token);
            full.push_str(token);
        }
    }
    // Non-streaming fallback: the server returned a normal JSON completion body
    // (no `data:` lines). Parse it like ocr_via and deliver the text in one shot
    // so callers against a server that ignores `stream: true` still get output
    // instead of an empty result.
    if !saw_sse && full.is_empty() {
        if let Ok(resp) = serde_json::from_str::<serde_json::Value>(&raw) {
            let text = parse_completion(&resp)?;
            on_token(&text);
            return Ok(text);
        }
    }
    Ok(full)
}

/// Pull the assistant message text out of an OpenAI-style chat completion.
/// Split out so it can be tested without a live server.
fn parse_completion(resp: &serde_json::Value) -> Res<String> {
    resp["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("unexpected response shape: {resp}").into())
}

// Test-only constructor: bind `port` to a pre-running stub HTTP server, with
// a dummy sleep child so Server's Drop impl does not panic on kill(). Used by
// the ocr_state_sequence_ordering test in lib.rs to exercise ocr_pages without
// a real llama-server binary.
#[cfg(test)]
impl Server {
    pub(crate) fn for_test(port: u16) -> Res<Self> {
        // A long-sleeping process satisfies the `Child` field; it is killed in
        // Drop, which is benign (the child exits). On Windows use `timeout`
        // with a large value; on Unix `sleep 9999` is universal.
        #[cfg(unix)]
        let child = Command::new("sleep").arg("9999").spawn()
            .map_err(|e| format!("Server::for_test: could not spawn sleep: {e}"))?;
        #[cfg(windows)]
        let child = Command::new("cmd").args(["/C", "timeout", "/t", "9999", "/nobreak"])
            .stdout(Stdio::null()).stderr(Stdio::null()).spawn()
            .map_err(|e| format!("Server::for_test: could not spawn timeout: {e}"))?;
        Ok(Server {
            child,
            stderr_log: tempfile::NamedTempFile::new()?,
            port,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ocr_via_stream, parse_completion, server_args, ImageOcr, RemoteEndpoint};
    use serde_json::json;
    use std::ffi::OsString;
    use std::path::Path;

    /// The optional DeepSeek-OCR flags must reach the spawn args only when set,
    /// and the no-flags baseline must stay byte-for-byte what it was. Mirrors the
    /// EH-0002 "prove the flag reaches the subprocess" pattern, no network/spawn.
    #[test]
    fn server_args_adds_optional_flags_only_when_set() {
        let model = Path::new("/m/model.gguf");
        let mmproj = Path::new("/m/mmproj.gguf");

        // Test paths are ASCII, so to_string_lossy round-trips losslessly here;
        // the OsString return type matters only for non-UTF8 paths in the wild.
        let stringify = |v: Vec<OsString>| -> Vec<String> {
            v.iter().map(|s| s.to_string_lossy().into_owned()).collect()
        };

        let base = stringify(server_args(model, mmproj, 8080, None, None));
        assert_eq!(
            base,
            vec![
                "-m", "/m/model.gguf",
                "--mmproj", "/m/mmproj.gguf",
                "--host", "127.0.0.1",
                "--port", "8080",
            ]
        );
        assert!(!base.iter().any(|a| a == "--image-max-tokens" || a == "--chat-template"));

        let full = stringify(server_args(model, mmproj, 8080, Some(1280), Some("deepseek-ocr")));
        // Each flag appears adjacent to its value.
        assert!(full.windows(2).any(|w| w == ["--image-max-tokens", "1280"]));
        assert!(full.windows(2).any(|w| w == ["--chat-template", "deepseek-ocr"]));
    }

    #[test]
    fn parses_content() {
        let resp = json!({ "choices": [{ "message": { "content": "# hi" } }] });
        assert_eq!(parse_completion(&resp).unwrap(), "# hi");
    }

    #[test]
    fn rejects_bad_shape() {
        assert!(parse_completion(&json!({})).is_err());
        assert!(parse_completion(&json!({ "choices": [] })).is_err());
        // content present but wrong type
        let bad = json!({ "choices": [{ "message": { "content": 42 } }] });
        assert!(parse_completion(&bad).is_err());
    }

    /// RemoteEndpoint must POST to {base}/v1/chat/completions, send the bearer
    /// token, and return the assistant text. Stub HTTP server captures the
    /// Authorization header + request path so we lock both the routing and auth.
    #[test]
    fn remote_endpoint_sends_bearer_and_parses() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::sync::mpsc;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let port = listener.local_addr().unwrap().port();

        let resp_body = json!({ "choices": [{ "message": { "content": "# remote ok" } }] })
            .to_string();
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            resp_body.len(),
            resp_body,
        );

        // Capture the request line + Authorization header from one request.
        let (tx, rx) = mpsc::channel::<(String, Option<String>)>();
        std::thread::spawn(move || {
            let Ok(s) = listener.incoming().next().unwrap() else { return };
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut writer = s;
            let mut request_line = String::new();
            reader.read_line(&mut request_line).ok();
            let mut auth = None;
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let t = line.trim_end_matches(|c| c == '\r' || c == '\n');
                if t.is_empty() {
                    break;
                }
                let lower = t.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("authorization:") {
                    auth = Some(v.trim().to_string());
                }
                if let Some(v) = lower.strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = reader.read_exact(&mut body);
            let _ = writer.write_all(http_response.as_bytes());
            let _ = tx.send((request_line.trim().to_string(), auth));
        });

        let ep = RemoteEndpoint {
            base_url: format!("http://127.0.0.1:{port}/"), // trailing slash must be trimmed
            api_key: Some("secret".to_string()),
            model: None,
        };
        let out = ep
            .ocr_image("<|grounding|>x", "data:image/png;base64,AAAA", 64, None)
            .expect("remote ocr");
        assert_eq!(out, "# remote ok");

        let (request_line, auth) = rx.recv().expect("stub recorded request");
        assert_eq!(request_line, "POST /v1/chat/completions HTTP/1.1");
        assert_eq!(auth.as_deref(), Some("bearer secret"));
    }

    /// A `model` set on RemoteEndpoint must land in the request body (gateways
    /// like litellm/vLLM require it); when unset, no `"model"` key is sent (a bare
    /// llama-server would reject an empty/unknown model). Stub captures the body.
    #[test]
    fn remote_endpoint_injects_model_only_when_set() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::sync::mpsc;

        fn run_once(model: Option<String>) -> serde_json::Value {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
            let port = listener.local_addr().unwrap().port();
            let resp_body =
                json!({ "choices": [{ "message": { "content": "ok" } }] }).to_string();
            let http_response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                resp_body.len(),
                resp_body,
            );
            let (tx, rx) = mpsc::channel::<Vec<u8>>();
            std::thread::spawn(move || {
                let Ok(s) = listener.incoming().next().unwrap() else { return };
                let mut reader = BufReader::new(s.try_clone().unwrap());
                let mut writer = s;
                let mut content_length = 0usize;
                let mut first = String::new();
                reader.read_line(&mut first).ok();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let t = line.trim_end_matches(|c| c == '\r' || c == '\n');
                    if t.is_empty() {
                        break;
                    }
                    if let Some(v) = t.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; content_length];
                let _ = reader.read_exact(&mut body);
                let _ = writer.write_all(http_response.as_bytes());
                let _ = tx.send(body);
            });

            let ep = RemoteEndpoint {
                base_url: format!("http://127.0.0.1:{port}"),
                api_key: None,
                model,
            };
            ep.ocr_image("p", "data:image/png;base64,AAAA", 64, None).expect("ocr");
            let body = rx.recv().expect("stub recorded body");
            serde_json::from_slice(&body).expect("body is json")
        }

        let with = run_once(Some("my-model".to_string()));
        assert_eq!(with["model"], json!("my-model"));

        let without = run_once(None);
        assert!(without.get("model").is_none(), "model key must be absent when unset");
    }

    /// EH-0010 acceptance: prove `ocr_via_stream` fires `on_token` once per SSE
    /// `data:` chunk and assembles the full text correctly.
    ///
    /// The stub HTTP server returns a proper SSE body with `stream: true` semantics:
    ///   data: {"choices":[{"delta":{"content":"Hello"}}]}
    ///   data: {"choices":[{"delta":{"content":" world"}}]}
    ///   data: [DONE]
    ///
    /// This is the real SSE wire format that llama-server sends. The test verifies:
    ///   1. on_token fires exactly twice, once per chunk.
    ///   2. The assembled return value equals the concatenation of both chunks.
    ///   3. Blank lines and [DONE] are silently skipped (not counted as tokens).
    #[test]
    fn sse_streaming_fires_on_token() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;

        // Build the SSE body: two chunks then [DONE].
        let sse_body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\r\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\r\n",
            "data: [DONE]\r\n",
        );
        // Use Transfer-Encoding: chunked so ureq reads line-by-line without
        // needing a fixed Content-Length for a streaming response.
        // Alternatively, send a fixed-length body — simpler and avoids chunked encoding.
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
            sse_body.len(),
            sse_body,
        );

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let port = listener.local_addr().unwrap().port();

        let http_resp_clone = http_response.clone();
        std::thread::spawn(move || {
            // Serve a single connection with the SSE response, then exit.
            if let Ok(stream) = listener.accept() {
                let (sock, _) = stream;
                let mut reader = BufReader::new(sock.try_clone().expect("clone"));
                let mut writer = sock;
                // Drain the request headers + body.
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let t = line.trim_end_matches(|c| c == '\r' || c == '\n');
                    if t.is_empty() {
                        break;
                    }
                    if t.to_ascii_lowercase().starts_with("content-length:") {
                        if let Some(v) = t.splitn(2, ':').nth(1) {
                            content_length = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0u8; content_length];
                let _ = Read::read_exact(&mut reader, &mut body);
                let _ = writer.write_all(http_resp_clone.as_bytes());
            }
        });

        let base_url = format!("http://127.0.0.1:{port}");
        let mut tokens: Vec<String> = Vec::new();
        let result = ocr_via_stream(
            &base_url,
            None,
            None,
            "test prompt",
            "data:image/png;base64,AAAA",
            64,
            None,
            &mut |chunk: &str| tokens.push(chunk.to_string()),
        );

        assert!(result.is_ok(), "ocr_via_stream failed: {:?}", result.err());
        let assembled = result.unwrap();

        // on_token must fire exactly once per data chunk (2 chunks, not 3 — [DONE] is not a token).
        assert_eq!(
            tokens,
            vec!["Hello".to_string(), " world".to_string()],
            "on_token fired with unexpected chunks: {tokens:?}"
        );
        // Assembled text must be the concatenation of both chunks.
        assert_eq!(
            assembled, "Hello world",
            "assembled text mismatch: {assembled:?}"
        );
    }

    /// Non-SSE fallback: a server that ignores `stream: true` and replies with a
    /// single plain JSON completion body must still yield its text (parsed like
    /// ocr_via) rather than an empty string. Regression guard for the streaming
    /// switch in ocr_pages, which otherwise silently dropped such responses.
    #[test]
    fn stream_falls_back_to_plain_json_completion() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;

        // A normal (non-streaming) OpenAI chat-completion body, no `data:` framing.
        let json_body = r#"{"choices":[{"message":{"content":"plain json ok"}}]}"#;
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            json_body.len(),
            json_body,
        );

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let port = listener.local_addr().unwrap().port();

        let http_resp_clone = http_response.clone();
        std::thread::spawn(move || {
            if let Ok((sock, _)) = listener.accept() {
                let mut reader = BufReader::new(sock.try_clone().expect("clone"));
                let mut writer = sock;
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let t = line.trim_end_matches(|c| c == '\r' || c == '\n');
                    if t.is_empty() {
                        break;
                    }
                    if t.to_ascii_lowercase().starts_with("content-length:") {
                        if let Some(v) = t.splitn(2, ':').nth(1) {
                            content_length = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0u8; content_length];
                let _ = Read::read_exact(&mut reader, &mut body);
                let _ = writer.write_all(http_resp_clone.as_bytes());
            }
        });

        let base_url = format!("http://127.0.0.1:{port}");
        let mut tokens: Vec<String> = Vec::new();
        let result = ocr_via_stream(
            &base_url,
            None,
            None,
            "test prompt",
            "data:image/png;base64,AAAA",
            64,
            None,
            &mut |chunk: &str| tokens.push(chunk.to_string()),
        );

        let assembled = result.expect("ocr_via_stream fallback failed");
        assert_eq!(assembled, "plain json ok", "fallback text mismatch");
        // The fallback delivers the whole body as one on_token call.
        assert_eq!(tokens, vec!["plain json ok".to_string()]);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // ponytail: Drop covers normal + error exit. Ctrl-C (SIGINT) does NOT
        // run Drop, so it can orphan llama-server. Add a `ctrlc` handler if
        // that turns out to bite.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
