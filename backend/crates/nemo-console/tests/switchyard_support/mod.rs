//! workstream A test support (Switchyard, Relay, Dynamo): the Rust stand-ins for the Python tests' `respx` mocks and scriptable
//! fake runners. Hermetic by construction: the fake HTTP server listens on an ephemeral loopback port, the context
//! carries a fixed empty environment, and nothing reaches a lab port.
#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc,
    clippy::type_complexity
)]

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use indexmap::IndexMap;
use nemo_console::adapters::base::{
    AdapterContext, CommandResult, CommandRunner, EventStream, RunOpts, RunnerError,
};
use nemo_console::domain::models::SystemEvent;
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The fixtures copied from backend/tests/<system>/fixtures.
pub fn fixture(system: &str, name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(system)
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

pub fn fixture_dir(system: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(system)
}

// ── fake HTTP upstream ──────────────────────────────────────────────────────────────────────────

/// What the fake server does for one request.
#[derive(Clone, Debug)]
pub enum Reply {
    /// Status, body, extra headers.
    Body(u16, String, Vec<(String, String)>),
    /// Accept the request, then close the connection without answering (a transport error for the client).
    Close,
    /// Answer after a delay (lets a client timeout fire).
    Delay(Duration, Box<Reply>),
}

impl Reply {
    pub fn status(code: u16) -> Reply {
        Reply::Body(code, String::new(), Vec::new())
    }

    pub fn text(code: u16, body: &str) -> Reply {
        Reply::Body(code, body.to_owned(), Vec::new())
    }

    pub fn json(code: u16, body: &serde_json::Value) -> Reply {
        Reply::Body(
            code,
            body.to_string(),
            vec![("content-type".into(), "application/json".into())],
        )
    }
}

/// One request the fake server saw.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Default)]
struct HttpState {
    defaults: HashMap<(String, String), Reply>,
    queues: HashMap<(String, String), VecDeque<Reply>>,
    seen: Vec<Seen>,
}

/// A tiny HTTP/1.1 server with per-(method, path) replies, reply sequences (respx `side_effect=[...]`) and a log.
#[derive(Clone)]
pub struct FakeHttp {
    pub addr: SocketAddr,
    st: Arc<Mutex<HttpState>>,
}

impl FakeHttp {
    pub async fn start() -> FakeHttp {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let st: Arc<Mutex<HttpState>> = Arc::default();
        let st2 = Arc::clone(&st);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let st = Arc::clone(&st2);
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let head_end = loop {
                        let Ok(n) = sock.read(&mut chunk).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                    let mut lines = head.split("\r\n");
                    let first = lines.next().unwrap_or("");
                    let mut parts = first.split(' ');
                    let method = parts.next().unwrap_or("").to_owned();
                    let target = parts.next().unwrap_or("").to_owned();
                    let path = target.split('?').next().unwrap_or("").to_owned();
                    let headers: Vec<(String, String)> = lines
                        .filter_map(|l| l.split_once(':'))
                        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                        .collect();
                    let len: usize = headers
                        .iter()
                        .find(|(k, _)| k == "content-length")
                        .and_then(|(_, v)| v.parse().ok())
                        .unwrap_or(0);
                    let mut body = buf[head_end + 4..].to_vec();
                    while body.len() < len {
                        let Ok(n) = sock.read(&mut chunk).await else {
                            return;
                        };
                        if n == 0 {
                            break;
                        }
                        body.extend_from_slice(&chunk[..n]);
                    }
                    let reply = {
                        let mut s = st.lock().unwrap();
                        s.seen.push(Seen {
                            method: method.clone(),
                            path: path.clone(),
                            headers,
                            body,
                        });
                        let key = (method.clone(), path.clone());
                        let queued = s.queues.get_mut(&key).and_then(VecDeque::pop_front);
                        queued
                            .or_else(|| s.defaults.get(&key).cloned())
                            .unwrap_or(Reply::status(404))
                    };
                    let mut reply = reply;
                    loop {
                        match reply {
                            Reply::Delay(d, inner) => {
                                tokio::time::sleep(d).await;
                                reply = *inner;
                            }
                            Reply::Close => {
                                drop(sock);
                                return;
                            }
                            Reply::Body(code, body, extra) => {
                                let mut out = format!(
                                    "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                                    body.len()
                                );
                                for (k, v) in extra {
                                    out.push_str(&format!("{k}: {v}\r\n"));
                                }
                                out.push_str("\r\n");
                                out.push_str(&body);
                                let _ = sock.write_all(out.as_bytes()).await;
                                let _ = sock.shutdown().await;
                                return;
                            }
                        }
                    }
                });
            }
        });
        FakeHttp { addr, st }
    }

    pub fn uri(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The default reply of (method, path) (respx `mock.get(url).respond(...)`, later calls replace it).
    pub fn on(&self, method: &str, path: &str, reply: Reply) {
        let mut s = self.st.lock().unwrap();
        let key = (method.to_owned(), path.to_owned());
        s.queues.remove(&key);
        s.defaults.insert(key, reply);
    }

    pub fn get(&self, path: &str, reply: Reply) {
        self.on("GET", path, reply);
    }

    /// Replies consumed in order before the default (respx `side_effect=[...]`).
    pub fn seq(&self, method: &str, path: &str, replies: Vec<Reply>) {
        let mut s = self.st.lock().unwrap();
        s.queues
            .insert((method.to_owned(), path.to_owned()), replies.into());
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.st.lock().unwrap().seen.clone()
    }

    pub fn count(&self, method: &str, path: &str) -> usize {
        self.seen()
            .iter()
            .filter(|s| s.method == method && s.path == path)
            .count()
    }

    pub fn clear_seen(&self) {
        self.st.lock().unwrap().seen.clear();
    }
}

