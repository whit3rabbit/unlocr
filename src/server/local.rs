use super::{ocr_via, ocr_via_stream, ImageOcr, HEALTH_TIMEOUT};
use crate::Res;
use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Manages the local llama-server process lifecycle.
pub struct Server {
    child: Child,
    #[allow(dead_code)]
    stderr_log: tempfile::NamedTempFile,
    /// The port on which the server is running.
    pub port: u16,
    #[cfg(windows)]
    #[allow(dead_code)]
    job_handle: Option<win_job::JobHandle>,
}

#[cfg(windows)]
mod win_job {
    use std::os::windows::io::{AsRawHandle, RawHandle};
    use std::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub struct JobHandle(HANDLE);

    unsafe impl Send for JobHandle {}
    unsafe impl Sync for JobHandle {}

    impl Drop for JobHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    pub fn bind_child(child: &Child) -> Result<Option<JobHandle>, String> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err("CreateJobObjectW failed".into());
            }

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            let res = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            );
            if res == 0 {
                CloseHandle(job);
                return Err("SetInformationJobObject failed".into());
            }

            let child_handle = child.as_raw_handle() as HANDLE;
            let res = AssignProcessToJobObject(job, child_handle);
            if res == 0 {
                CloseHandle(job);
                return Err("AssignProcessToJobObject failed".into());
            }

            Ok(Some(JobHandle(job)))
        }
    }
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
        if ureq::get(&url)
            .timeout(Duration::from_secs(2))
            .call()
            .is_ok()
        {
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

            // Capture stderr to a temp file so we can show the real error if the
            // model fails to load (e.g. a llama.cpp build without DeepSeek-OCR).
            let stderr_log = tempfile::NamedTempFile::new()?;
            let stderr_handle = stderr_log.reopen()?;

            let mut cmd = Command::new(bin);
            cmd.args(server_args(
                model,
                mmproj,
                current_port,
                image_max_tokens,
                chat_template,
            ));
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
                .map_err(|e| format!("failed to launch llama-server: {e}"))?;

            match await_health(&mut child, &stderr_log, current_port) {
                Ok(()) => {
                    #[cfg(windows)]
                    let job_handle = win_job::bind_child(&child).ok().flatten();

                    return Ok(Server {
                        child,
                        stderr_log,
                        port: current_port,
                        #[cfg(windows)]
                        job_handle,
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
pub(crate) fn server_args(
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
        on_token: &mut dyn FnMut(&str) -> bool,
        should_cancel: &dyn Fn() -> bool,
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
            should_cancel,
        )
    }
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
        let child = Command::new("sleep")
            .arg("9999")
            .spawn()
            .map_err(|e| format!("Server::for_test: could not spawn sleep: {e}"))?;
        #[cfg(windows)]
        let child = Command::new("cmd")
            .args(["/C", "timeout", "/t", "9999", "/nobreak"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Server::for_test: could not spawn timeout: {e}"))?;
        Ok(Server {
            child,
            stderr_log: tempfile::NamedTempFile::new()?,
            port,
            #[cfg(windows)]
            job_handle: None,
        })
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
