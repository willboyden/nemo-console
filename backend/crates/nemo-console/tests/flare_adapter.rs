//! The NVIDIA Flare adapter: status from `flare-local status-json` (every sm120 and sm121 target), the flare.*
//! metrics, the actions (target, package and mode checks before anything runs; `up` in the background, one at a
//! time) and the read-only lane files. Offline: a fake launcher with the REAL output shape of
//! inference/flare/flare.sh status-json, no Docker, no GPU, no ssh.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

#[path = "fixtures/dcgm/support.rs"]
mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use nemo_console::adapters::base::SystemAdapter;
use nemo_console::adapters::flare::{CHIPS, FlareAdapter, parse_status};
use nemo_console::domain::models::{Danger, Health, ParamType, Severity, SystemId, SystemStatus};
use nemo_console::services::runner::{ALLOWED_BINARIES, FLARE_UP, MAX_TIMEOUT, max_timeout};
use serde_json::{Map, Value, json};
use support::{CLOSED, FakeRunner, fixed, ok, rc};

const L: &str = "flare-local";

/// One target in the shape `flare.sh status-json` prints (`json_target`).
fn tj(
    target: &str,
    chip: &str,
    family: &str,
    container: &str,
    health: &str,
    packages: &[&str],
    extra: &str,
) -> String {
    // neutral names: the real ones live only in the lab's launcher
    let hosts = match target {
        "local" => "\"local\"".to_owned(),
        "spark-all" => "\"spark-a\",\"spark-b\",\"spark-c\",\"spark-d\"".to_owned(),
        s => format!("\"{s}\""),
    };
    let pk: Vec<String> = packages.iter().map(|p| format!("\"{p}\"")).collect();
    let modes = match target {
        "local" => "tp1 tp2",
        "spark-all" => "tp4",
        _ => "tp1",
    };
    format!(
        r#"{{"target":"{target}","chip":"{chip}","family":"{family}","modes":"{modes}","hosts":[{hosts}],"url":"http://127.0.0.1:8035","build":"902c88e","container":"{container}","image":true,"health":"{health}","busy":"","packages":[{}]{extra}}}"#,
        pk.join(",")
    )
}

fn all_stopped(gb202: &[&str], gb10: &[&str]) -> String {
    let mut v = vec![
        tj(
            "local",
            "sm120",
            "gb202",
            "absent",
            "down",
            gb202,
            r#","models":null,"metrics":null"#,
        ),
        tj(
            "gpu-box",
            "sm120",
            "gb202",
            "absent",
            "down",
            gb202,
            r#","models":null,"metrics":null"#,
        ),
    ];
    for s in ["spark-a", "spark-b", "spark-c", "spark-d", "spark-all"] {
        v.push(tj(
            s,
            "sm121",
            "gb10",
            "absent",
            "down",
            gb10,
            r#","models":null,"metrics":null"#,
        ));
    }
    format!("[{}]\n", v.join(","))
}

const SERVING_EXTRA: &str = r#","models":{"object":"list","data":[{"id":"qwen3.8-flash","object":"model","max_model_len":262144}]},"metrics":{"generated_tokens":1200,"prefill_tokens":5000,"active":2,"pending":1,"completed":40,"failed":0,"accepted_drafts":300,"verification_drafts":400,"reused_tokens":900}"#;

fn serving_on_sparks() -> String {
    let mut v = vec![
        tj(
            "local",
            "sm120",
            "gb202",
            "absent",
            "down",
            &[],
            r#","models":null,"metrics":null"#,
        ),
        tj(
            "gpu-box",
            "sm120",
            "gb202",
            "absent",
            "down",
            &[],
            r#","models":null,"metrics":null"#,
        ),
    ];
    for s in ["spark-a", "spark-b", "spark-c", "spark-d"] {
        // single-Spark targets see no container of their own while the four-Spark ranks run (exact names)
        v.push(tj(
            s,
            "sm121",
            "gb10",
            "absent",
            "down",
            &["glm53-gb10-tp4.safetensors"],
            r#","models":null,"metrics":null"#,
        ));
    }
    v.push(tj(
        "spark-all",
        "sm121",
        "gb10",
        "running",
        "up",
        &["glm53-gb10-tp4.safetensors"],
        SERVING_EXTRA,
    ));
    format!("[{}]\n", v.join(","))
}

