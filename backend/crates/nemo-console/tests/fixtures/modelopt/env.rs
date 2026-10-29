//! The Model Optimizer test environment: a fake `modelopt-local` (version probe + a job starter that writes
//! the REAL run.json shape), the lab's recipes.yaml and a context with the console's own paths. Everything offline.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    unreachable_pub
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nemo_console::adapters::base::CommandResult;
use nemo_console::adapters::modelopt::ModelOptAdapter;
use serde_json::{Map, Value, json};

use crate::support::{CLOSED, FakeRunner, fixed, fx, ok, with};

pub const LAUNCHER: &str = "modelopt-local";

/// The run dir the wrapper was asked to write into.
pub fn run_dir(argv: &[String]) -> PathBuf {
    let i = argv.iter().position(|a| a == "--run-dir").unwrap();
    PathBuf::from(&argv[i + 1])
}

/// The job id the console handed to the wrapper.
pub fn job_id(argv: &[String]) -> String {
    let i = argv.iter().position(|a| a == "--job-id").unwrap();
    argv[i + 1].clone()
}

/// The recipe the console asked for.
pub fn recipe(argv: &[String]) -> String {
    let i = argv.iter().position(|a| a == "--recipe").unwrap();
    argv[i + 1].clone()
}

/// The run.json the wrapper writes when a job starts (verdict "running").
pub fn running_meta(job_id: &str, recipe: &str) -> String {
    let mut m: Map<String, Value> = Map::new();
    m.insert("job_id".to_owned(), json!(job_id.to_owned()));
    m.insert("recipe".to_owned(), json!(recipe.to_owned()));
    m.insert("method".to_owned(), json!("reap".to_owned()));
    m.insert("base_model".to_owned(), json!("GLM-5.3-Flash".to_owned()));
    m.insert(
        "output".to_owned(),
        json!("GLM-5.3-Flash-REAP25-NVFP4".to_owned()),
    );
    m.insert("verdict".to_owned(), json!("running".to_owned()));
    m.insert("ts".to_owned(), json!(1767225600.0));
    m.insert("duration_ms".to_owned(), json!(0));
    m.insert("error".to_owned(), json!("".to_owned()));
    json!(&m).to_string()
}

/// The run.json of a finished job: the numbers the console turns into metrics.
pub fn finished_meta(job_id: &str, recipe: &str) -> String {
    let mut m: Map<String, Value> = Map::new();
    m.insert("job_id".to_owned(), json!(job_id.to_owned()));
    m.insert("recipe".to_owned(), json!(recipe.to_owned()));
    m.insert("method".to_owned(), json!("reap".to_owned()));
    m.insert("base_model".to_owned(), json!("GLM-5.3-Flash".to_owned()));
    m.insert(
        "output".to_owned(),
        json!("GLM-5.3-Flash-REAP25-NVFP4".to_owned()),
    );
    m.insert("verdict".to_owned(), json!("succeeded".to_owned()));
    m.insert("ts".to_owned(), json!(1767225600.0));
    m.insert("duration_ms".to_owned(), json!(2_820_000));
    m.insert("error".to_owned(), json!("".to_owned()));
    let mut metrics: Map<String, Value> = Map::new();
    metrics.insert("output_bytes".to_owned(), json!(76_682_545_454i64));
    metrics.insert("base_bytes".to_owned(), json!(245_850_611_000i64));
    metrics.insert("ratio".to_owned(), json!(0.312));
    m.insert("metrics".to_owned(), Value::Object(metrics));
    json!(&m).to_string()
}

/// A fake wrapper that answers the version probe and starts jobs (writing run.json).
fn start_job(argv: &[String]) -> CommandResult {
    let jid: &str = &job_id(argv);
    let rid: &str = &recipe(argv);
    let dir = run_dir(argv);
    std::fs::create_dir_all(&dir).unwrap();
    let meta: &str = &running_meta(jid, rid);
    std::fs::write(dir.join("run.json"), meta).unwrap();
    let said: &str = &format!("started {jid}\n");
    ok(said)
}

pub fn standard_runner() -> Arc<FakeRunner> {
    let r = FakeRunner::new();
    r.on_prefix(&[LAUNCHER, "--version"], fixed(ok("0.47.0\n")));
    r.on_prefix(&[LAUNCHER, "run"], with(start_job));
    r
}

/// The same wrapper without the version probe: the console then finds no Model Optimizer.
pub fn runner_without_the_launcher() -> Arc<FakeRunner> {
    let r = FakeRunner::new();
    r.on_prefix(&[LAUNCHER, "run"], with(start_job));
    r
}

#[derive(Clone)]
pub struct Opts {
    pub recipes: Option<String>,
    pub launcher: bool,
    pub jobs: usize,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            recipes: None,
            launcher: true,
            jobs: 0,
        }
    }
}

pub struct MoEnv {
    pub tmp: tempfile::TempDir,
}

impl MoEnv {
    pub async fn new() -> MoEnv {
        MoEnv {
            tmp: tempfile::tempdir().unwrap(),
        }
    }

    pub fn tmp(&self) -> &Path {
        self.tmp.path()
    }

    /// The lab's clients/modelopt directory (the console's "modelopt" layout key).
    pub fn modelopt_dir(&self) -> PathBuf {
        self.tmp().join("lab").join("clients").join("modelopt")
    }

    pub fn recipes_path(&self) -> PathBuf {
        self.modelopt_dir().join("recipes.yaml")
    }

    /// The console's state dir; the job dirs live under it.
    pub fn state_dir(&self) -> PathBuf {
        self.tmp().join("state")
    }

    /// `make_ctx` + `ModelOptAdapter(ctx)`.
    pub fn make(
        &self,
        runner: Option<Arc<FakeRunner>>,
        o: &Opts,
    ) -> (Arc<ModelOptAdapter>, Arc<FakeRunner>) {
        let run = match runner {
            Some(r) => r,
            None => {
                if o.launcher {
                    standard_runner()
                } else {
                    runner_without_the_launcher()
                }
            }
        };
        let dir = self.modelopt_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let prof = self.recipes_path();
        std::fs::write(
            &prof,
            o.recipes
                .clone()
                .unwrap_or_else(|| fx("modelopt", "recipes.yaml")),
        )
        .unwrap();
        let repo = self.tmp().join("repo");
        let paths: Vec<(&str, &Path)> = vec![
            ("modelopt_dir", &dir),
            ("modelopt_recipes", &prof),
            ("repo", &repo),
        ];
        let ctx = crate::support::ctx(
            Arc::clone(&run),
            &[("litellm", CLOSED)],
            &paths,
            &self.state_dir(),
        );
        let a = ModelOptAdapter::new(ctx);
        a.set_follow_interval(0.01);
        (Arc::new(a), run)
    }

    /// Write `n` finished jobs into the console's job dir, newest last.
    pub fn seed_jobs(&self, n: usize) {
        let base = self.state_dir().join("modelopt").join("jobs");
        for i in 0..n {
            let id: &str = &format!("20261028T09000{i}-123-reap-glm53");
            let dir = base.join(id);
            std::fs::create_dir_all(&dir).unwrap();
            let meta: &str = &finished_meta(id, "reap-glm53");
            std::fs::write(dir.join("run.json"), meta).unwrap();
        }
    }
}
