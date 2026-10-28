//! The ONE process-control choke point (ADR 3): argv list, binary allowlist, timeout, output cap, redaction, no shell.
//! Port of `services/runner.py`. Contract (PORTING.md section 6.5).
//!
//! Deviation (PORTING.md "Deviations" D-3): Python starts each child with `start_new_session=True` (setsid); Rust
//! cannot call setsid between fork and exec without `unsafe` (forbidden here), so the child gets its own PROCESS
//! GROUP (`process_group(0)`). Group-wide SIGINT/SIGKILL work the same; only the controlling-terminal detach differs,
//! and the console never has a terminal under the launcher.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::adapters::base::{CommandResult, CommandRunner, RunOpts, RunnerError};
use crate::services::common::redact;
use crate::services::environ::Environ;
use crate::services::pyfmt::{char_len, str_repr};

pub static ALLOWED_BINARIES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "docker",
        "systemctl",
        "nemo-relay",
        "skillevaluator",
        "skillevaluator-local",
        "data-designer-local",
        "data-designer-export",
        "openshell",
        "aiperf-local",
        "guardrails-local",
        "evaluator-local",
    ]
    .into_iter()
    .collect()
});
pub const CHILD_ENV_KEYS: [&str; 9] = [
    "PATH",
    "HOME",
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
    "DOCKER_HOST",
    "DOCKER_CONTEXT",
    "DOCKER_CONFIG",
    "LANG",
    "LC_ALL",
];
pub const SKILLEVAL_BINARIES: [&str; 2] = ["skillevaluator", "skillevaluator-local"];
pub const SKILLEVAL_ENV_PREFIXES: [&str; 2] = ["SKILL_EVAL_", "SKILLEVAL_"];
pub const SKILLEVAL_ENV_KEYS: [&str; 2] = ["LOCAL_AI_LAB", "LITELLM_URL"];
pub const MAX_TIMEOUT: f64 = 600.0;
pub const MAX_LIVE_EVAL_TIMEOUT: f64 = 4800.0;
pub const DD_BINARIES: [&str; 1] = ["data-designer-local"];
pub const DD_CREATE: f64 = 3600.0;
pub const AIPERF_BINARIES: [&str; 1] = ["aiperf-local"];
pub const AIPERF_PROFILE: f64 = 3600.0;
pub const EVALUATOR_BINARIES: [&str; 1] = ["evaluator-local"];
pub const EVALUATOR_RUN: f64 = MAX_LIVE_EVAL_TIMEOUT;
pub const STOP_GRACE: f64 = 5.0;
pub const MAX_ARGS: usize = 64;
pub const MAX_ARG_LEN: usize = 8192;
/// Python's `os.defpath`.
pub const DEFPATH: &str = "/bin:/usr/bin";

fn is_graceful(bin: &str) -> bool {
    SKILLEVAL_BINARIES.contains(&bin)
        || DD_BINARIES.contains(&bin)
        || AIPERF_BINARIES.contains(&bin)
        || EVALUATOR_BINARIES.contains(&bin)
}

/// The cap for this argv: the long one only for a SkillEvaluator launcher's `tier3`, `data-designer-local create`,
/// `aiperf-local profile` and `evaluator-local run`; everything else keeps MAX_TIMEOUT.
#[must_use]
pub fn max_timeout(argv: &[String]) -> f64 {
    let first = argv.first().map(String::as_str).unwrap_or("");
    let second = argv.get(1).map(String::as_str);
    if SKILLEVAL_BINARIES.contains(&first) && second == Some("tier3") {
        return MAX_LIVE_EVAL_TIMEOUT;
    }
    if DD_BINARIES.contains(&first) && second == Some("create") {
        return DD_CREATE;
    }
    if AIPERF_BINARIES.contains(&first) && second == Some("profile") {
        return AIPERF_PROFILE;
    }
    if EVALUATOR_BINARIES.contains(&first) && second == Some("run") {
        return EVALUATOR_RUN;
    }
    MAX_TIMEOUT
}

/// `shutil.which(name, path=...)`.
#[must_use]
pub fn which(name: &str, path: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    for dir in path.split(':') {
        let dir = if dir.is_empty() { "." } else { dir };
        let cand = Path::new(dir).join(name);
        if let Ok(md) = std::fs::metadata(&cand)
            && md.is_file()
            && md.permissions().mode() & 0o111 != 0
        {
            return Some(cand);
        }
    }
    None
}