struct Env {
    tmp: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        Env {
            tmp: tempfile::tempdir().unwrap(),
        }
    }
    fn dir(&self) -> PathBuf {
        self.tmp.path().join("lab").join("inference").join("flare")
    }
    fn make(&self, runner: Arc<FakeRunner>, urls: &[(&str, &str)]) -> Arc<FlareAdapter> {
        let dir = self.dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("docker-compose.yml"),
            "name: ai-lab-flare\nservices:\n  flare:\n    image: lab/flare:902c88e\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("docker-compose.tp1.yml"),
            "services:\n  flare: {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("docker-compose.tp2.yml"),
            "services:\n  flare: {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ignition-config.toml"),
            "[paths]\ncuda = \"/usr/local/cuda\"\n",
        )
        .unwrap();
        let paths: Vec<(&str, &Path)> = vec![("flare_dir", &dir)];
        let ctx = support::ctx(runner, urls, &paths, &self.tmp.path().join("state"));
        let a = FlareAdapter::new(ctx);
        a.set_status_ttl(0.0);
        Arc::new(a)
    }
}

fn runner(status: &str) -> Arc<FakeRunner> {
    let r = FakeRunner::new();
    r.on_prefix(&[L, "status-json"], fixed(ok(status)));
    r
}

fn codes(st: &SystemStatus) -> Vec<String> {
    st.problems.iter().map(|p| p.code.clone()).collect()
}

fn params(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

#[test]
fn parse_status_reads_every_target_and_drops_what_it_cannot_trust() {
    let t = parse_status(&serving_on_sparks()).unwrap();
    assert_eq!(t.len(), 7);
    let sparks = t.iter().find(|x| x.target == "spark-all").unwrap();
    assert!(sparks.serving && sparks.chip == "sm121" && sparks.family == "gb10");
    assert_eq!(sparks.hosts, ["spark-a", "spark-b", "spark-c", "spark-d"]);
    assert_eq!(sparks.model.as_deref(), Some("qwen3.8-flash"));
    assert_eq!(sparks.max_model_len, Some(262_144.0));
    assert!(sparks.metrics.is_some());
    // a target name that is not a plain name and a package name with a path in it are dropped; garbage is None
    let odd = r#"[{"target":"Evil Name","chip":"sm120"},{"target":"local","packages":["ok.safetensors","../x","a b"],"container":""}]"#;
    let t = parse_status(odd).unwrap();
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].packages, ["ok.safetensors"]);
    assert_eq!(t[0].container, "absent");
    assert!(parse_status("not json").is_none());
}

#[test]
fn the_targets_cover_both_chip_families() {
    assert_eq!(CHIPS, ["sm120", "sm121"]);
    let t = parse_status(&all_stopped(&[], &[])).unwrap();
    let chip = |c: &str| {
        t.iter()
            .filter(|x| x.chip == c)
            .map(|x| x.target.as_str())
            .collect::<Vec<_>>()
    };
    assert_eq!(chip("sm120"), ["local", "gpu-box"]);
    assert_eq!(
        chip("sm121"),
        ["spark-a", "spark-b", "spark-c", "spark-d", "spark-all"]
    );
    // names that could be an option or a path are dropped
    let t = parse_status(r#"[{"target":"--rm","chip":"sm120"},{"target":"a/b"},{"target":"ok-1","hosts":["h1","-x"]}]"#).unwrap();
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].hosts, ["h1"]);
}

#[tokio::test]
async fn without_the_launcher_the_system_is_down_with_the_link_as_fix() {
    let e = Env::new();
    let a = e.make(FakeRunner::new(), &[]);
    let st = a.status().await.unwrap();
    assert_eq!(st.system, SystemId::Flare);
    assert_eq!(st.health, Health::Down);
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "flare.not_installed")
        .unwrap();
    assert_eq!(p.severity, Severity::Error);
    let fix = p.fix.clone().unwrap();
    assert!(
        fix.starts_with("ln -s ")
            && fix.contains("inference/flare/flare.sh")
            && fix.ends_with("~/.local/bin/flare-local"),
        "{fix}"
    );
    assert!(!a.metrics().await.unwrap().available);
}

