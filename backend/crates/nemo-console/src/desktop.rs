//! `nemo-console open` / `nemo-console stop`: the desktop launcher's entry points (Linux). Port of `desktop.py`.
//!
//! `open` starts the console in the background if nothing is serving yet, waits for `/api/v1/health`, then opens the
//! UI with `xdg-open`. It opens a page only on a loopback address, and only when every socket listening on the port
//! belongs to the current user and the answer looks like Nemo Console. It never reads or passes a key. `stop` stops
//! the console that `open` started (tracked in `<state dir>/console.pid`).
//!
//! Cross-implementation contract (PORTING.md D-4): the background server is started as
//! `<exe> -I -m nemo_console.main` — Python's own SERVER_ARGS, which the Rust binary treats as "serve" exactly like
//! Python ignores unknown arguments — so `stop` (either implementation) recognises a console started by `open`
//! (either implementation) with Python's unchanged `argv[1:] == SERVER_ARGS` check.
//!
//! Test seams: what the Python tests monkeypatch (`check`, `start_background`, `open_browser`, `notify`,
//! `_pid_cmdline`, `_signal`, `POLL_S`, `START_TIMEOUT_S`, `subprocess.Popen`, `LOG_ROTATE_BYTES`) is a field of
//! [`Launcher`] (or a parameter of [`start_background_with`]); [`Launcher::default`] is the real implementation.

use std::fs::{File, OpenOptions, Permissions};
use std::io::{Read, Write};
use std::net::IpAddr;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use nix::sys::signal::Signal;

use crate::config::Settings;
use crate::services::pyfmt::str_repr;

pub const START_TIMEOUT_S: f64 = 30.0;
pub const STOP_TIMEOUT_S: f64 = 10.0;
pub const POLL_S: f64 = 0.5;
pub const LOG_ROTATE_BYTES: u64 = 10 * 1024 * 1024;
pub const HEALTH_KEYS: [&str; 4] = ["status", "version", "trust", "systems"];
pub const PROC_NET_TABLES: [&str; 2] = ["/proc/net/tcp", "/proc/net/tcp6"];
pub const SERVER_ARGS: [&str; 3] = ["-I", "-m", "nemo_console.main"];

/// A refusal the launcher reports (notification + exit status 1). Python's `OSError`s are reported the same way, so
/// they become a `LauncherError` too.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LauncherError(pub String);

fn err(msg: impl Into<String>) -> LauncherError {
    LauncherError(msg.into())
}

fn os(e: &std::io::Error) -> LauncherError {
    err(e.to_string())
}

/// The console's URL, loopback only: neither the probe nor the browser may be sent off the box.
pub fn console_url(cfg: &Settings) -> Result<String, LauncherError> {
    let mut host = cfg.host.trim().to_owned();
    if host.is_empty() || host == "0.0.0.0" {
        host = "127.0.0.1".to_owned();
    } else if host == "::" || host == "[::]" {
        host = "::1".to_owned();
    }
    if host != "localhost" {
        let bare = host.strip_prefix('[').unwrap_or(&host);
        let bare = bare.strip_suffix(']').unwrap_or(bare);
        let ip: Option<IpAddr> = bare.parse().ok();
        match ip {
            Some(ip) if ip.is_loopback() => {
                host = if ip.is_ipv6() {
                    format!("[{ip}]")
                } else {
                    ip.to_string()
                };
            }
            _ => {
                return Err(err(format!(
                    "host {} is not a loopback address; the launcher only opens a local console",
                    str_repr(&cfg.host)
                )));
            }
        }
    }
    if !(1..=65535).contains(&cfg.port) {
        return Err(err(format!("port {} is out of range", cfg.port)));
    }
    Ok(format!("http://{host}:{}/", cfg.port))
}

