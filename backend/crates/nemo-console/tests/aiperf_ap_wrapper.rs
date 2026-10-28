//! Port of backend/tests/aiperf/test_ap_wrapper.py: the lab wrapper's flag ALLOWLIST (clients/aiperf/aiperf-local),
//! run against a stub `aiperf` and a throwaway HOME. Like the Python module (`pytest.mark.skipif`), every test returns
//! early when the console is checked out without the lab repo around it (the wrapper lives in the lab, not here).
//! `AIPERF_WRAPPER_SRC=<path>` points the tests at a wrapper elsewhere. Nothing here touches the real router: the
//! wrapper's LITELLM_URL is a local fake, and its environment is exactly the variables below.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value,
    clippy::print_stderr
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Output;

use tokio::process::Command;

use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn wrapper_src() -> Option<PathBuf> {
    let p = std::env::var_os("AIPERF_WRAPPER_SRC")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../aiperf/aiperf-local")
        });
    p.is_file().then_some(p)
}

struct WEnv {
    _tmp: tempfile::TempDir,
    vars: HashMap<&'static str, String>,
    wrapper: PathBuf,
    art: PathBuf,
    stub_out: PathBuf,
    _server: MockServer,
}

async fn env() -> Option<WEnv> {
    let Some(src) = wrapper_src() else {
        eprintln!("skipped: lab repo (clients/aiperf) not present");
        return None;
    };
    let tmp = tempfile::tempdir().unwrap();
    let lab = tmp.path().join("lab").join("clients").join("aiperf");
    std::fs::create_dir_all(lab.join(".venv").join("bin")).unwrap();
    std::fs::copy(&src, lab.join("aiperf-local")).unwrap();
    std::fs::copy(
        src.parent().unwrap().join("auth-skeleton.yaml"),
        lab.join("auth-skeleton.yaml"),
    )
    .unwrap();
    let stub = lab.join(".venv").join("bin").join("aiperf");
    std::fs::write(&stub, "#!/bin/sh\nprintf \"%s\\n\" \"$@\" > \"$STUB_OUT\"\nprintf \"%s\" \"$AIPERF_ROUTER_KEY\" > \"$STUB_OUT.key\"\n").unwrap();
    set_mode(&stub, 0o755);
    // The wrapper verifies the VENDORED tokenizer's sha256 and installs it into the cache before a run (no download)
    std::fs::create_dir_all(lab.join("vendor")).unwrap();
    std::os::unix::fs::symlink(
        src.parent()
            .unwrap()
            .join("vendor")
            .join("o200k_base.tiktoken"),
        lab.join("vendor").join("o200k_base.tiktoken"),
    )
    .unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join(".config").join("aiperf")).unwrap();
    let key = home.join(".config").join("aiperf").join("litellm.key");
    std::fs::write(&key, "sk-FAKE-not-a-real-key-0123456789\n").unwrap();
    set_mode(&key, 0o600);
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;
    let stub_out = tmp.path().join("stub.out");
    let mut vars = HashMap::new();
    vars.insert("HOME", home.display().to_string());
    vars.insert(
        "PATH",
        std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
    );
    vars.insert("LITELLM_URL", server.uri());
    vars.insert("STUB_OUT", stub_out.display().to_string());
    Some(WEnv {
        art: tmp.path().join("art"),
        wrapper: lab.join("aiperf-local"),
        stub_out,
        vars,
        _tmp: tmp,
        _server: server,
    })
}

fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// The wrapper runs asynchronously: it calls the fake router served by this test's runtime.
async fn run(e: &WEnv, args: &[&str]) -> Output {
    let mut cmd = Command::new(&e.wrapper);
    cmd.arg("profile").args(args).env_clear().envs(&e.vars);
    output(&mut cmd).await
}

/// `output()`, retried while the freshly copied script is still open for writing in a sibling test's forked child
/// (ETXTBSY is a race between parallel tests, not a result).
async fn output(cmd: &mut Command) -> Output {
    for _ in 0..100 {
        match cmd.output().await {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            other => return other.unwrap(),
        }
    }
    panic!("the copied wrapper stayed busy")
}

const GOOD: &[&str] = &[
    "--model",
    "aitop/nemotron-lightning",
    "--endpoint-type",
    "chat",
    "--streaming",
    "--concurrency",
    "2",
    "--request-count",
    "8",
    "--prompt-input-tokens-mean",
    "128",
    "--prompt-output-tokens-mean",
    "64",
    "--warmup-request-count",
    "1",
    "--request-timeout-seconds",
    "120",
    "--random-seed",
    "42",
    "--export-level",
    "records",
    "--use-server-token-count",
];

#[tokio::test]
async fn test_every_flag_the_console_renders_is_accepted() {
    let Some(e) = env().await else { return };
    let art = e.art.display().to_string();
    let mut args: Vec<&str> = GOOD.to_vec();
    args.extend(["--artifact-dir", &art]);
    let r = run(&e, &args).await;
    assert!(
        r.status.code() == Some(0),
        "{}",
        String::from_utf8_lossy(&r.stderr)
    );
    let out = std::fs::read_to_string(&e.stub_out).unwrap();
    let got: Vec<&str> = out.split('\n').collect();
    assert!(
        got[0] == "profile"
            && got.contains(&"--config")
            && got.contains(&"--no-gpu-telemetry")
            && got.contains(&"--no-server-metrics")
    );
    let i = got.iter().position(|x| *x == "--model").unwrap();
    assert!(got[i + 1] == "aitop/nemotron-lightning" && got.contains(&"--use-server-token-count"));
    let key = std::fs::read_to_string(format!("{}.key", e.stub_out.display())).unwrap();
    assert_eq!(key, "sk-FAKE-not-a-real-key-0123456789"); // env only
    assert!(!out.contains("sk-FAKE")); // never argv
}