// ── fake runners ────────────────────────────────────────────────────────────────────────────────

/// One recorded runner call.
#[derive(Clone, Debug)]
pub struct Call {
    pub argv: Vec<String>,
    pub stdin: Option<Vec<u8>>,
    pub cwd: Option<PathBuf>,
    pub timeout: f64,
    pub max_bytes: usize,
}

pub type Pred = Arc<dyn Fn(&[String]) -> bool + Send + Sync>;
pub type Resp =
    Arc<dyn Fn(&[String], Option<&[u8]>) -> Result<CommandResult, RunnerError> + Send + Sync>;

/// Matches argv by predicate, FIRST match wins (Python switchyard/dynamo `FakeRunner`); unmatched -> rc 127.
/// `rewrite` replaces text in `docker inspect` stdout (dynamo: the captured compose dir -> the test's copy).
#[derive(Default)]
pub struct FakeRunner {
    pub calls: Mutex<Vec<Call>>,
    pub rules: Mutex<Vec<(Pred, Resp)>>,
    pub rewrite: Mutex<Option<(String, String)>>,
}

impl FakeRunner {
    pub fn new() -> Arc<FakeRunner> {
        Arc::new(FakeRunner::default())
    }

    pub fn on(&self, pred: impl Fn(&[String]) -> bool + Send + Sync + 'static, res: CommandResult) {
        self.rules.lock().unwrap().push((
            Arc::new(pred),
            Arc::new(move |_a: &[String], _s: Option<&[u8]>| Ok(res.clone())),
        ));
    }

    pub fn on_fn(
        &self,
        pred: impl Fn(&[String]) -> bool + Send + Sync + 'static,
        f: impl Fn(&[String]) -> CommandResult + Send + Sync + 'static,
    ) {
        self.rules.lock().unwrap().push((
            Arc::new(pred),
            Arc::new(move |a: &[String], _s: Option<&[u8]>| Ok(f(a))),
        ));
    }

    /// A responder that raises (Python `side_effect` raising inside the runner).
    pub fn on_err(&self, pred: impl Fn(&[String]) -> bool + Send + Sync + 'static, e: RunnerError) {
        self.rules.lock().unwrap().push((
            Arc::new(pred),
            Arc::new(move |_a: &[String], _s: Option<&[u8]>| Err(e.clone())),
        ));
    }

    /// Put a rule FIRST (Python `runner.rules.insert(0, ...)`).
    pub fn on_first(
        &self,
        pred: impl Fn(&[String]) -> bool + Send + Sync + 'static,
        f: impl Fn(&[String]) -> CommandResult + Send + Sync + 'static,
    ) {
        self.rules.lock().unwrap().insert(
            0,
            (
                Arc::new(pred),
                Arc::new(move |a: &[String], _s: Option<&[u8]>| Ok(f(a))),
            ),
        );
    }

    pub fn clear_rules(&self) {
        self.rules.lock().unwrap().clear();
    }

