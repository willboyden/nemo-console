//! Port of backend/tests/core/test_runner.py: the real CommandRunner against tiny fake executables in a temp dir.
//! Hermetic: PATH is the temp bin dir ONLY (the coreutils the scripts need are symlinked into it), so no real
//! docker/systemctl can ever run.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test helpers outside #[test] fns
mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nemo_console::adapters::base::{CommandRunner, RunOpts};
use nemo_console::config::Settings;
use nemo_console::services::environ::Environ;
use nemo_console::services::runner::{
    MAX_LIVE_EVAL_TIMEOUT, MAX_TIMEOUT, SubprocessRunner, max_timeout,
};
use nix::sys::signal::killpg;
use nix::unistd::Pid;
use serde_json::{Map, json};

/// The tests of this file write fake executables and spawn them; run concurrently, one thread's fork can inherit
/// another thread's still-open script file and `execve` fails with ETXTBSY ("Text file busy"). pytest runs them one
/// at a time; so do these.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Bin {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
}

impl Bin {
    fn new() -> Bin {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("bin");
        std::fs::create_dir(&dir).unwrap();
        for tool in ["sleep", "cat", "yes", "touch", "printf"] {
            if let Some(src) = ["/usr/bin", "/bin"]
                .iter()
                .map(|d| Path::new(d).join(tool))
                .find(|p| p.exists())
            {
                std::os::unix::fs::symlink(src, dir.join(tool)).unwrap();
            }
        }
        Bin { _tmp: tmp, dir }
    }

    fn script(&self, name: &str, body: &str) {
        let p = self.dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}")).unwrap();
        common::set_mode(&p, 0o755);
    }

    fn runner(&self, extra_env: &[(&str, &str)]) -> SubprocessRunner {
        let dir = self.dir.to_string_lossy().into_owned();
        let mut env: Vec<(&str, &str)> = vec![("PATH", dir.as_str())];
        env.extend_from_slice(extra_env);
        SubprocessRunner::with_environ(std::slice::from_ref(&dir), Environ::from_pairs(&env))
    }
}

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

#[tokio::test]
async fn test_runs_allowlisted_binary_with_argv_no_shell() {
    let _serial = SERIAL.lock().await;
    let b = Bin::new();
    b.script("docker", "printf \"%s|\" \"$@\"\n");
    let marker = b.dir.join("pwned");
    let m = format!("; touch {}", marker.display());
    let r = b
        .runner(&[])
        .run(
            &argv(&["docker", "ps", &m, "$(touch x)", "a b"]),
            RunOpts::default(),
        )
        .await
        .unwrap();
    assert_eq!(r.returncode, 0);
    assert_eq!(
        r.stdout,
        format!("ps|; touch {}|$(touch x)|a b|", marker.display())
    );
    assert!(!marker.exists());
}

