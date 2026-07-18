//! Local MLX backend: manages an `mlxcel-server` process (github.com/lablup/mlxcel),
//! a native Rust MLX runtime with built-in "unlimited-ocr" architecture support --
//! including the R-SWA sliding-window decode cache natively, unlike the llama.cpp
//! path which needs an unmerged patch (PR #24975) for the same feature. Apple
//! Silicon only. `mlxcel-server` speaks the same OpenAI-compatible
//! `/v1/chat/completions` + `/health` surface as llama-server, so this mirrors
//! `local::Server`'s process-lifecycle shape and reuses `ocr_via`/`ocr_via_stream`
//! unmodified -- only the spawn args differ (an HF repo id via `-m`, not local
//! GGUF file paths).

use super::{ocr_via, ocr_via_stream, ImageOcr, OcrResult};
use crate::Res;
use std::ffi::OsString;
use std::io::{Read, Seek, SeekFrom};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long to wait for mlxcel-server to become healthy. Deliberately NOT the
/// shared `super::HEALTH_TIMEOUT` (180s): that constant covers llama-server's
/// RAM-load-only wait (the GGUF is already downloaded by the time `Server::start`
/// runs). mlxcel-server's first run downloads the multi-GB safetensors model
/// itself, INSIDE this same wait (see the module doc); 180s is nowhere near
/// enough on a slow connection, and the whole wait was previously silent, so a
/// slow-but-fine download looked identical to a hang. 30 minutes gives real
/// headroom; `on_tick` (below) gives live feedback for whatever duration it takes.
const MLX_HEALTH_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// How often `await_health` calls `on_tick` with (elapsed, stderr tail) while
/// waiting, so a caller (CLI println, GUI `ocr://status` emit) can show the
/// wait is progressing instead of a static, unchanging message.
const TICK_INTERVAL: Duration = Duration::from_secs(3);

/// Default `--mlx-model` / GUI MLX quant when none is given: the 8bit MLX
/// quant (CER 1.572, 205 tok/s, ~5GB mem), the balanced default among the
/// four published quants (4bit/8bit/mxfp4/mxfp8). Shared by the CLI
/// (`cli_args::MLX_DEFAULT_MODEL`) and the GUI so both surfaces agree.
pub const DEFAULT_MODEL: &str = "sahilchachra/unlimited-ocr-8bit-mlx";

/// True when this build targets a platform `mlxcel-server` actually ships a
/// binary for (macOS on Apple Silicon; see the mac-aarch64-only `ToolPin` in
/// `tools::mod`). Compile-time, matching the rest of the codebase's
/// `cfg!(target_os)`-everywhere convention (the GUI ships per-platform
/// builds, so this never needs a runtime check).
pub const fn platform_supported() -> bool {
    cfg!(target_os = "macos") && cfg!(target_arch = "aarch64")
}

/// Recommend an MLX quant repo id by available RAM, mirroring
/// `preflight::sysreq::recommend_quant`'s GGUF tiers so the two surfaces (GGUF
/// quality tier / MLX quant) agree at the same RAM thresholds. Benchmarks
/// (published on the sahilchachra/unlimited-ocr-*-mlx model cards): 4bit ~3.7GB
/// peak mem/CER 2.29, 8bit ~5.1GB/CER 1.57, mxfp8 ~5.0GB/CER 1.46 (best
/// accuracy among the sub-fp16 quants; no fp16 MLX build is published).
pub fn recommend_model(ram_bytes: Option<u64>) -> &'static str {
    const GIB: u64 = 1024 * 1024 * 1024;
    match ram_bytes {
        Some(b) if b >= 16 * GIB => "sahilchachra/unlimited-ocr-mxfp8-mlx",
        Some(b) if b >= 8 * GIB => DEFAULT_MODEL,
        _ => "sahilchachra/unlimited-ocr-4bit-mlx",
    }
}

/// Manages the local mlxcel-server process lifecycle.
pub struct MlxServer {
    child: Child,
    #[allow(dead_code)]
    stderr_log: tempfile::NamedTempFile,
    /// The port on which the server is running.
    pub port: u16,
}