    pub fn clear_calls(&self) {
        self.calls.lock().unwrap().clear();
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    pub fn argvs(&self) -> Vec<Vec<String>> {
        self.calls().into_iter().map(|c| c.argv).collect()
    }
}

#[async_trait]
impl CommandRunner for FakeRunner {
    async fn run(&self, argv: &[String], opts: RunOpts) -> Result<CommandResult, RunnerError> {
        self.calls.lock().unwrap().push(Call {
            argv: argv.to_vec(),
            stdin: opts.stdin.clone(),
            cwd: opts.cwd.clone(),
            timeout: opts.timeout,
            max_bytes: opts.max_bytes,
        });
        let rules: Vec<(Pred, Resp)> = self.rules.lock().unwrap().clone();
        for (pred, resp) in rules {
            if pred(argv) {
                let mut res = resp(argv, opts.stdin.as_deref())?;
                if argv.len() >= 2
                    && argv[0] == "docker"
                    && argv[1] == "inspect"
                    && let Some((from, to)) = self.rewrite.lock().unwrap().clone()
                {
                    res.stdout = res.stdout.replace(&from, &to);
                }
                return Ok(res);
            }
        }
        Ok(CommandResult::new(127, "", "no rule"))
    }
}

/// Matches argv by PREFIX, longest prefix first, a later stub for the same prefix replaces the earlier one (Python
/// relay `FakeRunner`). Unmatched -> rc 127 so a missing stub is loud.
#[derive(Default)]
pub struct PrefixRunner {
    pub rules: Mutex<Vec<(Vec<String>, Resp)>>,
    pub calls: Mutex<Vec<Vec<String>>>,
}

impl PrefixRunner {
    pub fn new() -> Arc<PrefixRunner> {
        Arc::new(PrefixRunner::default())
    }

    pub fn on(&self, prefix: &[&str], res: CommandResult) {
        self.on_fn(prefix, move |_a| res.clone());
    }

    pub fn on_fn(
        &self,
        prefix: &[&str],
        f: impl Fn(&[String]) -> CommandResult + Send + Sync + 'static,
    ) {
        let p: Vec<String> = prefix.iter().map(|s| (*s).to_owned()).collect();
        let mut r = self.rules.lock().unwrap();
        r.retain(|(q, _)| *q != p);
        r.push((p, Arc::new(move |a: &[String], _s: Option<&[u8]>| Ok(f(a)))));
    }

    pub fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl CommandRunner for PrefixRunner {
    async fn run(&self, argv: &[String], opts: RunOpts) -> Result<CommandResult, RunnerError> {
        self.calls.lock().unwrap().push(argv.to_vec());
        let mut rules: Vec<(Vec<String>, Resp)> = self.rules.lock().unwrap().clone();
        rules.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
        for (prefix, resp) in rules {
            if argv.len() >= prefix.len() && argv[..prefix.len()] == prefix[..] {
                return resp(argv, opts.stdin.as_deref());
            }
        }
        Ok(CommandResult::new(
            127,
            "",
            &format!("no fake for {argv:?}"),
        ))
    }
}

pub fn ok(stdout: &str) -> CommandResult {
    CommandResult::new(0, stdout, "")
}

pub fn ok2(stdout: &str, stderr: &str) -> CommandResult {
    CommandResult::new(0, stdout, stderr)
}

pub fn fail(stderr: &str) -> CommandResult {
    CommandResult::new(1, "", stderr)
}

pub fn rc(code: i32, stdout: &str, stderr: &str) -> CommandResult {
    CommandResult::new(code, stdout, stderr)
}

/// An adapter context with a fixed EMPTY environment (hermetic) and the given runner, urls and paths.
pub fn ctx(
    runner: Arc<dyn CommandRunner>,
    urls: &[(&str, &str)],
    paths: &[(&str, PathBuf)],
    state_dir: &Path,
) -> AdapterContext {
    AdapterContext {
        runner,
        http: Http::new().unwrap(),
        urls: urls
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        paths: paths
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect::<IndexMap<String, PathBuf>>(),
        secret_env: IndexMap::new(),
        state_dir: state_dir.to_path_buf(),
        environ: Environ::from_pairs(&[]),
    }
}

/// Collect `n` events from a live tail (fails after `secs`).
pub async fn take_events(mut s: EventStream, n: usize, secs: u64) -> Vec<SystemEvent> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while out.len() < n {
        match tokio::time::timeout_at(deadline, s.next()).await {
            Ok(Some(Ok(ev))) => out.push(ev),
            Ok(Some(Err(e))) => panic!("stream error {e:?}"),
            Ok(None) | Err(_) => break,
        }
    }
    out
}

/// A loopback URL nothing listens on (a ConnectError for every request).
pub async fn closed_url() -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    drop(l);
    format!("http://{a}")
}