#[tokio::test]
async fn stopped_everywhere_is_down_and_says_what_to_do_next() {
    let e = Env::new();
    let a = e.make(runner(&all_stopped(&[], &[])), &[]);
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Down);
    assert_eq!(codes(&st), ["flare.stopped"]);
    assert_eq!(
        st.problems[0].fix.as_deref(),
        Some("flare-local prepare-help")
    );
    assert_eq!(st.components.len(), 1 + 7);
    assert!(
        st.summary.contains("2 sm120") && st.summary.contains("5 sm121"),
        "{}",
        st.summary
    );
    // with packages the fix names `up`, and the counts split by family
    let a = e.make(
        runner(&all_stopped(
            &["q-tp2.safetensors"],
            &["g-gb10.safetensors"],
        )),
        &[],
    );
    let st = a.status().await.unwrap();
    assert!(
        st.problems[0].message.contains("1 for sm120")
            && st.problems[0].message.contains("1 for sm121")
    );
    assert!(
        st.problems[0]
            .fix
            .as_deref()
            .unwrap()
            .starts_with("flare-local up ")
    );
}

#[tokio::test]
async fn serving_on_the_sparks_is_up_with_its_endpoint_version_and_metrics() {
    let e = Env::new();
    let a = e.make(runner(&serving_on_sparks()), &[]);
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Up, "{:?}", st.problems);
    assert!(
        st.summary
            .contains("serving on spark-all (sm121): qwen3.8-flash"),
        "{}",
        st.summary
    );
    assert_eq!(st.endpoints.len(), 1);
    assert_eq!(st.endpoints[0].role, "api");
    assert_eq!(st.version.as_deref(), Some("902c88e")); // the pinned upstream commit of the image
    let c = st
        .components
        .iter()
        .find(|c| c.name == "flare @ spark-all")
        .unwrap();
    assert_eq!(c.state, "running");
    assert!(c.detail.as_deref().unwrap().contains("context 262144"));
    let m = a.metrics().await.unwrap();
    let get = |k: &str| {
        m.points
            .iter()
            .find(|p| p.key == k && p.labels.get("target").map(String::as_str) == Some("spark-all"))
            .map(|p| p.value)
    };
    assert_eq!(get("flare.generated_total"), Some(1200.0));
    assert_eq!(get("flare.active_requests"), Some(2.0));
    assert_eq!(get("flare.draft_acceptance"), Some(0.75));
    assert_eq!(
        m.points
            .iter()
            .find(|p| p.key == "flare.targets_serving")
            .unwrap()
            .value,
        1.0
    );
}

#[tokio::test]
async fn a_running_container_without_health_is_degraded_and_points_at_the_log() {
    let e = Env::new();
    let s = format!(
        "[{}]",
        tj(
            "gpu-box",
            "sm120",
            "gb202",
            "running",
            "down",
            &["b.safetensors"],
            r#","models":null,"metrics":null"#
        )
    );
    let a = e.make(runner(&s), &[]);
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Degraded);
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "flare.gpu-box.loading")
        .unwrap();
    assert_eq!(p.fix.as_deref(), Some("flare-local log gpu-box"));
    let s = format!(
        "[{}]",
        tj(
            "spark-b",
            "sm121",
            "gb10",
            "exited",
            "down",
            &[],
            r#","models":null,"metrics":null"#
        )
    );
    let st = e.make(runner(&s), &[]).status().await.unwrap();
    assert!(codes(&st).contains(&"flare.spark-b.exited".to_owned()));
}

