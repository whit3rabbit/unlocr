// Manage one llama-server process: spawn, wait for /health, send chat
// completions, and kill it on drop.

use crate::Res;
use std::io::Read;
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
    pub fn start(bin: &Path, model: &Path, mmproj: &Path, port: u16) -> Res<Server> {
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

            let mut child = Command::new(bin)
                .arg("-m").arg(model)
                .arg("--mmproj").arg(mmproj)
                .arg("--host").arg("127.0.0.1")
                .arg("--port").arg(current_port.to_string())
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
    pub fn ocr_image(&self, prompt: &str, data_uri: &str, max_tokens: u32) -> Res<String> {
        ocr_via(
            &format!("http://127.0.0.1:{}", self.port),
            None,
            prompt,
            data_uri,
            max_tokens,
        )
    }
}

/// Anything that can OCR one image given a prompt. Lets the page loop in
/// `lib::ocr_pages` drive either a local `Server` or a `RemoteEndpoint` without
/// caring which. The body it sends is provider-agnostic (OpenAI chat-completions).
pub trait ImageOcr {
    fn ocr_image(&self, prompt: &str, data_uri: &str, max_tokens: u32) -> Res<String>;
}

impl ImageOcr for Server {
    fn ocr_image(&self, prompt: &str, data_uri: &str, max_tokens: u32) -> Res<String> {
        Server::ocr_image(self, prompt, data_uri, max_tokens)
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
    fn ocr_image(&self, prompt: &str, data_uri: &str, max_tokens: u32) -> Res<String> {
        ocr_via(
            self.base_url.trim_end_matches('/'),
            self.api_key.as_deref(),
            prompt,
            data_uri,
            max_tokens,
        )
    }
}

/// POST one image + prompt to an OpenAI-compatible `{base_url}/v1/chat/completions`
/// and return the assistant text. Shared by the local `Server` and `RemoteEndpoint`;
/// the only difference is the base URL and an optional bearer token.
fn ocr_via(
    base_url: &str,
    api_key: Option<&str>,
    prompt: &str,
    data_uri: &str,
    max_tokens: u32,
) -> Res<String> {
    let url = format!("{base_url}/v1/chat/completions");
    let body = serde_json::json!({
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
    let mut req = ureq::post(&url).timeout(Duration::from_secs(600));
    if let Some(key) = api_key {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }
    let resp: serde_json::Value = req.send_json(body)?.into_json()?;
    parse_completion(&resp)
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
    use super::{parse_completion, ImageOcr, RemoteEndpoint};
    use serde_json::json;

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
        };
        let out = ep
            .ocr_image("<|grounding|>x", "data:image/png;base64,AAAA", 64)
            .expect("remote ocr");
        assert_eq!(out, "# remote ok");

        let (request_line, auth) = rx.recv().expect("stub recorded request");
        assert_eq!(request_line, "POST /v1/chat/completions HTTP/1.1");
        assert_eq!(auth.as_deref(), Some("bearer secret"));
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