fn await_health(
    child: &mut Child,
    stderr_log: &tempfile::NamedTempFile,
    port: u16,
    on_tick: &mut dyn FnMut(Duration, &str),
) -> Res<()> {
    let url = format!("http://127.0.0.1:{}/health", port);
    let start = Instant::now();
    let deadline = start + MLX_HEALTH_TIMEOUT;
    let mut last_tick = start;
    loop {
        // If the process already exited, the model failed to load.
        if let Some(status) = child.try_wait()? {
            return Err(format!(
                "mlxcel-server exited ({status}) before becoming healthy.\n\
                 --- mlxcel-server stderr ---\n{}",
                read_stderr_log(stderr_log)
            )
            .into());
        }
        if ureq::get(&url)
            .timeout(Duration::from_secs(2))
            .call()
            .is_ok()
        {
            return Ok(());
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(format!(
                "mlxcel-server did not become healthy within {}s.\n\
                 --- mlxcel-server stderr ---\n{}",
                MLX_HEALTH_TIMEOUT.as_secs(),
                read_stderr_log(stderr_log)
            )
            .into());
        }
        if now.duration_since(last_tick) >= TICK_INTERVAL {
            on_tick(now.duration_since(start), &read_stderr_log(stderr_log));
            last_tick = now;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn read_stderr_log(stderr_log: &tempfile::NamedTempFile) -> String {
    // Read only the last TAIL_BYTES rather than the whole file: await_health calls
    // this every TICK_INTERVAL for up to MLX_HEALTH_TIMEOUT (30 min), and the log
    // grows during the first-run multi-GB model download. Bounded seek keeps each
    // tick O(TAIL_BYTES) instead of O(file). read to bytes + from_utf8_lossy since
    // the seek can land mid-UTF-8 (from_utf8 would error there and drop the tail).
    const TAIL_BYTES: u64 = 16 * 1024;
    let mut buf = Vec::new();
    if let Ok(mut f) = stderr_log.reopen() {
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len > TAIL_BYTES {
            let _ = f.seek(SeekFrom::Start(len - TAIL_BYTES));
        }
        let _ = f.read_to_end(&mut buf);
    }
    let s = String::from_utf8_lossy(&buf);
    // keep the tail; startup logs can be long (incl. first-run HF model download)
    let tail: String = s
        .lines()
        .rev()
        .take(20)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    tail
}

impl MlxServer {
    /// OS process id of the spawned mlxcel-server. Mirrors `local::Server::pid`;
    /// unused by the CLI (server is dropped, not killed by pid).
    #[allow(dead_code)]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Spawn mlxcel-server against `model_repo` (a Hugging Face repo id, e.g.
    /// "sahilchachra/unlimited-ocr-8bit-mlx"). mlxcel resolves and caches the
    /// model itself on first run (`$MLXCEL_CACHE_DIR`, default
    /// `~/.cache/mlxcel`) -- no local GGUF-style file management needed here,
    /// but that download happens INSIDE this call before `/health` responds,
    /// which can take minutes; `on_tick(elapsed, stderr_tail)` fires every
    /// `TICK_INTERVAL` so the caller can show live progress instead of a
    /// static "starting..." message for the whole wait.
    pub fn start(
        bin: &std::path::Path,
        model_repo: &str,
        port: u16,
        on_tick: &mut dyn FnMut(Duration, &str),
    ) -> Res<MlxServer> {
        let max_attempts = if port == 0 { 5 } else { 1 };
        let mut last_err = None;

        for attempt in 1..=max_attempts {
            let current_port = if port == 0 {
                match super::free_port() {
                    Ok(p) => p,
                    Err(e) => {
                        last_err = Some(e);
                        continue;
                    }
                }
            } else {
                port
            };

            let stderr_log = tempfile::NamedTempFile::new()?;
            let stderr_handle = stderr_log.reopen()?;

            let mut cmd = Command::new(bin);
            cmd.args(server_args(model_repo, current_port));
            #[cfg(target_os = "linux")]
            {
                use std::os::unix::process::CommandExt;
                unsafe {
                    cmd.pre_exec(|| {
                        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                        Ok(())
                    });
                }
            }

            let mut child = cmd
                .stdout(Stdio::null())
                .stderr(Stdio::from(stderr_handle))
                .spawn()
                .map_err(|e| format!("failed to launch mlxcel-server: {e}"))?;

            match await_health(&mut child, &stderr_log, current_port, on_tick) {
                Ok(()) => {
                    return Ok(MlxServer {
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
                        || err_str.contains("already in use");

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

        Err(last_err.unwrap_or_else(|| "Failed to start mlxcel-server after retries".into()))
    }

    /// Send one image + prompt, return the model's markdown.
    #[allow(clippy::too_many_arguments)]
    pub fn ocr_image(
        &self,
        prompt: &str,
        data_uri: &str,
        max_tokens: u32,
        temperature: Option<f32>,
        repeat_penalty: Option<f32>,
        dry_multiplier: Option<f32>,
        dry_base: Option<f32>,
        dry_allowed_length: Option<u32>,
        dry_penalty_last_n: Option<i32>,
    ) -> Res<OcrResult> {
        ocr_via(
            &format!("http://127.0.0.1:{}", self.port),
            None,
            None,
            prompt,
            data_uri,
            max_tokens,
            temperature.unwrap_or(0.0),
            repeat_penalty,
            dry_multiplier,
            dry_base,
            dry_allowed_length,
            dry_penalty_last_n,
        )
    }
}

/// Build the mlxcel-server argument vector (everything after the binary path).
/// Split out so the arg wiring is unit-testable without spawning a process.
pub(crate) fn server_args(model_repo: &str, port: u16) -> Vec<OsString> {
    vec![
        "-m".into(),
        model_repo.into(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string().into(),
    ]
}

impl ImageOcr for MlxServer {
    fn ocr_image(
        &self,
        prompt: &str,
        data_uri: &str,
        max_tokens: u32,
        temperature: Option<f32>,
        repeat_penalty: Option<f32>,
        dry_multiplier: Option<f32>,
        dry_base: Option<f32>,
        dry_allowed_length: Option<u32>,
        dry_penalty_last_n: Option<i32>,
    ) -> Res<OcrResult> {
        MlxServer::ocr_image(
            self,
            prompt,
            data_uri,
            max_tokens,
            temperature,
            repeat_penalty,
            dry_multiplier,
            dry_base,
            dry_allowed_length,
            dry_penalty_last_n,
        )
    }

    fn ocr_image_stream(
        &self,
        prompt: &str,
        data_uri: &str,
        max_tokens: u32,
        temperature: Option<f32>,
        repeat_penalty: Option<f32>,
        dry_multiplier: Option<f32>,
        dry_base: Option<f32>,
        dry_allowed_length: Option<u32>,
        dry_penalty_last_n: Option<i32>,
        on_token: &mut dyn FnMut(&str) -> bool,
        should_cancel: &dyn Fn() -> bool,
    ) -> Res<OcrResult> {
        ocr_via_stream(
            &format!("http://127.0.0.1:{}", self.port),
            None,
            None,
            prompt,
            data_uri,
            max_tokens,
            temperature.unwrap_or(0.0),
            repeat_penalty,
            dry_multiplier,
            dry_base,
            dry_allowed_length,
            dry_penalty_last_n,
            on_token,
            should_cancel,
        )
    }
}

impl Drop for MlxServer {
    fn drop(&mut self) {
        // See `local::Server`'s Drop for the caveats (Ctrl-C/SIGKILL/panic skip
        // Drop; macOS has no kernel kill-on-parent-death backstop). Recover a
        // stranded process with `pkill mlxcel-server`.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_args_shape() {
        let args = server_args("sahilchachra/unlimited-ocr-8bit-mlx", 8080);
        assert_eq!(
            args,
            vec![
                OsString::from("-m"),
                OsString::from("sahilchachra/unlimited-ocr-8bit-mlx"),
                OsString::from("--host"),
                OsString::from("127.0.0.1"),
                OsString::from("--port"),
                OsString::from("8080"),
            ]
        );
    }

    #[test]
    fn recommend_model_tiers_by_ram() {
        const GIB: u64 = 1024 * 1024 * 1024;
        assert_eq!(
            recommend_model(Some(16 * GIB)),
            "sahilchachra/unlimited-ocr-mxfp8-mlx"
        );
        assert_eq!(recommend_model(Some(8 * GIB)), DEFAULT_MODEL);
        assert_eq!(
            recommend_model(Some(4 * GIB)),
            "sahilchachra/unlimited-ocr-4bit-mlx"
        );
        assert_eq!(recommend_model(None), "sahilchachra/unlimited-ocr-4bit-mlx");
    }
}
