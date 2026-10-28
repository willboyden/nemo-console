//! The AIPerf test environment (Python tests/aiperf/ap_helpers.py + conftest.py): a fake runner that writes the
//! REAL aiperf summary JSON (captured live 2026-10-24), a context with the lab's real profiles.yaml, and a fake router
//! (wiremock, ephemeral port) answering GET /health/liveliness. Everything offline.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    unreachable_pub
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nemo_console::adapters::aiperf::AiPerfAdapter;
use nemo_console::adapters::base::CommandResult;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{CLOSED, FakeRunner, Respond, fixed, fixtures, fx, ok, rc, with};

pub const LAUNCHER: &str = "aiperf-local";

pub fn artifact_dir(argv: &[String]) -> PathBuf {
    let i = argv.iter().position(|a| a == "--artifact-dir").unwrap();
    PathBuf::from(&argv[i + 1])
}

/// `aiperf-local profile ... --artifact-dir DIR`: the captured summary JSON (or an override) plus the captured
/// console tail on stdout.
pub fn write_profile(argv: &[String], code: i32, summary: Option<&str>) -> CommandResult {
    let out = artifact_dir(argv);
    std::fs::create_dir_all(&out).unwrap();
    match summary {
        None => {
            std::fs::copy(
                fixtures("aiperf").join("profile_export_aiperf.json"),
                out.join("profile_export_aiperf.json"),
            )
            .unwrap();
        }
        Some(s) => std::fs::write(out.join("profile_export_aiperf.json"), s).unwrap(),
    }
    rc(code, &fx("aiperf", "smoke-console.txt"), "")
}

pub fn writes_profile(summary: Option<String>) -> Respond {
    with(move |argv| write_profile(argv, 0, summary.as_deref()))
}

/// A run whose every request failed: only the per-request records and the error panel on stderr; exit 1.
pub fn writes_records() -> Respond {
    with(|argv| {
        let out = artifact_dir(argv);
        std::fs::create_dir_all(&out).unwrap();
        std::fs::copy(
            fixtures("aiperf").join("failed-records.jsonl"),
            out.join("profile_export.jsonl"),
        )
        .unwrap();
        rc(1, "", &fx("aiperf", "all-failed.txt"))
    })
}

pub fn standard_runner() -> Arc<FakeRunner> {
    let r = FakeRunner::new();
    r.on_prefix(
        &[LAUNCHER, "--version"],
        fixed(ok(&fx("aiperf", "version.txt"))),
    );
    r.on_prefix(&[LAUNCHER, "profile"], writes_profile(None));
    r
}

/// A responder that waits for a permit before answering like `writes_profile()` (the gated runs).
pub fn gated(gate: Arc<tokio::sync::Semaphore>) -> Respond {
    Arc::new(move |argv| {
        let g = Arc::clone(&gate);
        Box::pin(async move {
            let _p = g.acquire().await;
            Ok(write_profile(&argv, 0, None))
        })
    })
}

#[derive(Clone)]
pub struct Opts {
    pub profiles: Option<String>,
    pub key_file: bool,
    pub tokenizer: bool,
    pub repo: Option<PathBuf>,
    pub router_down: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            profiles: None,
            key_file: true,
            tokenizer: true,
            repo: None,
            router_down: false,
        }
    }
}

pub struct ApEnv {
    pub tmp: tempfile::TempDir,
    pub router: MockServer,
}

impl ApEnv {
    pub async fn new() -> ApEnv {
        let router = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health/liveliness"))
            .respond_with(ResponseTemplate::new(200).set_body_json("I'm alive!"))
            .mount(&router)
            .await;
        ApEnv {
            tmp: tempfile::tempdir().unwrap(),
            router,
        }
    }

    pub fn tmp(&self) -> &Path {
        self.tmp.path()
    }

    pub fn aiperf_dir(&self) -> PathBuf {
        self.tmp().join("lab").join("clients").join("aiperf")
    }

    pub fn key_path(&self) -> PathBuf {
        self.tmp()
            .join("home")
            .join(".config")
            .join("aiperf")
            .join("litellm.key")
    }

    /// `make_ctx` + `AiPerfAdapter(ctx)` with follow_interval 0.01.
    pub fn make(
        &self,
        runner: Option<Arc<FakeRunner>>,
        o: &Opts,
    ) -> (Arc<AiPerfAdapter>, Arc<FakeRunner>) {
        let run = runner.unwrap_or_else(standard_runner);
        let dir = self.aiperf_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let prof = dir.join("profiles.yaml");
        std::fs::write(
            &prof,
            o.profiles
                .clone()
                .unwrap_or_else(|| fx("aiperf", "profiles.yaml")),
        )
        .unwrap();
        let key = self.key_path();
        if o.key_file {
            std::fs::create_dir_all(key.parent().unwrap()).unwrap();
            std::fs::write(&key, "sk-FAKE-not-a-real-key-0123456789\n").unwrap(); // EXISTENCE only; never read
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        if o.tokenizer {
            let cache = dir.join(".tiktoken-cache");
            std::fs::create_dir_all(&cache).unwrap();
            std::fs::write(cache.join("fb374d419588a4632f3f557e76b4b70aebbca790"), b"x").unwrap();
        }
        let mut paths: Vec<(&str, &Path)> = vec![
            ("aiperf_dir", &dir),
            ("aiperf_profiles", &prof),
            ("aiperf_key_file", &key),
        ];
        if let Some(r) = &o.repo {
            paths.push(("repo", r));
        }
        let url = if o.router_down {
            CLOSED.to_owned()
        } else {
            self.router.uri()
        };
        let ctx = crate::support::ctx(
            Arc::clone(&run),
            &[("litellm", &url)],
            &paths,
            &self.tmp().join("state"),
        );
        let a = AiPerfAdapter::new(ctx);
        a.set_follow_interval(0.01);
        (Arc::new(a), run)
    }
}