/// `str(Path(d).expanduser())`.
#[must_use]
pub fn expanduser(d: &str) -> String {
    if d == "~" {
        return crate::config::home_dir().display().to_string();
    }
    if let Some(rest) = d.strip_prefix("~/") {
        return crate::config::home_dir().join(rest).display().to_string();
    }
    d.to_owned()
}

pub struct SubprocessRunner {
    extra: Vec<String>,
    environ: Environ,
    /// Grace between SIGINT and SIGKILL for the graceful binaries (tests shorten it).
    pub stop_grace: f64,
}

impl SubprocessRunner {
    #[must_use]
    pub fn new(extra_bin_dirs: &[String]) -> Self {
        SubprocessRunner::with_environ(extra_bin_dirs, Environ::process())
    }

    /// The runner with a pinned environment (tests: PATH pointing at fake binaries, no credentials).
    #[must_use]
    pub fn with_environ(extra_bin_dirs: &[String], environ: Environ) -> Self {
        SubprocessRunner {
            extra: extra_bin_dirs.iter().map(|d| expanduser(d)).collect(),
            environ,
            stop_grace: STOP_GRACE,
        }
    }

    fn search_path(&self) -> String {
        let base = self
            .environ
            .get("PATH")
            .unwrap_or_else(|| DEFPATH.to_owned());
        let mut parts = vec![base];
        parts.extend(self.extra.iter().cloned());
        parts.join(":")
    }

    /// Bare allowlisted names only: also rules out '/bin/sh', '../x'.
    pub fn resolve(&self, name: &str) -> Result<PathBuf, RunnerError> {
        if !ALLOWED_BINARIES.contains(name) {
            return Err(RunnerError::Refused(format!(
                "binary {} is not allowed",
                str_repr(name)
            )));
        }
        which(name, &self.search_path()).ok_or_else(|| {
            RunnerError::Refused(format!("binary {} not found on PATH", str_repr(name)))
        })
    }

    fn check(argv: &[String]) -> Result<(), RunnerError> {
        if argv.is_empty() || argv.len() > MAX_ARGS {
            return Err(RunnerError::Refused("bad argv length".to_owned()));
        }
        if argv
            .iter()
            .any(|a| a.contains('\0') || char_len(a) > MAX_ARG_LEN)
        {
            return Err(RunnerError::Refused("bad argv element".to_owned()));
        }
        Ok(())
    }

    fn env(&self, binary: &str) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = CHILD_ENV_KEYS
            .iter()
            .filter_map(|k| self.environ.get(k).map(|v| ((*k).to_owned(), v)))
            .collect();
        let path = env
            .iter()
            .find(|(k, _)| k == "PATH")
            .map_or_else(|| DEFPATH.to_owned(), |(_, v)| v.clone());
        let mut parts = vec![path];
        parts.extend(self.extra.iter().cloned());
        env.retain(|(k, _)| k != "PATH");
        env.push(("PATH".to_owned(), parts.join(":")));
        if SKILLEVAL_BINARIES.contains(&binary) {
            for (k, v) in self.environ.vars() {
                if SKILLEVAL_ENV_PREFIXES.iter().any(|p| k.starts_with(p))
                    || SKILLEVAL_ENV_KEYS.contains(&k.as_str())
                {
                    env.retain(|(e, _)| *e != k);
                    env.push((k, v));
                }
            }
        }
        env
    }
}

fn kill_group(pid: Option<u32>, sig: Signal) {
    if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) {
        let _ = killpg(Pid::from_raw(pid), sig);
    }
}

