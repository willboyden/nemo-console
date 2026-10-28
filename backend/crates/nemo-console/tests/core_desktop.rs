//! Port of backend/tests/core/test_desktop.py: `nemo-console open` / `stop` — URL, ownership check, background start
//! and stop. All faked through `desktop::Launcher` (Python monkeypatches the module): nothing is started, opened or
//! signalled.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nemo_console::config::Settings;
use nemo_console::desktop::{
    self, Check, LaunchedChild, Launcher, LauncherError, SERVER_ARGS, STOP_TIMEOUT_S, SpawnSpec,
};
use nix::sys::signal::Signal;
use serde_json::{Map, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const URL: &str = "http://127.0.0.1:8793/";
const TCP_HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";

fn cfg(host: &str, port: i64, state_dir: &Path) -> Settings {
    let mut init = Map::new();
    init.insert("host".to_owned(), json!(host));
    init.insert("port".to_owned(), json!(port));
    init.insert("state_dir".to_owned(), json!(state_dir.to_string_lossy()));
    Settings::from_sources(None, &[], init).unwrap()
}

fn url_of(host: &str, port: i64) -> Result<String, LauncherError> {
    let tmp = tempfile::tempdir().unwrap();
    desktop::console_url(&cfg(host, port, tmp.path()))
}

/// Python's FakeProc.
struct FakeProc {
    exit_code: Option<i32>,
    terminated: Arc<AtomicBool>,
}

impl LaunchedChild for FakeProc {
    fn pid(&self) -> u32 {
        4242
    }
    fn poll(&mut self) -> Option<i32> {
        self.exit_code
    }
    fn terminate(&mut self) {
        self.terminated.store(true, Ordering::SeqCst);
    }
    fn wait(&mut self, _timeout: Duration) -> bool {
        true
    }
    fn kill(&mut self) {}
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

// --- URL -------------------------------------------------------------------------------------------------------

#[test]
fn test_console_url_maps_wildcard_binds_to_loopback() {
    assert_eq!(url_of("127.0.0.1", 8793).unwrap(), URL);
    assert_eq!(url_of("0.0.0.0", 9000).unwrap(), "http://127.0.0.1:9000/");
    assert_eq!(url_of("::", 8793).unwrap(), "http://[::1]:8793/");
    assert_eq!(url_of("::1", 8793).unwrap(), "http://[::1]:8793/");
    assert_eq!(url_of("localhost", 8793).unwrap(), "http://localhost:8793/");
}

#[test]
fn test_console_url_refuses_anything_off_the_box() {
    for host in [
        "127.0.0.1@evil.example",
        "evil.example/#",
        "evil.example",
        "172.16.1.7",
        "10.0.0.1",
    ] {
        let e = url_of(host, 8793).unwrap_err();
        assert!(e.0.contains("not a loopback address"), "{host}: {e}");
    }
}

#[test]
fn test_console_url_refuses_an_out_of_range_port() {
    assert!(
        url_of("127.0.0.1", 70000)
            .unwrap_err()
            .0
            .contains("out of range")
    );
}

// --- who holds the port ----------------------------------------------------------------------------------------

fn set(v: Option<Vec<u32>>) -> std::collections::BTreeSet<u32> {
    v.unwrap().into_iter().collect()
}

#[test]
fn test_listen_uids_reads_every_listener_on_the_port() {
    let tmp = tempfile::tempdir().unwrap();
    let (tcp, tcp6) = (tmp.path().join("tcp"), tmp.path().join("tcp6"));
    std::fs::write(
        &tcp,
        format!(
            "{TCP_HEADER}   0: 0100007F:2259 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 11\n   1: 0100007F:2259 0100007F:9C40 01 00000000:00000000 00:00000000 00000000     0        0 12\n   2: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 13\n"
        ),
    )
    .unwrap();
    std::fs::write(
        &tcp6,
        format!(
            "{TCP_HEADER}   0: 00000000000000000000000000000000:2259 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000   999        0 14\n"
        ),
    )
    .unwrap();
    let (t, t6) = (
        tcp.to_string_lossy().into_owned(),
        tcp6.to_string_lossy().into_owned(),
    );
    // ESTABLISHED and other ports ignored
    assert_eq!(
        set(desktop::listen_uids(8793, &[&t])),
        [1000].into_iter().collect()
    );
    // a wildcard IPv6 squatter counts
    assert_eq!(
        set(desktop::listen_uids(8793, &[&t, &t6])),
        [1000, 999].into_iter().collect()
    );
    let missing = tmp.path().join("missing").to_string_lossy().into_owned();
    assert!(set(desktop::listen_uids(8793, &[&missing])).is_empty());
}

#[test]
fn test_check_flags_a_console_shaped_answer_from_another_users_socket() {
    let me = desktop::my_uid();
    assert_ne!(me, 0, "the test needs a non-root user");
    assert_eq!(
        desktop::classify(Check::Console, Some(&[0]), me),
        Check::OtherUser
    );
    assert_eq!(
        desktop::classify(Check::Console, Some(&[me, 0]), me),
        Check::OtherUser
    );
    assert_eq!(
        desktop::classify(Check::Console, None, me),
        Check::OtherUser
    ); // unreadable: fail closed
    assert_eq!(
        desktop::classify(Check::Console, Some(&[me]), me),
        Check::Console
    );
}

#[tokio::test]
async fn test_probe_tells_console_foreign_and_down_apart() {
    let health = json!({"status": "degraded", "version": "0.1.1", "trust": "bearer", "role": "public",
                        "trust_notice": null, "systems": {}});
    for (t, want) in [
        (
            ResponseTemplate::new(200).set_body_json(health),
            Check::Console,
        ),
        (
            ResponseTemplate::new(200).set_body_string("<html>some other service</html>"),
            Check::Foreign,
        ),
        (
            ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})),
            Check::Foreign,
        ),
    ] {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/health"))
            .respond_with(t)
            .mount(&s)
            .await;
        assert_eq!(desktop::probe(&format!("{}/", s.uri())).await, want);
    }
    assert_eq!(desktop::probe("http://127.0.0.1:9/").await, Check::Down); // connection refused
}