#[tokio::test]
async fn actions_are_target_aware_and_offer_the_known_packages() {
    let e = Env::new();
    let a = e.make(
        runner(&all_stopped(
            &["q-tp2.safetensors"],
            &["g-gb10.safetensors"],
        )),
        &[],
    );
    let ids: Vec<String> = a.actions().unwrap().iter().map(|x| x.id.clone()).collect();
    assert_eq!(
        ids,
        [
            "targets",
            "preflight",
            "up",
            "down",
            "probe",
            "log",
            "stage",
            "last_start",
            "prepare_help"
        ]
    );
    let acts = a.actions().unwrap();
    let up = acts.iter().find(|x| x.id == "up").unwrap();
    assert_eq!(up.danger, Danger::Disruptive);
    assert_eq!(
        acts.iter().find(|x| x.id == "down").unwrap().danger,
        Danger::Disruptive
    );
    assert_eq!(
        acts.iter().find(|x| x.id == "preflight").unwrap().danger,
        Danger::Safe
    );
    // before any status call target and package are free text; after one both are choices from the launcher's report
    assert_eq!(
        up.params.iter().find(|p| p.key == "target").unwrap().type_,
        ParamType::String
    );
    assert_eq!(
        up.params.iter().find(|p| p.key == "package").unwrap().type_,
        ParamType::String
    );
    a.status().await.unwrap();
    let acts2 = a.actions().unwrap();
    let target = acts2
        .iter()
        .find(|x| x.id == "up")
        .unwrap()
        .params
        .iter()
        .find(|p| p.key == "target")
        .unwrap()
        .clone();
    assert_eq!(
        target.choices.unwrap(),
        [
            "local",
            "gpu-box",
            "spark-a",
            "spark-b",
            "spark-c",
            "spark-d",
            "spark-all"
        ]
    );
    let acts = a.actions().unwrap();
    let pk = acts
        .iter()
        .find(|x| x.id == "up")
        .unwrap()
        .params
        .iter()
        .find(|p| p.key == "package")
        .unwrap()
        .clone();
    assert_eq!(pk.type_, ParamType::Enum);
    assert_eq!(
        pk.choices.unwrap(),
        ["g-gb10.safetensors", "q-tp2.safetensors"]
    );
}

#[tokio::test]
async fn bad_targets_packages_and_modes_are_refused_before_anything_runs() {
    let e = Env::new();
    let r = runner(&all_stopped(&[], &[]));
    let a = e.make(Arc::clone(&r), &[]);
    for (p, why) in [
        (
            params(&[
                ("target", json!("nowhere")),
                ("package", json!("x.safetensors")),
            ]),
            "unknown target",
        ),
        (
            params(&[
                ("target", json!("local")),
                ("package", json!("../etc/passwd")),
            ]),
            "name a package",
        ),
        (
            params(&[
                ("target", json!("local")),
                ("package", json!("x.safetensors")),
                ("mode", json!("tp4")),
            ]),
            "does not take tp4",
        ),
        (
            params(&[
                ("target", json!("local")),
                ("package", json!("x.safetensors")),
            ]),
            "name the one the package was exported for",
        ),
        (
            params(&[
                ("target", json!("spark-a")),
                ("package", json!("x.safetensors")),
                ("mode", json!("tp2")),
            ]),
            "does not take tp2",
        ),
        (
            params(&[
                ("target", json!("spark-all")),
                ("package", json!("x.safetensors")),
                ("mode", json!("tp1")),
            ]),
            "does not take tp1",
        ),
    ] {
        for action in ["preflight", "up"] {
            let res = a.run_action(action, &p).await.unwrap();
            assert!(
                !res.ok && res.output.contains(why),
                "{action}: {}",
                res.output
            );
        }
    }
    assert_eq!(r.count(&[L, "up"]) + r.count(&[L, "preflight"]), 0);
    let res = a
        .run_action("stage", &params(&[("target", json!("local"))]))
        .await
        .unwrap();
    assert!(!res.ok && res.output.contains("needs no staging"));
    // without the launcher no target can be checked, so nothing runs
    let none = e.make(FakeRunner::new(), &[]);
    let res = none
        .run_action("down", &params(&[("target", json!("local"))]))
        .await
        .unwrap();
    assert!(!res.ok && res.output.contains("ln -s "), "{}", res.output);
    let res = a
        .run_action(
            "log",
            &params(&[("target", json!("local")), ("lines", json!(9000))]),
        )
        .await
        .unwrap();
    assert!(!res.ok);
    assert!(matches!(
        a.run_action("nope", &Map::new()).await,
        Err(nemo_console::adapters::base::AdapterError::UnknownId(_))
    ));
}