/// Owners of every LISTEN socket on `port`, any address, IPv4 and IPv6 (a set: each uid once). None if a table is
/// unreadable.
#[must_use]
pub fn listen_uids(port: u16, tables: &[&str]) -> Option<Vec<u32>> {
    let mut uids = Vec::new();
    for table in tables {
        let text = match std::fs::read_to_string(table) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue, // no IPv6
            Err(_) => return None,
        };
        for row in text.lines().skip(1) {
            let f: Vec<&str> = row.split_whitespace().collect();
            if f.len() > 7
                && f[3] == "0A"
                && f[1]
                    .rsplit(':')
                    .next()
                    .and_then(|p| u16::from_str_radix(p, 16).ok())
                    == Some(port)
                && let Ok(uid) = f[7].parse::<u32>()
                && !uids.contains(&uid)
            {
                uids.push(uid);
            }
        }
    }
    Some(uids)
}

/// The current user's uid.
#[must_use]
pub fn my_uid() -> u32 {
    nix::unistd::getuid().as_raw()
}

/// `held_by_me` with the listener owners given: every listener is ours (an unreadable table fails closed).
#[must_use]
pub fn held_by(uids: Option<&[u32]>, me: u32) -> bool {
    match uids {
        Some(u) => !u.is_empty() && u.iter().all(|x| *x == me),
        None => false,
    }
}

#[must_use]
pub fn held_by_me(port: u16) -> bool {
    held_by(listen_uids(port, &PROC_NET_TABLES).as_deref(), my_uid())
}

/// What answers on the console's URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    Console,
    OtherUser,
    Foreign,
    Down,
}

/// `console` if the answer to /health has the console's shape, `foreign` for any other answer, `down` for none.
pub async fn probe(url: &str) -> Check {
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return Check::Down;
    };
    let Ok(resp) = client.get(format!("{url}api/v1/health")).send().await else {
        return Check::Down;
    };
    let Ok(bytes) = resp.bytes().await else {
        return Check::Down;
    };
    match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(serde_json::Value::Object(m)) if HEALTH_KEYS.iter().all(|k| m.contains_key(*k)) => {
            Check::Console
        }
        _ => Check::Foreign,
    }
}

/// `check()` with the probe answer and the listener owners given: an answer shaped like the console from a port that
/// is not entirely ours is `OtherUser`.
#[must_use]
pub fn classify(state: Check, uids: Option<&[u32]>, me: u32) -> Check {
    if state == Check::Console && !held_by(uids, me) {
        return Check::OtherUser; // answers like the console, but not every listener is ours
    }
    state
}

pub async fn check(url: &str, port: u16) -> Check {
    let state = probe(url).await;
    classify(
        state,
        listen_uids(port, &PROC_NET_TABLES).as_deref(),
        my_uid(),
    )
}

fn private_state_dir(state_dir: &Path) -> Result<(), LauncherError> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state_dir)
        .map_err(|e| os(&e))?;
    let md = std::fs::symlink_metadata(state_dir).map_err(|e| os(&e))?;
    if !md.is_dir() || md.uid() != my_uid() {
        return Err(err(format!(
            "state dir {} is not a directory you own",
            state_dir.display()
        )));
    }
    std::fs::set_permissions(state_dir, Permissions::from_mode(0o700)).map_err(|e| os(&e))
}

/// Open a regular file we own, never through a symlink or a FIFO, and force mode 0600.
fn open_private(path: &Path, truncate: bool) -> Result<File, LauncherError> {
    let f = OpenOptions::new()
        .write(true)
        .create(true)
        .append(!truncate)
        .truncate(truncate)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| os(&e))?;
    let md = f.metadata().map_err(|e| os(&e))?;
    if !md.is_file() || md.uid() != my_uid() {
        return Err(err(format!(
            "{} is not a regular file you own",
            path.display()
        )));
    }
    f.set_permissions(Permissions::from_mode(0o600))
        .map_err(|e| os(&e))?;
    Ok(f)
}

fn rotate(log: &Path, limit: u64) -> Result<(), LauncherError> {
    if let Ok(md) = std::fs::symlink_metadata(log)
        && md.is_file()
        && md.len() > limit
    {
        let name = log
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        std::fs::rename(log, log.with_file_name(format!("{name}.1"))).map_err(|e| os(&e))?;
    }
    Ok(())
}