/// Stops the child's whole group when the run future is dropped (the caller gave up: shutdown, action timeout),
/// like Python's `except CancelledError: await _stop(proc, graceful=...)`: SIGINT and a grace period first for the
/// graceful binaries, SIGKILL to the group in every case, and the integratorer is reaped (no zombie keeps the group
/// alive). The stop runs on a plain thread so it completes even while the runtime shuts down.
struct GroupGuard {
    child: Option<tokio::process::Child>,
    pid: Option<u32>,
    graceful: bool,
    grace: f64,
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let (pid, graceful, grace) = (self.pid, self.graceful, self.grace);
        kill_group(
            pid,
            if graceful {
                Signal::SIGINT
            } else {
                Signal::SIGKILL
            },
        );
        let stop = move || {
            if graceful {
                let deadline = Instant::now() + Duration::from_secs_f64(grace);
                while Instant::now() < deadline {
                    if matches!(child.try_wait(), Ok(Some(_))) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            kill_group(pid, Signal::SIGKILL); // also reaps grandchildren that outlived the integratorer
            let _ = child.start_kill();
            for _ in 0..250 {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        if std::thread::Builder::new()
            .name("runner-stop".to_owned())
            .spawn(stop)
            .is_err()
        {
            kill_group(pid, Signal::SIGKILL);
        }
    }
}

/// `proc.wait()` on the child held by the guard.
async fn wait_child(guard: &mut GroupGuard) {
    if let Some(c) = guard.child.as_mut() {
        let _ = c.wait().await;
    }
}

#[async_trait]
impl CommandRunner for SubprocessRunner {
    async fn run(&self, argv: &[String], opts: RunOpts) -> Result<CommandResult, RunnerError> {
        Self::check(argv)?;
        let exe = self.resolve(&argv[0])?;
        let timeout = opts.timeout.min(max_timeout(argv)).max(0.1);
        let graceful = is_graceful(&argv[0]);
        let start = Instant::now();
        let mut cmd = Command::new(&exe);
        cmd.args(&argv[1..])
            .env_clear()
            .envs(self.env(&argv[0]))
            .stdin(if opts.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(false);
        if let Some(cwd) = &opts.cwd {
            cmd.current_dir(cwd);
        }
        let mut child = cmd.spawn().map_err(|e| {
            let class = match e.kind() {
                std::io::ErrorKind::NotFound => "FileNotFoundError",
                std::io::ErrorKind::PermissionDenied => "PermissionError",
                _ => "OSError",
            };
            RunnerError::Os(class, e.to_string())
        })?;
        let pid = child.id();
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let stdin = child.stdin.take();
        let mut guard = GroupGuard {
            child: Some(child),
            pid,
            graceful,
            grace: self.stop_grace,
        };
        let max_bytes = opts.max_bytes;
        let stdin_data = opts.stdin.clone();

        let drain = |stream: Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>| async move {
            let mut buf: Vec<u8> = Vec::new();
            let mut over = false;
            if let Some(mut s) = stream {
                let mut chunk = vec![0u8; 65536];
                loop {
                    match s.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if buf.len() < max_bytes {
                                let take = n.min(max_bytes - buf.len());
                                buf.extend_from_slice(&chunk[..take]);
                            }
                            if buf.len() >= max_bytes && !over {
                                over = true;
                                kill_group(pid, Signal::SIGKILL); // runaway output: stop the producer
                            }
                        }
                    }
                }
            }
            buf
        };
        let feed = async move {
            if let (Some(mut w), Some(data)) = (stdin, stdin_data) {
                let _ = w.write_all(&data).await;
                let _ = w.shutdown().await;
            }
        };
        let out_stream: Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>> = stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>);
        let err_stream: Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>> = stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>);

        let work = async {
            let (o, e, ()) = tokio::join!(drain(out_stream), drain(err_stream), feed);
            (o, e)
        };
        let mut timed_out = false;
        let (out, err) = match tokio::time::timeout(Duration::from_secs_f64(timeout), work).await {
            Ok((o, e)) => {
                match tokio::time::timeout(Duration::from_secs(5), wait_child(&mut guard)).await {
                    Ok(_) => (o, e),
                    Err(_) => {
                        timed_out = true;
                        (Vec::new(), Vec::new())
                    }
                }
            }
            Err(_) => {
                timed_out = true;
                (Vec::new(), Vec::new())
            }
        };
        if timed_out {
            if graceful {
                kill_group(pid, Signal::SIGINT);
                let _ = tokio::time::timeout(
                    Duration::from_secs_f64(self.stop_grace),
                    wait_child(&mut guard),
                )
                .await;
            }
            kill_group(pid, Signal::SIGKILL);
            if let Some(c) = guard.child.as_mut() {
                let _ = c.start_kill();
            }
            let _ = tokio::time::timeout(Duration::from_secs(5), wait_child(&mut guard)).await;
        }
        // finished: disarm the guard (it would otherwise signal the group on drop)
        let Some(mut child) = guard.child.take() else {
            return Err(RunnerError::Os("OSError", "child handle lost".to_owned()));
        };
        let returncode = match child.try_wait() {
            Ok(Some(st)) => {
                use std::os::unix::process::ExitStatusExt;
                st.code().unwrap_or_else(|| st.signal().map_or(-1, |s| -s))
            }
            _ => -1,
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let duration_ms = start.elapsed().as_millis() as u64;
        Ok(CommandResult {
            returncode,
            stdout: redact(&String::from_utf8_lossy(&out)),
            stderr: redact(&String::from_utf8_lossy(&err)),
            timed_out,
            duration_ms,
        })
    }
}
