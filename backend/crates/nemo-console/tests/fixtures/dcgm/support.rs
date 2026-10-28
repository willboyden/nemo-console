//! Test support shared by the workstream B test files (dcgm, aiperf, guardrails, openshell): scriptable fake runners (the
//! Python tests' `FakeRunner`s), an `AdapterContext` builder and fixture readers. Included with
//! `#[path = "fixtures/dcgm/support.rs"] mod support;`. Hermetic: no real CLI, no real port (wiremock or the closed
//! port 127.0.0.1:9), a fixed empty environment.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    unreachable_pub
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use nemo_console::adapters::base::{
    AdapterContext, CommandResult, CommandRunner, RunOpts, RunnerError,
};
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;

/// A URL nothing listens on (connections are refused): never a live lab port.
pub const CLOSED: &str = "http://127.0.0.1:9";

pub fn fixtures(sys: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(sys)
}

pub fn fx(sys: &str, name: &str) -> String {
    std::fs::read_to_string(fixtures(sys).join(name))
        .unwrap_or_else(|e| panic!("fixture {sys}/{name}: {e}"))
}

pub fn ok(stdout: &str) -> CommandResult {
    CommandResult::new(0, stdout, "")
}

pub fn rc(code: i32, stdout: &str, stderr: &str) -> CommandResult {
    CommandResult::new(code, stdout, stderr)
}

pub fn bad(stderr: &str) -> CommandResult {
    CommandResult::new(1, "", stderr)
}

pub fn timed_out() -> CommandResult {
    let mut r = CommandResult::new(-9, "", "");
    r.timed_out = true;
    r
}

pub type Respond = Arc<
    dyn Fn(Vec<String>) -> BoxFuture<'static, Result<CommandResult, RunnerError>> + Send + Sync,
>;

/// A responder from a fixed result.
pub fn fixed(r: CommandResult) -> Respond {
    Arc::new(move |_| {
        let r = r.clone();
        Box::pin(async move { Ok(r) })
    })
}

/// A responder computed from the argv.
pub fn with(f: impl Fn(&[String]) -> CommandResult + Send + Sync + 'static) -> Respond {
    let f = Arc::new(f);
    Arc::new(move |argv| {
        let f = Arc::clone(&f);
        Box::pin(async move { Ok(f(&argv)) })
    })
}

/// A responder that raises (the runner refused the argv, like a binary that is not allowlisted).
pub fn raising(class: &'static str, msg: &str) -> Respond {
    let msg = msg.to_owned();
    Arc::new(move |_| {
        let msg = msg.clone();
        Box::pin(async move { Err(RunnerError::Os(class, msg)) })
    })
}

#[derive(Debug, Clone)]
pub struct Call {
    pub argv: Vec<String>,
    pub timeout: f64,
    pub max_bytes: usize,
    pub cwd: Option<PathBuf>,
    pub stdin: Option<Vec<u8>>,
}

type Pred = Arc<dyn Fn(&[String]) -> bool + Send + Sync>;

enum Rule {
    Pred(Pred, Respond),
    Prefix(Vec<String>, Respond),
}

/// Python's two FakeRunner flavours in one: predicate rules (first match wins, `insert_first` = `rules.insert(0,..)`)
/// and prefix rules (longest prefix wins, re-registering a prefix replaces it). Unmatched argv -> rc 127.
#[derive(Default)]
pub struct FakeRunner {
    rules: Mutex<Vec<Rule>>,
    pub calls: Mutex<Vec<Call>>,
}

impl FakeRunner {
    pub fn new() -> Arc<FakeRunner> {
        Arc::new(FakeRunner::default())
    }

    /// Append a predicate rule.
    pub fn on(&self, pred: impl Fn(&[String]) -> bool + Send + Sync + 'static, r: Respond) {
        self.rules
            .lock()
            .unwrap()
            .push(Rule::Pred(Arc::new(pred), r));
    }

    /// Insert a predicate rule in front of all others.
    pub fn insert_first(
        &self,
        pred: impl Fn(&[String]) -> bool + Send + Sync + 'static,
        r: Respond,
    ) {
        self.rules
            .lock()
            .unwrap()
            .insert(0, Rule::Pred(Arc::new(pred), r));
    }

    /// Register (or replace) a prefix rule.
    pub fn on_prefix(&self, prefix: &[&str], r: Respond) {
        let p: Vec<String> = prefix.iter().map(|s| (*s).to_owned()).collect();
        let mut rules = self.rules.lock().unwrap();
        rules.retain(|rule| !matches!(rule, Rule::Prefix(q, _) if *q == p));
        rules.push(Rule::Prefix(p, r));
    }

    pub fn clear(&self) {
        self.rules.lock().unwrap().clear();
    }

    pub fn argvs(&self) -> Vec<Vec<String>> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.argv.clone())
            .collect()
    }

    /// Calls whose argv starts with `prefix`.
    pub fn called(&self, prefix: &[&str]) -> Vec<Vec<String>> {
        self.argvs()
            .into_iter()
            .filter(|a| starts(a, prefix))
            .collect()
    }

    pub fn count(&self, prefix: &[&str]) -> usize {
        self.called(prefix).len()
    }

    pub fn call_log(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn pick(&self, argv: &[String]) -> Option<Respond> {
        let rules = self.rules.lock().unwrap();
        let mut best: Option<(usize, Respond)> = None;
        for rule in rules.iter() {
            match rule {
                Rule::Pred(p, r) => {
                    if p(argv) {
                        return Some(Arc::clone(r));
                    }
                }
                Rule::Prefix(p, r) => {
                    if argv.len() >= p.len()
                        && argv[..p.len()] == p[..]
                        && best.as_ref().is_none_or(|(n, _)| p.len() > *n)
                    {
                        best = Some((p.len(), Arc::clone(r)));
                    }
                }
            }
        }
        best.map(|(_, r)| r)
    }
}

pub fn starts(argv: &[String], prefix: &[&str]) -> bool {
    argv.len() >= prefix.len() && argv.iter().zip(prefix).all(|(a, p)| a == p)
}

#[async_trait]
impl CommandRunner for FakeRunner {
    async fn run(&self, argv: &[String], opts: RunOpts) -> Result<CommandResult, RunnerError> {
        self.calls.lock().unwrap().push(Call {
            argv: argv.to_vec(),
            timeout: opts.timeout,
            max_bytes: opts.max_bytes,
            cwd: opts.cwd.clone(),
            stdin: opts.stdin.clone(),
        });
        match self.pick(argv) {
            Some(r) => r(argv.to_vec()).await,
            None => Ok(CommandResult::new(
                127,
                "",
                &format!("no fake for {argv:?}"),
            )),
        }
    }
}

/// `AdapterContext(runner=, http=, urls=, paths=, state_dir=)` with an empty, fixed environment.
pub fn ctx(
    runner: Arc<FakeRunner>,
    urls: &[(&str, &str)],
    paths: &[(&str, &Path)],
    state_dir: &Path,
) -> AdapterContext {
    AdapterContext {
        runner,
        http: Http::new().unwrap(),
        urls: urls
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect::<IndexMap<_, _>>(),
        paths: paths
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.to_path_buf()))
            .collect::<IndexMap<_, _>>(),
        secret_env: IndexMap::new(),
        state_dir: state_dir.to_path_buf(),
        environ: Environ::fixed(std::collections::HashMap::new()),
    }
}

pub fn mode(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

/// Copy a fixture tree (regular files and dirs) into `dst`.
pub fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), &to).unwrap();
        }
    }
}