// --- open ------------------------------------------------------------------------------------------------------

#[derive(Default)]
struct Rec {
    checks: Mutex<Vec<Check>>,
    started: AtomicUsize,
    opened: Mutex<Vec<String>>,
    notes: Mutex<Vec<String>>,
    proc_exit: Mutex<Option<i32>>,
    terminated: Arc<AtomicBool>,
}

/// Python's `launcher` fixture: check answers from `checks` (the last one repeats), start/open/notify recorded.
fn launcher(rec: &Arc<Rec>) -> Launcher {
    let (r1, r2, r3, r4) = (
        Arc::clone(rec),
        Arc::clone(rec),
        Arc::clone(rec),
        Arc::clone(rec),
    );
    Launcher {
        check: Box::new(move |url: &str, port: u16| {
            assert_eq!((url, port), (URL, 8793));
            let mut c = r1.checks.lock().unwrap();
            let st = if c.len() > 1 { c.remove(0) } else { c[0] };
            Box::pin(async move { st })
        }),
        start_background: Box::new(move |_cfg: &Settings| {
            r2.started.fetch_add(1, Ordering::SeqCst);
            let exit_code = *r2.proc_exit.lock().unwrap();
            Ok(Box::new(FakeProc {
                exit_code,
                terminated: Arc::clone(&r2.terminated),
            }) as Box<dyn LaunchedChild>)
        }),
        open_browser: Box::new(move |url: &str| {
            r3.opened.lock().unwrap().push(url.to_owned());
            true
        }),
        notify: Box::new(move |m: &str| r4.notes.lock().unwrap().push(m.to_owned())),
        poll_s: 0.0,
        ..Launcher::default()
    }
}

fn fixture(checks: &[Check]) -> (Arc<Rec>, tempfile::TempDir, Settings) {
    let tmp = tempfile::tempdir().unwrap();
    let rec = Arc::new(Rec::default());
    *rec.checks.lock().unwrap() = checks.to_vec();
    let c = cfg("127.0.0.1", 8793, tmp.path());
    (rec, tmp, c)
}

fn notes(rec: &Rec) -> Vec<String> {
    rec.notes.lock().unwrap().clone()
}

fn opened(rec: &Rec) -> Vec<String> {
    rec.opened.lock().unwrap().clone()
}

#[test]
fn test_open_when_already_running_starts_nothing() {
    let (rec, _t, c) = fixture(&[Check::Console]);
    assert_eq!(desktop::open_console_with(&[], Some(c), &launcher(&rec)), 0);
    assert_eq!(rec.started.load(Ordering::SeqCst), 0);
    assert_eq!(opened(&rec), [URL]);
    assert!(notes(&rec).is_empty());
}

#[test]
fn test_open_starts_the_console_then_waits_for_health() {
    let (rec, _t, c) = fixture(&[Check::Down, Check::Down, Check::Console]);
    assert_eq!(desktop::open_console_with(&[], Some(c), &launcher(&rec)), 0);
    assert_eq!(rec.started.load(Ordering::SeqCst), 1);
    assert_eq!(opened(&rec), [URL]);
}

