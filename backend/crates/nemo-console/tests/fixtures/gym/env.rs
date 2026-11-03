//! The NeMo Gym test environment: a fake `gym-local` (status, verify and smoke, printing the REAL output shapes
//! of clients/nemo-gym/gym-local), the lab's clients/nemo-gym dir and a context with the console's own paths.
//! Everything offline: no Docker, no router, no clock.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    unreachable_pub
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nemo_console::adapters::gym::GymAdapter;

use crate::support::{CLOSED, FakeRunner, fixed, ok, rc};

pub const LAUNCHER: &str = "gym-local";

/// `gym-local status` with everything in place: broker created but stopped, network internal, image built, key set.
pub const STATUS_UP: &str = "SERVICE                  STATUS\n\
nemo-gym-router-broker   Exited (0) 2 hours ago\n\
RESULT: PASS — network ai-lab-nemo-gym is internal\n\
image: lab/nemo-gym:0.7.0 sha256:5f0c1d2e3a4b5c6d7e8f\n\
key: /home/u/.config/nemo-gym/litellm.key [set, 25 chars]\n";

/// The same with the broker running.
pub const STATUS_BROKER_UP: &str = "SERVICE                  STATUS\n\
nemo-gym-router-broker   Up 3 minutes (healthy)\n\
RESULT: PASS — network ai-lab-nemo-gym is internal\n\
image: lab/nemo-gym:0.7.0 sha256:5f0c1d2e3a4b5c6d7e8f\n\
key: /home/u/.config/nemo-gym/litellm.key [set, 25 chars]\n";

/// A fresh install: nothing created yet (the network appears on the first `up`), no image, no key.
pub const STATUS_FRESH: &str = "SERVICE   STATUS\n\
RESULT: FAIL — network ai-lab-nemo-gym Internal=missing (expected true): the client would have a route out.\n\
image: lab/nemo-gym:0.7.0 not built\n\
key: missing (/home/u/Projects/local-ai-lab/clients/nemo-gym/gym-local key)\n";

/// The network exists and is NOT internal: the containment is broken.
pub const STATUS_OPEN: &str = "SERVICE                  STATUS\n\
nemo-gym-router-broker   Exited (0) 2 hours ago\n\
RESULT: FAIL — network ai-lab-nemo-gym Internal=false (expected true): the client would have a route out.\n\
image: lab/nemo-gym:0.7.0 sha256:5f0c1d2e3a4b5c6d7e8f\n\
key: /home/u/.config/nemo-gym/litellm.key [set, 25 chars]\n";

/// What `gym-local smoke` prints for a good run (numbers only).
pub const SMOKE_PASS: &str = "smoke: mcqa x simple_agent x vllm_model, model glm-5.3-flash-fp4-local, 5 tasks, 2 in parallel (servers + Ray start first, ~1-3 min)\n\
servers up: 4\n\
rollouts: 5   with a reward: 5   rewards: [1.0, 1.0, 1.0, 0.0, 1.0]   mean: 0.8\n\
aggregate: {'mean/reward': 0.8, 'pass@1': 80.0}\n\
run dir (in the nemo-gym-work volume): /work/smoke/20261102T230501Z\n\
RESULT: PASS smoke\n";

/// What it prints when the eval step fails.
pub const SMOKE_FAIL: &str = "smoke: mcqa x simple_agent x vllm_model, model glm-5.3-flash-fp4-local, 5 tasks, 2 in parallel\n\
servers up: 4\n\
rollouts: 0   with a reward: 0   rewards: []   mean: None\n\
RESULT: FAIL gym eval run exited 1 ; last log lines:\n\
openai.APIConnectionError: Connection error.\n";

pub fn runner_with(status: &str) -> Arc<FakeRunner> {
    let r = FakeRunner::new();
    r.on_prefix(&[LAUNCHER, "status"], fixed(ok(status)));
    r.on_prefix(
        &[LAUNCHER, "verify"],
        fixed(ok("RESULT: PASS — network ai-lab-nemo-gym is internal\n")),
    );
    r.on_prefix(&[LAUNCHER, "smoke"], fixed(ok(SMOKE_PASS)));
    r
}

pub fn failing_smoke_runner() -> Arc<FakeRunner> {
    let r = runner_with(STATUS_UP);
    r.on_prefix(&[LAUNCHER, "smoke"], fixed(rc(1, SMOKE_FAIL, "")));
    r
}

/// No wrapper at all: every call is unmatched (rc 127, like a binary that is not on PATH).
pub fn runner_without_the_launcher() -> Arc<FakeRunner> {
    FakeRunner::new()
}

pub struct GymEnv {
    pub tmp: tempfile::TempDir,
}

impl GymEnv {
    pub fn new() -> GymEnv {
        GymEnv {
            tmp: tempfile::tempdir().unwrap(),
        }
    }

    pub fn tmp(&self) -> &Path {
        self.tmp.path()
    }

    /// The lab's clients/nemo-gym directory (the console's "gym" layout key), with the wrapper file in it.
    pub fn gym_dir(&self) -> PathBuf {
        self.tmp().join("lab").join("clients").join("nemo-gym")
    }

    /// The console's state dir; the run dirs live under it.
    pub fn state_dir(&self) -> PathBuf {
        self.tmp().join("state")
    }

    pub fn runs_dir(&self) -> PathBuf {
        self.state_dir().join("gym").join("runs")
    }

    /// `make_ctx` + `GymAdapter(ctx)`; `lab_files` decides whether clients/nemo-gym/gym-local exists.
    pub fn make(&self, runner: Arc<FakeRunner>, lab_files: bool) -> Arc<GymAdapter> {
        let dir = self.gym_dir();
        if lab_files {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(LAUNCHER), "#!/bin/sh\n").unwrap();
        }
        let repo = self.tmp().join("repo");
        let paths: Vec<(&str, &Path)> = vec![("gym_dir", &dir), ("repo", &repo)];
        let ctx = crate::support::ctx(runner, &[("litellm", CLOSED)], &paths, &self.state_dir());
        let a = GymAdapter::new(ctx);
        a.set_follow_interval(0.01);
        a.set_status_ttl(0.0);
        Arc::new(a)
    }

    /// Write a stored run.json (the console's own file) for `id`.
    pub fn seed_run(&self, id: &str, body: &str) {
        let dir = self.runs_dir().join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("run.json"), body).unwrap();
    }
}
