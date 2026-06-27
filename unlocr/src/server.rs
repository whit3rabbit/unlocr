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

impl Server {
    pub fn start(bin: &Path, model: &Path, mmproj: &Path, port: u16) -> Res<Server> {
        // Capture stderr to a temp file so we can show the real error if the
        // model fails to load (e.g. a llama.cpp build without DeepSeek-OCR).
        let stderr_log = tempfile::NamedTempFile::new()?;
        let stderr_handle = stderr_log.reopen()?;

        let child = Command::new(bin)
            .arg("-m").arg(model)
            .arg("--mmproj").arg(mmproj)
            .arg("--host").arg("127.0.0.1")
            .arg("--port").arg(port.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr_handle))
            .spawn()
            .map_err(|e| format!("failed to launch llama-server: {e}"))?;

        let mut srv = Server { child, stderr_log, port };
        srv.await_health()?;
        Ok(srv)
    }

    fn await_health(&mut self) -> Res<()> {
        let url = format!("http://127.0.0.1:{}/health", self.port);
        let deadline = Instant::now() + HEALTH_TIMEOUT;
        loop {
            // If the process already exited, the model failed to load.
            if let Some(status) = self.child.try_wait()? {
                return Err(format!(
                    "llama-server exited ({status}) before becoming healthy. \
                     Likely an old build without DeepSeek-OCR support \
                     (run `brew upgrade llama.cpp`).\n--- llama-server stderr ---\n{}",
                    self.read_stderr()
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
                    self.read_stderr()
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    fn read_stderr(&self) -> String {
        let mut s = String::new();
        if let Ok(mut f) = self.stderr_log.reopen() {
            let _ = f.read_to_string(&mut s);
        }
        // keep the tail; startup logs can be long
        let tail: String = s.lines().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        tail
    }

    /// Send one image + prompt, return the model's markdown.
    pub fn ocr_image(&self, prompt: &str, data_uri: &str, max_tokens: u32) -> Res<String> {
        let url = format!("http://127.0.0.1:{}/v1/chat/completions", self.port);
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
        let resp: serde_json::Value = ureq::post(&url)
            .timeout(Duration::from_secs(600))
            .send_json(body)?
            .into_json()?;
        resp["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| format!("unexpected response shape: {resp}").into())
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