#[test]
fn test_open_refuses_a_port_it_cannot_trust() {
    for (state, note) in [
        (Check::Foreign, "something other than Nemo Console"),
        (Check::OtherUser, "another user's process"),
    ] {
        let (rec, _t, c) = fixture(&[state]);
        assert_eq!(desktop::open_console_with(&[], Some(c), &launcher(&rec)), 1);
        assert_eq!(rec.started.load(Ordering::SeqCst), 0);
        assert!(opened(&rec).is_empty());
        assert!(notes(&rec)[0].contains(note), "{:?}", notes(&rec));
    }
}

#[test]
fn test_open_refuses_a_squatter_that_wins_the_port_during_start() {
    let (rec, _t, c) = fixture(&[Check::Down, Check::OtherUser]); // our child lost the bind; the squatter answers
    *rec.proc_exit.lock().unwrap() = Some(1);
    assert_eq!(desktop::open_console_with(&[], Some(c), &launcher(&rec)), 1);
    assert!(opened(&rec).is_empty());
    assert!(notes(&rec)[0].contains("another user's process"));
}

#[test]
fn test_open_reports_a_console_that_exits_during_start() {
    let (rec, _t, c) = fixture(&[Check::Down]);
    *rec.proc_exit.lock().unwrap() = Some(1);
    assert_eq!(desktop::open_console_with(&[], Some(c), &launcher(&rec)), 1);
    assert!(opened(&rec).is_empty());
    assert!(notes(&rec)[0].contains("console.log"));
}

#[test]
fn test_open_stops_its_child_after_the_start_timeout() {
    let (rec, _t, c) = fixture(&[Check::Down]);
    let l = Launcher {
        start_timeout_s: 0.0,
        ..launcher(&rec)
    };
    assert_eq!(desktop::open_console_with(&[], Some(c), &l), 1);
    assert!(opened(&rec).is_empty());
    assert!(rec.terminated.load(Ordering::SeqCst));
}

#[test]
fn test_open_refuses_a_non_loopback_host_before_probing() {
    let (rec, tmp, _c) = fixture(&[Check::Console]);
    assert_eq!(
        desktop::open_console_with(
            &[],
            Some(cfg("evil.example", 8793, tmp.path())),
            &launcher(&rec)
        ),
        1
    );
    assert_eq!(rec.started.load(Ordering::SeqCst), 0);
    assert!(notes(&rec)[0].contains("not a loopback address"));
}

#[test]
fn test_open_and_stop_reject_arguments() {
    let (rec, _t, c) = fixture(&[Check::Console]);
    let args = |a: &[&str]| -> Vec<String> { a.iter().map(|s| (*s).to_owned()).collect() };
    assert_eq!(
        desktop::open_console_with(&args(&["--port", "1"]), Some(c.clone()), &launcher(&rec)),
        2
    );
    assert_eq!(
        desktop::stop_console_with(&args(&["now"]), Some(c), &launcher(&rec)),
        2
    );
    assert_eq!(rec.started.load(Ordering::SeqCst), 0);
}

// --- background start ------------------------------------------------------------------------------------------

type Spawned = Arc<Mutex<Vec<(String, Vec<String>, bool)>>>;

/// Python's `popen` fixture: record what would be started.
fn recording_spawn(
    calls: Spawned,
) -> impl Fn(SpawnSpec) -> Result<Box<dyn LaunchedChild>, LauncherError> + Send + Sync {
    move |spec: SpawnSpec| {
        calls.lock().unwrap().push((
            spec.program.to_string_lossy().into_owned(),
            spec.args.clone(),
            spec.new_process_group,
        ));
        Ok(Box::new(FakeProc {
            exit_code: None,
            terminated: Arc::new(AtomicBool::new(false)),
        }) as Box<dyn LaunchedChild>)
    }
}

#[test]
fn test_start_background_is_isolated_detached_and_private() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let calls: Spawned = Arc::default();
    desktop::start_background_with(
        &cfg("127.0.0.1", 8793, &state),
        desktop::LOG_ROTATE_BYTES,
        &recording_spawn(Arc::clone(&calls)),
    )
    .unwrap();
    let (program, args, own_group) = calls.lock().unwrap()[0].clone();
    let exe = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(program, exe);
    assert_eq!(args, SERVER_ARGS);
    assert!(own_group);
    assert_eq!(mode(&state), 0o700);
    for name in ["console.log", "console.pid"] {
        assert_eq!(mode(&state.join(name)), 0o600, "{name}");
    }
    assert_eq!(
        std::fs::read_to_string(state.join("console.pid")).unwrap(),
        "4242\n"
    );
}

#[test]
fn test_start_background_refuses_a_symlinked_log() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, target) = (tmp.path().join("state"), tmp.path().join("elsewhere"));
    std::fs::create_dir(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(&target, "keep\n").unwrap();
    std::os::unix::fs::symlink(&target, state.join("console.log")).unwrap();
    let calls: Spawned = Arc::default();
    let res = desktop::start_background_with(
        &cfg("127.0.0.1", 8793, &state),
        desktop::LOG_ROTATE_BYTES,
        &recording_spawn(Arc::clone(&calls)),
    );
    assert!(res.is_err());
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep\n");
}