/// A started background console (Python: the `subprocess.Popen` object).
pub trait LaunchedChild: Send {
    fn pid(&self) -> u32;
    /// `poll()`: the exit code once it has exited.
    fn poll(&mut self) -> Option<i32>;
    /// `terminate()` (SIGTERM).
    fn terminate(&mut self);
    /// `wait(timeout)`: true when it exited in time.
    fn wait(&mut self, timeout: Duration) -> bool;
    /// `kill()` (SIGKILL).
    fn kill(&mut self);
}

struct RealChild(std::process::Child);

impl LaunchedChild for RealChild {
    fn pid(&self) -> u32 {
        self.0.id()
    }

    fn poll(&mut self) -> Option<i32> {
        match self.0.try_wait() {
            Ok(Some(st)) => Some(st.code().unwrap_or(-1)),
            _ => None,
        }
    }

    fn terminate(&mut self) {
        if let Ok(pid) = i32::try_from(self.0.id()) {
            let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), Signal::SIGTERM);
        }
    }

    fn wait(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    fn kill(&mut self) {
        let _ = self.0.kill();
    }
}

/// What the launcher asks the OS to start (Python: `Popen(args, start_new_session=True, stdout=log, stderr=log)`).
pub struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// The child leads its own process group (Python `start_new_session=True`; see PORTING.md D-3).
    pub new_process_group: bool,
    /// The console log the child's stdout and stderr go to.
    pub log: File,
}

/// The spawn function of [`start_background_with`] (tests record instead of starting).
pub type SpawnFn = dyn Fn(SpawnSpec) -> Result<Box<dyn LaunchedChild>, LauncherError> + Send + Sync;

/// The real spawn: stdin from /dev/null, stdout+stderr to the log, own process group.
pub fn real_spawn(spec: SpawnSpec) -> Result<Box<dyn LaunchedChild>, LauncherError> {
    use std::os::unix::process::CommandExt;
    let errf = spec.log.try_clone().map_err(|e| os(&e))?;
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .stdin(Stdio::null())
        .stdout(spec.log)
        .stderr(errf);
    if spec.new_process_group {
        cmd.process_group(0);
    }
    let child = cmd.spawn().map_err(|e| os(&e))?;
    Ok(Box::new(RealChild(child)))
}

/// `start_background(cfg)` with the spawn function and the rotation size given.
pub fn start_background_with(
    cfg: &Settings,
    rotate_bytes: u64,
    spawn: &SpawnFn,
) -> Result<Box<dyn LaunchedChild>, LauncherError> {
    private_state_dir(&cfg.state_dir)?;
    let log = cfg.state_dir.join("console.log");
    rotate(&log, rotate_bytes)?;
    let out = open_private(&log, false)?;
    let program = std::env::current_exe().map_err(|e| os(&e))?;
    let child = spawn(SpawnSpec {
        program,
        args: SERVER_ARGS.iter().map(|s| (*s).to_owned()).collect(),
        new_process_group: true,
        log: out,
    })?;
    let mut pid = open_private(&cfg.state_dir.join("console.pid"), true)?;
    pid.write_all(format!("{}\n", child.pid()).as_bytes())
        .map_err(|e| os(&e))?;
    Ok(child)
}

/// `start_background(cfg)`: the real one.
pub fn start_background(cfg: &Settings) -> Result<Box<dyn LaunchedChild>, LauncherError> {
    start_background_with(cfg, LOG_ROTATE_BYTES, &real_spawn)
}

fn stop_child(child: &mut dyn LaunchedChild) {
    child.terminate();
    if !child.wait(Duration::from_secs(5)) {
        child.kill();
    }
}

/// `shutil.which(name)` on the process PATH.
fn which(name: &str) -> Option<PathBuf> {
    let path =
        std::env::var("PATH").unwrap_or_else(|_| crate::services::runner::DEFPATH.to_owned());
    crate::services::runner::which(name, &path)
}

/// `open_browser(url)`: xdg-open, detached; false when it is not installed.
#[must_use]
pub fn open_browser(url: &str) -> bool {
    let Some(opener) = which("xdg-open") else {
        return false;
    };
    use std::os::unix::process::CommandExt;
    Command::new(opener)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .is_ok()
}