#[tokio::test]
async fn test_binary_allowlist() {
    let _serial = SERIAL.lock().await;
    let b = Bin::new();
    b.script("sh2", "echo hi\n");
    let run = b.runner(&[]);
    for a in [
        argv(&["sh", "-c", "id"]),
        argv(&["/bin/sh"]),
        argv(&["bash"]),
        argv(&["../docker"]),
        argv(&["sh2"]),
        vec![],
    ] {
        assert!(run.run(&a, RunOpts::default()).await.is_err(), "{a:?}");
    }
    assert!(
        run.run(&argv(&["docker", "a\0b"]), RunOpts::default())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn test_timeout_kills_process() {
    let _serial = SERIAL.lock().await;
    let b = Bin::new();
    b.script("systemctl", "sleep 30\n");
    let r = tokio::time::timeout(
        Duration::from_secs(5),
        b.runner(&[])
            .run(&argv(&["systemctl", "x"]), RunOpts::timeout(0.3)),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(r.timed_out);
    assert_ne!(r.returncode, 0);
}

#[tokio::test]
async fn test_output_cap_stops_runaway_producer() {
    let _serial = SERIAL.lock().await;
    let b = Bin::new();
    b.script("nemo-relay", "yes AAAAAAAAAAAAAAAA\n");
    let r = tokio::time::timeout(
        Duration::from_secs(8),
        b.runner(&[]).run(
            &argv(&["nemo-relay", "doctor"]),
            RunOpts::timeout(10.0).max_bytes(1000),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(r.stdout.len() <= 1000);
}

#[tokio::test]
async fn test_stdin_bytes_and_env_is_minimal() {
    let _serial = SERIAL.lock().await;
    let b = Bin::new();
    b.script("docker", "cat; echo \"KEY=${LITELLM_API_KEY:-unset}\"\n");
    let run = b.runner(&[("LITELLM_API_KEY", "should-not-be-inherited-123")]);
    let r = run
        .run(
            &argv(&["docker", "x"]),
            RunOpts::default().stdin(b"hello\n".to_vec()),
        )
        .await
        .unwrap();
    assert!(
        r.stdout.contains("hello") && r.stdout.contains("KEY=unset"),
        "{}",
        r.stdout
    );
}

#[tokio::test]
async fn test_nonzero_exit_reported() {
    let _serial = SERIAL.lock().await;
    let b = Bin::new();
    b.script("docker", "echo oops >&2; exit 3\n");
    let r = b
        .runner(&[])
        .run(&argv(&["docker", "x"]), RunOpts::default())
        .await
        .unwrap();
    assert_eq!(r.returncode, 3);
    assert!(r.stderr.contains("oops"));
}

/// True once no process is left in the group (Python `_group_gone`, 100 x 20 ms).
async fn group_gone(pgid: i32) -> bool {
    for _ in 0..100 {
        if killpg(Pid::from_raw(pgid), None).is_err() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

async fn wait_for_pid(pidfile: &Path) -> i32 {
    for _ in 0..100 {
        if let Ok(t) = std::fs::read_to_string(pidfile)
            && let Ok(pid) = t.trim().parse()
        {
            return pid;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the script never wrote its pid")
}

#[test]
fn test_long_timeout_cap_is_only_for_skillevaluator_tier3() {
    assert_eq!(
        max_timeout(&argv(&["skillevaluator-local", "tier3", "/s"])),
        MAX_LIVE_EVAL_TIMEOUT
    );
    assert_eq!(MAX_LIVE_EVAL_TIMEOUT, 4800.0);
    assert_eq!(
        max_timeout(&argv(&["skillevaluator", "tier3"])),
        MAX_LIVE_EVAL_TIMEOUT
    );
    for a in [
        argv(&["skillevaluator-local", "tier1", "/s"]),
        argv(&["skillevaluator-local", "doctor"]),
        argv(&["docker", "tier3"]),
        argv(&["systemctl", "tier3"]),
        argv(&["nemo-relay", "tier3"]),
        argv(&["skillevaluator-local"]),
    ] {
        assert_eq!(max_timeout(&a), MAX_TIMEOUT, "{a:?}");
    }
    assert_eq!(MAX_TIMEOUT, 600.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_cancelled_skillevaluator_launcher_gets_sigint_then_its_group_is_gone() {
    let _serial = SERIAL.lock().await;
    let b = Bin::new();
    let (marker, pidfile) = (b.dir.join("got-int"), b.dir.join("pid"));
    // The lab launcher stops its job container on SIGINT; this stand-in records that it saw one.
    b.script(
        "skillevaluator-local",
        &format!(
            "echo $$ > {}\ntrap \"touch {}; exit 130\" INT\nsleep 30 &\nwait\n",
            pidfile.display(),
            marker.display()
        ),
    );
    let mut run = b.runner(&[]);
    run.stop_grace = 2.0;
    let task = tokio::spawn(async move {
        run.run(
            &argv(&["skillevaluator-local", "tier3", "x"]),
            RunOpts::timeout(60.0),
        )
        .await
    });
    let pid = wait_for_pid(&pidfile).await;
    task.abort();
    let res = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap();
    assert!(res.unwrap_err().is_cancelled());
    // interrupted first, not just killed (the stop runs on its own thread: give the trap a moment)
    let mut interrupted = false;
    for _ in 0..100 {
        if marker.exists() {
            interrupted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(interrupted, "the launcher never saw SIGINT");
    assert!(
        group_gone(pid).await,
        "the whole group (incl. the background sleep) is gone"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_cancelled_other_binary_is_killed_without_grace() {
    let _serial = SERIAL.lock().await;
    let b = Bin::new();
    let pidfile = b.dir.join("pid");
    // ignores SIGINT: only SIGKILL stops it
    b.script(
        "docker",
        &format!("echo $$ > {}\ntrap \"\" INT\nsleep 30\n", pidfile.display()),
    );
    let run = b.runner(&[]);
    let task = tokio::spawn(async move {
        run.run(&argv(&["docker", "x"]), RunOpts::timeout(60.0))
            .await
    });
    let pid = wait_for_pid(&pidfile).await;
    let t0 = Instant::now();
    task.abort();
    let res = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap();
    assert!(res.unwrap_err().is_cancelled());
    assert!(t0.elapsed() < Duration::from_secs(3));
    assert!(group_gone(pid).await);
}

#[test]
fn test_addendum_98_container_and_image_settings_refuse_option_like_values() {
    // Audit L8: container names and the diag image reach docker argv; a leading '-' must never get through.
    for kw in [
        json!({"dcgm_exporter_container": "-v"}),
        json!({"guardrails_container": "--rm"}),
        json!({"openshell_containers": ["ok-name", "-x"]}),
        json!({"dcgm_diag_image": "--privileged"}),
    ] {
        let init: Map<String, serde_json::Value> = kw.as_object().unwrap().clone();
        assert!(Settings::from_sources(None, &[], init).is_err(), "{kw}");
    }
    let s = Settings::from_sources(None, &[], Map::new()).unwrap();
    assert!(
        !s.dcgm_exporter_container.is_empty()
            && !s.guardrails_container.is_empty()
            && !s.openshell_containers.is_empty()
            && !s.dcgm_diag_image.is_empty()
    );
}