#[tokio::test]
async fn preflight_down_probe_and_log_pass_the_target_through() {
    let e = Env::new();
    let r = runner(&all_stopped(&[], &[]));
    r.on_prefix(
        &[L, "preflight"],
        fixed(ok(
            "preflight OK: g.safetensors on spark-all (sm121, gb10, tp4)\n",
        )),
    );
    r.on_prefix(&[L, "down"], fixed(ok("stopped Flare on spark-a\n")));
    r.on_prefix(
        &[L, "probe"],
        fixed(rc(1, "", "flare.sh: probe request failed: curl: (7)\n")),
    );
    r.on_prefix(&[L, "log"], fixed(ok("== spark-a\nready\n")));
    let a = e.make(Arc::clone(&r), &[]);
    let res = a
        .run_action(
            "preflight",
            &params(&[
                ("target", json!("spark-all")),
                ("package", json!("g.safetensors")),
                ("mode", json!("tp4")),
            ]),
        )
        .await
        .unwrap();
    assert!(res.ok && res.output.contains("preflight OK"));
    assert_eq!(
        r.called(&[L, "preflight"])[0],
        [
            "flare-local",
            "preflight",
            "spark-all",
            "g.safetensors",
            "tp4"
        ]
    );
    // auto adds no mode: flare.sh picks the target's only one
    a.run_action(
        "preflight",
        &params(&[
            ("target", json!("gpu-box")),
            ("package", json!("b.safetensors")),
        ]),
    )
    .await
    .unwrap();
    assert_eq!(
        r.called(&[L, "preflight"])[1],
        ["flare-local", "preflight", "gpu-box", "b.safetensors"]
    );
    assert!(
        a.run_action("down", &params(&[("target", json!("spark-a"))]))
            .await
            .unwrap()
            .ok
    );
    assert_eq!(
        r.called(&[L, "down"])[0],
        ["flare-local", "down", "spark-a"]
    );
    assert!(
        !a.run_action("probe", &params(&[("target", json!("gpu-box"))]))
            .await
            .unwrap()
            .ok
    );
    let res = a
        .run_action(
            "log",
            &params(&[("target", json!("spark-a")), ("lines", json!(20))]),
        )
        .await
        .unwrap();
    assert!(res.ok);
    assert_eq!(
        r.called(&[L, "log"])[0],
        ["flare-local", "log", "spark-a", "20"]
    );
    let res = a.run_action("targets", &Map::new()).await.unwrap();
    assert!(
        res.ok && res.output.contains("spark-all") && res.output.contains("sm121"),
        "{}",
        res.output
    );
}

// multi-thread: the fake launcher blocks a thread while it 'loads', as a real subprocess would not block the runtime
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn up_runs_in_the_background_one_at_a_time_and_reports_its_output() {
    let e = Env::new();
    let r = runner(&all_stopped(&[], &["g.safetensors"]));
    r.on_prefix(
        &[L, "up"],
        support::with(|_| {
            std::thread::sleep(Duration::from_millis(300));
            rc(
                1,
                "  spark-a is busy:\n  container fabric-x holds the GPU\n",
                "",
            )
        }),
    );
    let a = e.make(Arc::clone(&r), &[]);
    let p = params(&[
        ("target", json!("spark-all")),
        ("package", json!("g.safetensors")),
        ("mode", json!("tp4")),
    ]);
    let res = a.run_action("up", &p).await.unwrap();
    assert!(
        res.ok && res.output.contains("in the background"),
        "{}",
        res.output
    );
    let again = a.run_action("up", &p).await.unwrap();
    assert!(!again.ok && again.output.contains("already running"));
    let st = a.status().await.unwrap();
    assert!(st.components.iter().any(|c| c.name == "start on spark-all"));
    assert!(
        a.run_action("last_start", &Map::new())
            .await
            .unwrap()
            .output
            .contains("Still starting")
    );
    for _ in 0..100 {
        if a.last_start().and_then(|s| s.finished).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        r.called(&[L, "up"])[0],
        ["flare-local", "up", "spark-all", "g.safetensors", "tp4"]
    );
    let last = a.run_action("last_start", &Map::new()).await.unwrap();
    assert!(
        !last.ok && last.output.contains("holds the GPU"),
        "{}",
        last.output
    );
    assert!(codes(&a.status().await.unwrap()).contains(&"flare.last_start_failed".to_owned()));
}

#[test]
fn the_launcher_is_allowlisted_and_up_may_wait_longer() {
    assert!(ALLOWED_BINARIES.contains("flare-local"));
    let argv = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        max_timeout(&argv(&["flare-local", "up", "spark-all", "g", "tp4"])),
        FLARE_UP
    );
    assert_eq!(
        max_timeout(&argv(&["flare-local", "probe", "spark-all"])),
        MAX_TIMEOUT
    );
}