/// `html.escape(message, quote=False)`.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `notify(message)`: stderr, plus `notify-send` when it exists (a desktop launch has no terminal).
#[allow(clippy::print_stderr)]
pub fn notify(message: &str) {
    eprintln!("nemo-console: {message}");
    if let Some(tool) = which("notify-send") {
        let child = Command::new(tool)
            .args(["--", "Nemo Console", &html_escape(message)])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut c) = child {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if matches!(c.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = c.kill();
        }
    }
}

/// (owner uid, argv) of a live process, or None if it is gone.
#[must_use]
pub fn pid_cmdline(pid: i32) -> Option<(u32, Vec<String>)> {
    let uid = std::fs::metadata(format!("/proc/{pid}")).ok()?.uid();
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let argv = raw
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    Some((uid, argv))
}

/// `_signal(pid, sig)`: `os.kill`.
pub fn send_signal(pid: i32, sig: Signal) -> Result<(), LauncherError> {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), sig).map_err(|e| err(e.to_string()))
}

type PidCmdlineFn = dyn Fn(i32) -> Option<(u32, Vec<String>)> + Send + Sync;
type CheckFn = dyn Fn(&str, u16) -> BoxFuture<'static, Check> + Send + Sync;
type StartFn = dyn Fn(&Settings) -> Result<Box<dyn LaunchedChild>, LauncherError> + Send + Sync;

/// Everything `open` and `stop` touch outside themselves. [`Launcher::default`] is the real one; tests replace fields
/// the way the Python tests monkeypatch module attributes.
pub struct Launcher {
    pub check: Box<CheckFn>,
    pub start_background: Box<StartFn>,
    pub open_browser: Box<dyn Fn(&str) -> bool + Send + Sync>,
    pub notify: Box<dyn Fn(&str) + Send + Sync>,
    pub pid_cmdline: Box<PidCmdlineFn>,
    pub signal: Box<dyn Fn(i32, Signal) -> Result<(), LauncherError> + Send + Sync>,
    pub poll_s: f64,
    pub start_timeout_s: f64,
    pub stop_timeout_s: f64,
}

impl Default for Launcher {
    fn default() -> Self {
        Launcher {
            check: Box::new(|url: &str, port: u16| {
                let url = url.to_owned();
                Box::pin(async move { check(&url, port).await })
            }),
            start_background: Box::new(start_background),
            open_browser: Box::new(open_browser),
            notify: Box::new(notify),
            pid_cmdline: Box::new(pid_cmdline),
            signal: Box::new(send_signal),
            poll_s: POLL_S,
            start_timeout_s: START_TIMEOUT_S,
            stop_timeout_s: STOP_TIMEOUT_S,
        }
    }
}

fn other_user(url: &str) -> LauncherError {
    err(format!(
        "another user's process is listening on the console's port ({url}); not opening it"
    ))
}

#[allow(clippy::print_stdout)]
async fn do_open(cfg: &Settings, l: &Launcher) -> Result<(), LauncherError> {
    let url = console_url(cfg)?;
    let port =
        u16::try_from(cfg.port).map_err(|_| err(format!("port {} is out of range", cfg.port)))?;
    let state = (l.check)(&url, port).await;
    match state {
        Check::OtherUser => return Err(other_user(&url)),
        Check::Foreign => {
            return Err(err(format!(
                "something other than Nemo Console answers on {url}; not opening it"
            )));
        }
        Check::Down => {
            let mut child = (l.start_background)(cfg)?;
            let log = cfg.state_dir.join("console.log");
            let deadline = Instant::now() + Duration::from_secs_f64(l.start_timeout_s.max(0.0));
            loop {
                let st = (l.check)(&url, port).await; // check first: a concurrent launch may have won
                if st == Check::Console {
                    break;
                }
                if st == Check::OtherUser {
                    stop_child(child.as_mut());
                    return Err(other_user(&url));
                }
                if child.poll().is_some() {
                    return Err(err(format!(
                        "the console did not start; see {}",
                        log.display()
                    )));
                }
                if Instant::now() > deadline {
                    stop_child(child.as_mut());
                    return Err(err(format!(
                        "the console did not become healthy in {START_TIMEOUT_S:.0} s; see {}",
                        log.display()
                    )));
                }
                tokio::time::sleep(Duration::from_secs_f64(l.poll_s.max(0.0))).await;
            }
            println!(
                "started Nemo Console in the background (pid {}); log: {}",
                child.pid(),
                log.display()
            );
        }
        Check::Console => {}
    }
    if !(l.open_browser)(&url) {
        return Err(err(format!("xdg-open not found; open {url} in a browser")));
    }
    Ok(())
}