#[test]
fn test_start_background_tightens_an_existing_log_and_rotates_a_big_one() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
    let log = state.join("console.log");
    std::fs::write(&log, "x".repeat(20)).unwrap();
    std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).unwrap();
    let calls: Spawned = Arc::default();
    desktop::start_background_with(
        &cfg("127.0.0.1", 8793, &state),
        10,
        &recording_spawn(Arc::clone(&calls)),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(state.join("console.log.1")).unwrap(),
        "x".repeat(20)
    );
    assert_eq!(mode(&log), 0o600);
    assert_eq!(mode(&state), 0o700);
}

// --- stop ------------------------------------------------------------------------------------------------------

#[derive(Default)]
struct StopRec {
    alive: Mutex<Vec<(u32, Vec<String>)>>,
    signals: Mutex<Vec<(i32, Signal)>>,
    notes: Mutex<Vec<String>>,
}

/// Python's `stopper` fixture.
fn stopper() -> (Arc<StopRec>, tempfile::TempDir, Settings, Launcher) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("console.pid"), "4242\n").unwrap();
    let rec = Arc::new(StopRec::default());
    let (r1, r2, r3) = (Arc::clone(&rec), Arc::clone(&rec), Arc::clone(&rec));
    let l = Launcher {
        pid_cmdline: Box::new(move |pid: i32| {
            assert_eq!(pid, 4242);
            let mut a = r1.alive.lock().unwrap();
            if a.is_empty() {
                None
            } else {
                Some(a.remove(0))
            }
        }),
        signal: Box::new(move |pid: i32, sig: Signal| {
            r2.signals.lock().unwrap().push((pid, sig));
            Ok(())
        }),
        notify: Box::new(move |m: &str| r3.notes.lock().unwrap().push(m.to_owned())),
        poll_s: 0.0,
        ..Launcher::default()
    };
    let c = cfg("127.0.0.1", 8793, tmp.path());
    (rec, tmp, c, l)
}

fn me() -> (u32, Vec<String>) {
    let exe = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    (
        desktop::my_uid(),
        std::iter::once(exe)
            .chain(SERVER_ARGS.iter().map(|s| (*s).to_owned()))
            .collect(),
    )
}

#[test]
fn test_stop_terminates_the_console_it_started() {
    let (rec, tmp, c, l) = stopper();
    *rec.alive.lock().unwrap() = vec![me(), me()]; // identity check, then one poll before it exits
    assert_eq!(desktop::stop_console_with(&[], Some(c), &l), 0);
    assert_eq!(*rec.signals.lock().unwrap(), vec![(4242, Signal::SIGTERM)]);
    assert!(!tmp.path().join("console.pid").exists());
}

#[test]
fn test_stop_never_signals_a_process_it_did_not_start() {
    let (_, mine) = me();
    let cases = vec![
        (0, mine), // another user
        (
            desktop::my_uid(),
            vec![
                "/usr/bin/vim".to_owned(),
                "backend/src/nemo_console/main.py".to_owned(),
            ],
        ), // recycled
    ];
    for proc_ in cases {
        let (rec, _tmp, c, l) = stopper();
        *rec.alive.lock().unwrap() = vec![proc_];
        assert_eq!(desktop::stop_console_with(&[], Some(c), &l), 1);
        assert!(rec.signals.lock().unwrap().is_empty());
        assert!(rec.notes.lock().unwrap()[0].contains("nothing stopped"));
    }
}

#[test]
fn test_stop_reports_a_console_that_is_already_gone() {
    let (rec, tmp, c, l) = stopper();
    assert_eq!(desktop::stop_console_with(&[], Some(c), &l), 1);
    assert!(rec.signals.lock().unwrap().is_empty());
    assert!(!tmp.path().join("console.pid").exists());
}

#[tokio::test]
async fn test_server_shutdown_grace_is_bounded_below_the_stop_timeout() {
    // with a browser holding an SSE stream the server must give up on open connections before `stop` gives up on it
    let cfg = Settings::from_sources(None, &[], Map::new()).unwrap();
    let grace = nemo_console::server::graceful_shutdown_s(&cfg);
    #[allow(clippy::cast_precision_loss)]
    let g = grace as f64;
    assert!(grace > 0 && g < STOP_TIMEOUT_S, "{grace}");
    // and the exit hook wakes the SSE streams before the server starts waiting for connections
    let env = common::Env::new(None);
    assert!(!env.state.shutdown.is_set());
    nemo_console::server::on_exit_signal(&env.state);
    assert!(env.state.shutdown.is_set());
}