#[tokio::test]
async fn the_lane_files_are_read_only_and_only_parsed() {
    let e = Env::new();
    let a = e.make(runner(&all_stopped(&[], &[])), &[]);
    let files = a.config_files().unwrap();
    assert_eq!(
        files.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
        ["compose", "compose-tp1", "compose-tp2", "ignition"]
    );
    assert!(
        files
            .iter()
            .all(|f| !f.editable && f.why_readonly.as_deref().unwrap().contains("flare.sh"))
    );
    let cf = a.read_config("compose").await.unwrap();
    assert!(cf.content.starts_with("name: ai-lab-flare"));
    assert!(
        a.validate_config("ignition", "[paths]\ncuda = \"x\"\n")
            .await
            .unwrap()
            .ok
    );
    let bad = a.validate_config("compose", "a: [1, 2\n").await.unwrap();
    assert!(!bad.ok && !bad.authoritative);
    assert!(!a.post_apply("compose", true).await.unwrap().ok);
    assert!(a.read_config("nope").await.is_err());
    // a symlink out of the lane directory is not followed
    let outside = e.tmp.path().join("outside.yml");
    std::fs::write(&outside, "secret: 1\n").unwrap();
    std::fs::remove_file(e.dir().join("docker-compose.tp1.yml")).unwrap();
    std::os::unix::fs::symlink(&outside, e.dir().join("docker-compose.tp1.yml")).unwrap();
    assert!(a.read_config("compose-tp1").await.is_err());
}

#[test]
fn the_playground_talks_to_the_loopback_target_only() {
    let e = Env::new();
    let a = e.make(FakeRunner::new(), &[]);
    assert_eq!(
        a.playground_url().as_deref(),
        Some("http://127.0.0.1:8035/v1/chat/completions")
    );
    let spec = a.playground().unwrap().unwrap();
    assert_eq!(spec.wire, "openai_chat");
    assert!(spec.note.contains("ignores the model name"));
    let a = e.make(FakeRunner::new(), &[("flare", CLOSED)]);
    assert_eq!(
        a.playground_url().as_deref(),
        Some("http://127.0.0.1:9/v1/chat/completions")
    );
}

/// 0.10.0 audit F1: the launcher (ssh to every host) runs once at a time in the background; concurrent and later polls
/// get the last report at once instead of starting it again or waiting on it.
#[tokio::test]
async fn the_status_check_is_single_flight_and_never_blocks_a_poll_on_a_stale_report() {
    let e = Env::new();
    let r = FakeRunner::new();
    let first = all_stopped(&[], &[]);
    r.on_prefix(
        &[L, "status-json"],
        support::with(move |_| {
            std::thread::sleep(Duration::from_millis(200));
            ok(&first)
        }),
    );
    let a = e.make(Arc::clone(&r), &[]);
    // ten polls at once: one launcher run
    let polls: Vec<_> = (0..10)
        .map(|_| {
            let a = Arc::clone(&a);
            tokio::spawn(async move { a.status().await.unwrap() })
        })
        .collect();
    for p in polls {
        assert_eq!(p.await.unwrap().health, Health::Down);
    }
    assert_eq!(r.count(&[L, "status-json"]), 1);
    // a stale report is answered at once while one refresh runs behind it
    let t0 = std::time::Instant::now();
    a.status().await.unwrap();
    a.metrics().await.unwrap();
    assert!(
        t0.elapsed() < Duration::from_millis(150),
        "{:?}",
        t0.elapsed()
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(r.count(&[L, "status-json"]), 2);
}

#[tokio::test]
async fn an_unreachable_host_is_said_so_never_shown_as_absent() {
    let e = Env::new();
    let s = format!(
        "[{}]",
        tj(
            "spark-b",
            "sm121",
            "gb10",
            "unknown",
            "down",
            &[],
            r#","models":null,"metrics":null,"error":"unreachable: spark-b""#
        )
    );
    let st = e.make(runner(&s), &[]).status().await.unwrap();
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "flare.spark-b.unreachable")
        .unwrap();
    assert_eq!(p.severity, Severity::Warn);
    assert_eq!(
        st.components
            .iter()
            .find(|c| c.name == "flare @ spark-b")
            .unwrap()
            .state,
        "unknown"
    );
}