#[allow(clippy::print_stdout)]
async fn do_stop(cfg: &Settings, l: &Launcher) -> Result<(), LauncherError> {
    let pid_file = cfg.state_dir.join("console.pid");
    let mut f = match OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(&pid_file)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(err("no console started by `nemo-console open` is recorded"));
        }
        Err(e) => return Err(os(&e)),
    };
    let mut buf = [0u8; 64];
    let n = f.read(&mut buf).map_err(|e| os(&e))?;
    drop(f);
    let text = String::from_utf8_lossy(&buf[..n]).trim().to_owned();
    if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit()) {
        return Err(err(format!("{} does not hold a pid", pid_file.display())));
    }
    let pid: i32 = text
        .parse()
        .map_err(|_| err(format!("{} does not hold a pid", pid_file.display())))?;
    let Some((uid, argv)) = (l.pid_cmdline)(pid) else {
        let _ = std::fs::remove_file(&pid_file);
        return Err(err(
            "the console started by `nemo-console open` is no longer running",
        ));
    };
    let args: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();
    if uid != my_uid() || args != SERVER_ARGS {
        // a recycled pid: never signal a stranger
        let _ = std::fs::remove_file(&pid_file);
        return Err(err(format!(
            "pid {pid} is not a console started by `nemo-console open`; nothing stopped"
        )));
    }
    (l.signal)(pid, Signal::SIGTERM)?;
    let deadline = Instant::now() + Duration::from_secs_f64(l.stop_timeout_s.max(0.0));
    while (l.pid_cmdline)(pid).is_some() {
        if Instant::now() > deadline {
            return Err(err(format!(
                "pid {pid} did not exit within {STOP_TIMEOUT_S:.0} s"
            )));
        }
        tokio::time::sleep(Duration::from_secs_f64(l.poll_s.max(0.0))).await;
    }
    let _ = std::fs::remove_file(&pid_file);
    println!("stopped Nemo Console (pid {pid})");
    Ok(())
}

#[allow(clippy::print_stderr)]
fn run(command: &str, argv: &[String], cfg: Option<Settings>, l: &Launcher) -> i32 {
    if !argv.is_empty() {
        eprintln!("usage: nemo-console {command}");
        return 2;
    }
    let cfg = match cfg {
        Some(c) => c,
        None => match Settings::load() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        },
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            (l.notify)(&e.to_string());
            return 1;
        }
    };
    let res = rt.block_on(async {
        if command == "open" {
            do_open(&cfg, l).await
        } else {
            do_stop(&cfg, l).await
        }
    });
    match res {
        Ok(()) => 0,
        Err(e) => {
            (l.notify)(&e.0);
            1
        }
    }
}

/// `nemo-console open` with the given launcher seams (tests); runs its own current-thread runtime, so call it from a
/// plain thread (or `spawn_blocking`), never from inside another runtime.
#[must_use]
pub fn open_console_with(argv: &[String], cfg: Option<Settings>, l: &Launcher) -> i32 {
    run("open", argv, cfg, l)
}

/// `nemo-console stop` with the given launcher seams (tests).
#[must_use]
pub fn stop_console_with(argv: &[String], cfg: Option<Settings>, l: &Launcher) -> i32 {
    run("stop", argv, cfg, l)
}

#[must_use]
pub fn open_console(argv: &[String], cfg: Option<Settings>) -> i32 {
    open_console_with(argv, cfg, &Launcher::default())
}

#[must_use]
pub fn stop_console(argv: &[String], cfg: Option<Settings>) -> i32 {
    stop_console_with(argv, cfg, &Launcher::default())
}