#[tokio::test]
async fn test_equals_forms_are_accepted() {
    let Some(e) = env().await else { return };
    let r = run(
        &e,
        &[
            "--model=m",
            "--concurrency=1",
            &format!("--artifact-dir={}", e.art.display()),
        ],
    )
    .await;
    assert!(
        r.status.code() == Some(0),
        "{}",
        String::from_utf8_lossy(&r.stderr)
    );
}

#[tokio::test]
async fn test_everything_else_is_refused_before_any_run() {
    let flags: [&[&str]; 28] = [
        &["--api-key", "x"],
        &["--api-k", "x"],
        &["--api", "x"],
        &["--conc", "2"],
        &["--concurr=2"],
        &["--model-n", "x"],
        &["--header", "a:b"],
        &["-H", "a:b"],
        &["-Ha:b"],
        &["--config", "/etc/passwd"],
        &["-f", "x"],
        &["--public-dataset", "sharegpt"],
        &["--input-file", "/etc/passwd"],
        &["--otel-url", "http://x"],
        &["--mlflow-tracking-uri=http://x"],
        &["--wandb-project", "p"],
        &["--gpu-telemetry"],
        &["--server-metrics", "http://x"],
        &["--ui", "dashboard"],
        &["--auto-plot"],
        &["--tokenizer-trust-remote-code"],
        &["--api-port", "9"],
        &["--zmq-host", "x"],
        &["--some-future-flag", "1"],
        &["stray-positional"],
        &["--streaming=false"],
        &["-u"],
        &["--model"],
    ];
    for flag in flags {
        let Some(e) = env().await else { return };
        let art = e.art.display().to_string();
        let mut args: Vec<&str> = flag.to_vec();
        args.extend(["--artifact-dir", &art]);
        let r = run(&e, &args).await;
        assert_eq!(
            r.status.code(),
            Some(2),
            "{flag:?}: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        assert!(!e.stub_out.exists() && !e.art.exists());
    }
}

#[tokio::test]
async fn test_non_loopback_urls_are_refused() {
    for url in [
        "http://evil.example:4000",
        "http://127.0.0.1:4000@evil.example",
        "https://127.0.0.1:4000",
        "http://127.0.0.1:4000/v1",
        "http://0.0.0.0:4000",
    ] {
        let Some(e) = env().await else { return };
        let art = e.art.display().to_string();
        let eq = format!("--url={url}");
        for spelled in [vec!["--url", url], vec![eq.as_str()], vec!["-u", url]] {
            let mut args = spelled.clone();
            args.extend(["--artifact-dir", &art]);
            let r = run(&e, &args).await;
            let err = String::from_utf8_lossy(&r.stderr);
            assert!(
                r.status.code() == Some(2) && err.contains("non-loopback") && !e.stub_out.exists(),
                "{spelled:?}: {err}"
            );
        }
    }
}

#[tokio::test]
async fn test_tokenizer_must_be_builtin_or_a_local_dir() {
    let Some(e) = env().await else { return };
    let art = e.art.display().to_string();
    assert_eq!(
        run(
            &e,
            &["--tokenizer", "meta-llama/Llama-3", "--artifact-dir", &art]
        )
        .await
        .status
        .code(),
        Some(2)
    );
    assert_eq!(
        run(&e, &["--tokenizer", "builtin", "--artifact-dir", &art])
            .await
            .status
            .code(),
        Some(0)
    );
}

#[tokio::test]
async fn test_artifact_dir_is_required_and_the_key_file_too() {
    let Some(e) = env().await else { return };
    assert_eq!(run(&e, &["--model", "m"]).await.status.code(), Some(2));
    std::fs::remove_file(
        Path::new(&e.vars["HOME"])
            .join(".config")
            .join("aiperf")
            .join("litellm.key"),
    )
    .unwrap();
    let r = run(
        &e,
        &[
            "--model",
            "m",
            "--artifact-dir",
            &e.art.display().to_string(),
        ],
    )
    .await;
    assert!(
        r.status.code() == Some(3) && String::from_utf8_lossy(&r.stderr).contains("mint-key.sh")
    );
}

#[tokio::test]
async fn test_a_tampered_vendored_tokenizer_is_refused() {
    // The vocabulary is vendored instead of downloaded: a file that fails its pinned sha256 is never used.
    let Some(src) = wrapper_src() else {
        eprintln!("skipped: lab repo (clients/aiperf) not present");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let lab = tmp.path().join("lab");
    std::fs::create_dir_all(lab.join("vendor")).unwrap();
    std::fs::copy(&src, lab.join("aiperf-local")).unwrap();
    std::fs::write(
        lab.join("vendor").join("o200k_base.tiktoken"),
        b"not the vocabulary\n",
    )
    .unwrap();
    let mut cmd = Command::new(lab.join("aiperf-local"));
    cmd.arg("install-tokenizer")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", tmp.path());
    let r = output(&mut cmd).await;
    assert!(
        r.status.code() == Some(3)
            && String::from_utf8_lossy(&r.stderr).contains("does not match its pinned sha256")
    );
    let cache = lab.join(".tiktoken-cache");
    assert!(!cache.exists() || std::fs::read_dir(&cache).unwrap().next().is_none());
}
