//! Port of backend/tests/guardrails/test_guardrails_adapter.py. Fixtures are captured from the live lab deployment
//! (2026-10-24). Plus Rust-native tests of the "weakening the rails must be deliberate" security control (ADR 0015).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

#[path = "fixtures/guardrails/env.rs"]
mod env;
#[path = "fixtures/dcgm/support.rs"]
mod support;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use env::{CONTAINER, GrEnv, standard_runner};
use futures::StreamExt;
use nemo_console::adapters::base::{AdapterError, SystemAdapter};
use nemo_console::adapters::guardrails::{WEAKEN_MARKER, weaken_findings};
use nemo_console::domain::models::{Danger, Health, PostCheck, Severity};
use serde_json::{Map, Value, json};
use support::{FakeRunner, fixed, fx, mode, ok, rc, with};

const CFG: &str = "lab-input-output--config.yml";

fn live_config() -> String {
    fx("guardrails", "config-tree/lab-input-output/config.yml")
}

fn params(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

// ── identity / contract ─────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_identity_and_action_dangers() {
    let env = GrEnv::new().await;
    let (a, _) = env.make(None);
    assert_eq!(
        (a.id().as_str(), a.label().as_str()),
        ("guardrails", "NeMo Guardrails")
    );
    let dangers: HashMap<String, Danger> = a
        .actions()
        .unwrap()
        .into_iter()
        .map(|s| (s.id, s.danger))
        .collect();
    assert_eq!(
        dangers,
        HashMap::from([
            ("up".into(), Danger::Disruptive),
            ("restart".into(), Danger::Disruptive),
            ("stop".into(), Danger::Disruptive),
            ("check".into(), Danger::Disruptive)
        ])
    );
    assert_eq!(
        a.playground_url(),
        Some(format!("{}/v1/chat/completions", env.server.uri()))
    );
}

// ── status ──────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_status_up() {
    let env = GrEnv::new().await;
    let (a, runner) = env.make(None);
    let st = a.status().await.unwrap();
    assert!(
        st.system.as_str() == "guardrails"
            && st.health == Health::Up
            && st.version.as_deref() == Some("0.24.1")
    );
    assert_eq!(st.summary, "1 rails config(s) served");
    assert!(st.problems.is_empty(), "{:?}", st.problems);
    let names: HashMap<String, String> = st
        .components
        .iter()
        .map(|c| (c.name.clone(), c.state.clone()))
        .collect();
    assert!(names[CONTAINER] == "running" && names[&format!("{CONTAINER}-gw")] == "running");
    assert_eq!(names[&format!("{CONTAINER}-router-broker")], "running");
    assert!(
        !runner
            .called(&[
                "docker",
                "network",
                "inspect",
                "--format",
                "{{.Internal}}",
                "ai-lab-guardrails"
            ])
            .is_empty()
    );
}

#[tokio::test]
async fn test_status_off_when_container_absent() {
    let env = GrEnv::new().await;
    let r = FakeRunner::new();
    r.on_prefix(
        &["docker", "inspect"],
        fixed(rc(1, "", "Error: No such object")),
    );
    let (a, _) = env.make_with(Some(r), true);
    let st = a.status().await.unwrap();
    assert!(st.health == Health::Off && st.version.is_none());
    assert_eq!(
        st.problems
            .iter()
            .map(|p| p.code.as_str())
            .collect::<Vec<_>>(),
        ["guardrails.off"]
    );
    let fix = st.problems[0].fix.clone().unwrap();
    assert!(fix.contains("docker compose -f") && fix.contains("up -d --no-build"));
}

#[tokio::test]
async fn test_status_down_when_running_but_silent() {
    let env = GrEnv::new().await;
    let (a, _) = env.make_with(None, true);
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Down);
    assert!(st.problems.iter().any(|p| p.code == "guardrails.down"));
}

#[tokio::test]
async fn test_status_flags_open_egress_and_published_port() {
    let env = GrEnv::new().await;
    let r = standard_runner(
        Some(r#"ai-lab-guardrails ai-lab |{"8000/tcp":[{"HostIp":"0.0.0.0","HostPort":"8000"}]}"#),
        Some("false\n"),
    );
    let (a, _) = env.make(Some(r));
    let st = a.status().await.unwrap();
    let codes: Vec<&str> = st.problems.iter().map(|p| p.code.as_str()).collect();
    assert_eq!(st.health, Health::Degraded);
    assert!(
        codes
            .iter()
            .filter(|c| **c == "guardrails.egress.open")
            .count()
            == 2
            && codes.contains(&"guardrails.port.published")
    );
    let egress = st
        .problems
        .iter()
        .find(|p| p.code == "guardrails.egress.open")
        .unwrap();
    assert!(
        egress
            .fix
            .clone()
            .unwrap()
            .starts_with("docker network disconnect ")
    );
}

#[tokio::test]
async fn test_status_wrapper_missing_is_a_warning_with_fix() {
    let env = GrEnv::new().await;
    let r = standard_runner(None, None);
    r.on_prefix(
        &["guardrails-local", "--version"],
        fixed(rc(127, "", "not found")),
    );
    let (a, _) = env.make(Some(r));
    let st = a.status().await.unwrap();
    assert!(st.health == Health::Degraded && st.version.is_none());
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "guardrails.wrapper.missing")
        .unwrap();
    let fix = p.fix.clone().unwrap();
    assert!(
        fix.ends_with("   # builds the pinned image and links guardrails-local onto PATH")
            && fix.contains("install.sh")
    );
}

#[tokio::test]
async fn test_status_config_on_disk_not_served() {
    let env = GrEnv::new().await;
    env.configs.set(200, "[]");
    let (a, _) = env.make(None);
    let st = a.status().await.unwrap();
    let codes: Vec<&str> = st.problems.iter().map(|p| p.code.as_str()).collect();
    assert!(
        codes.contains(&"guardrails.configs.none")
            && codes.contains(&"guardrails.configs.not_served")
    );
}

#[tokio::test]
async fn test_version_is_cached() {
    let env = GrEnv::new().await;
    let (a, runner) = env.make(None);
    a.status().await.unwrap();
    a.status().await.unwrap();
    assert_eq!(runner.count(&["guardrails-local", "--version"]), 1);
}

// ── metrics ─────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_metrics_only_what_is_readable() {
    let env = GrEnv::new().await;
    let (a, _) = env.make(None);
    let m = a.metrics().await.unwrap();
    let got: HashMap<String, f64> = m.points.iter().map(|p| (p.key.clone(), p.value)).collect();
    assert!(
        m.available && got == HashMap::from([("gr.up".into(), 1.0), ("gr.configs".into(), 1.0)])
    );
}

#[tokio::test]
async fn test_metrics_unavailable_when_down() {
    let env = GrEnv::new().await;
    let (a, _) = env.make_with(None, true);
    let m = a.metrics().await.unwrap();
    assert!(!m.available && m.points.is_empty() && m.error.is_some());
}

// ── config files ────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_config_ids_match_the_core_id_rules() {
    let env = GrEnv::new().await;
    let (a, _) = env.make(None);
    let specs: HashMap<String, _> = a
        .config_files()
        .unwrap()
        .into_iter()
        .map(|s| (s.id.clone(), s))
        .collect();
    let mut ids: Vec<&String> = specs.keys().collect();
    ids.sort();
    assert_eq!(
        ids,
        [
            "lab-input-output--actions.py",
            CFG,
            "lab-input-output--kb--faq.md",
            "lab-input-output--prompts.yml"
        ]
    );
    let id_re = regex::Regex::new(r"\A[a-z0-9][a-z0-9_.\-]{0,63}\z").unwrap(); // api/deps.py ID_RE
    assert!(specs.keys().all(|i| id_re.is_match(i)));
    assert!(
        specs[CFG].editable && specs[CFG].format == "yaml" && !specs[CFG].validators.is_empty()
    );
    assert_eq!(specs["lab-input-output--kb--faq.md"].format, "text");
    let py = &specs["lab-input-output--actions.py"];
    assert!(!py.editable && py.why_readonly.clone().unwrap().contains("code"));
    assert_eq!(
        specs[CFG].display_path,
        "security/nemo-guardrails/config/lab-input-output/config.yml"
    );
}

#[tokio::test]
async fn test_symlinks_uppercase_hidden_and_foreign_dirs_are_not_offered() {
    let env = GrEnv::new().await;
    let (a, _) = env.make(None);
    let ids: Vec<String> = a
        .config_files()
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert!(!ids.iter().any(|i| i.contains("linked")
        || i.to_lowercase().contains("notes")
        || i.contains("hidden")
        || i.contains("no-config")));
    for bad in [
        "lab-input-output--linked.yml",
        "lab-input-output--Notes.md",
        "../x",
        "nope",
    ] {
        assert!(
            matches!(a.config_path(bad), Err(AdapterError::UnknownId(_))),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn test_read_config() {
    let env = GrEnv::new().await;
    let (a, _) = env.make(None);
    let cf = a.read_config(CFG).await.unwrap();
    assert_eq!(cf.content, live_config());
    assert!(cf.sha256.len() == 64 && cf.spec.id == CFG);
    assert!(matches!(
        a.read_config("nope--config.yml").await,
        Err(AdapterError::UnknownId(_))
    ));
}

// ── validation ──────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_validate_good_candidate_loads_a_temp_copy() {
    let env = GrEnv::new().await;
    let seen = Arc::new(std::sync::Mutex::new(HashMap::<&'static str, String>::new()));
    let s2 = Arc::clone(&seen);
    let r = standard_runner(None, None);
    r.on_prefix(
        &["guardrails-local", "validate"],
        with(move |argv| {
            let d = std::path::PathBuf::from(&argv[2]);
            let mut s = s2.lock().unwrap();
            s.insert("argv01", format!("{} {}", argv[0], argv[1]));
            s.insert(
                "config",
                std::fs::read_to_string(d.join("config.yml")).unwrap(),
            );
            s.insert("prompts", d.join("prompts.yml").exists().to_string());
            s.insert(
                "name",
                d.file_name().unwrap().to_string_lossy().into_owned(),
            );
            s.insert("mode", format!("{:o}", mode(d.parent().unwrap())));
            ok(&fx("guardrails", "validate_ok.stdout"))
        }),
    );
    let (a, _) = env.make(Some(r));
    let live = a.config_path(CFG).unwrap();
    let before = std::fs::read_to_string(&live).unwrap();
    let cand = before.replace("temperature: 0.2", "temperature: 0.7");
    let res = a.validate_config(CFG, &cand).await.unwrap();
    assert!(res.ok && res.authoritative && res.validator.starts_with("guardrails-local validate"));
    assert!(res.note.unwrap().contains("ok: loaded"));
    let s = seen.lock().unwrap();
    assert_eq!(s["argv01"], "guardrails-local validate");
    assert!(s["name"] == "lab-input-output" && s["config"] == cand && s["prompts"] == "true");
    assert_eq!(s["mode"], "700");
    assert_eq!(std::fs::read_to_string(&live).unwrap(), before); // the live file is never touched
    let leftovers = std::fs::read_dir(env.tmp().join("state").join("guardrails"))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("validate-"))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(leftovers, 0); // temp copy cleaned up
}

#[tokio::test]
async fn test_validate_broken_yaml_reports_line() {
    let env = GrEnv::new().await;
    let r = standard_runner(None, None);
    r.on_prefix(
        &["guardrails-local", "validate"],
        fixed(rc(1, "", &fx("guardrails", "validate_yaml.stderr"))),
    );
    let (a, _) = env.make(Some(r));
    let res = a
        .validate_config(CFG, "models: [\n  - type: main")
        .await
        .unwrap();
    assert!(!res.ok && res.authoritative);
    let errs: Vec<_> = res
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(!errs.is_empty() && errs[0].line.is_some() && errs[0].message.contains("YAML syntax"));
    assert_eq!(errs.len(), 1); // the container's ParserError is the same finding
}

#[tokio::test]
async fn test_validate_unknown_flow_is_an_authoritative_error() {
    let env = GrEnv::new().await;
    let r = standard_runner(None, None);
    r.on_prefix(
        &["guardrails-local", "validate"],
        fixed(rc(1, "", &fx("guardrails", "validate_flow.stderr"))),
    );
    let (a, _) = env.make(Some(r));
    let cand = format!(
        "# nemo-console: allow-weaken-rails\n{}",
        live_config().replace("self check output", "self check nonsense")
    );
    let res = a.validate_config(CFG, &cand).await.unwrap();
    assert!(!res.ok && res.authoritative);
    let m = &res.diagnostics[0].message;
    assert!(
        m.contains("self check nonsense")
            && m.contains("does not exist")
            && !m.contains("  at llmrails.py")
    );
}

#[tokio::test]
async fn test_validate_missing_prompt() {
    let env = GrEnv::new().await;
    let r = standard_runner(None, None);
    r.on_prefix(
        &["guardrails-local", "validate"],
        fixed(rc(1, "", &fx("guardrails", "validate_prompt.stderr"))),
    );
    let (a, _) = env.make(Some(r));
    let res = a
        .validate_config("lab-input-output--prompts.yml", "prompts: []\n")
        .await
        .unwrap();
    assert!(!res.ok && res.diagnostics[0].message.contains("self_check_input"));
}

#[tokio::test]
async fn test_validate_without_wrapper_is_not_authoritative() {
    let env = GrEnv::new().await;
    let r = standard_runner(None, None);
    r.on_prefix(
        &["guardrails-local", "validate"],
        fixed(rc(127, "", "no such file")),
    );
    let (a, _) = env.make(Some(r));
    let good = a
        .validate_config(CFG, &live_config().replace("0.2", "0.3"))
        .await
        .unwrap();
    assert!(good.ok && !good.authoritative && good.note.clone().unwrap().contains("install.sh"));
    let bad = a.validate_config(CFG, "a: [\n").await.unwrap();
    assert!(!bad.ok && !bad.authoritative);
}

#[tokio::test]
async fn test_validate_refuses_python_and_oversize() {
    let env = GrEnv::new().await;
    let (a, runner) = env.make(None);
    let py = a
        .validate_config("lab-input-output--actions.py", "print(1)")
        .await
        .unwrap();
    assert!(!py.ok);
    let big = a
        .validate_config(CFG, &format!("a: {}", "x".repeat(300_000)))
        .await
        .unwrap();
    assert!(!big.ok && big.diagnostics[0].message.contains("exceeds"));
    assert!(runner.called(&["guardrails-local", "validate"]).is_empty());
}

// ── apply ───────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_post_apply_without_restart_is_skipped() {
    let env = GrEnv::new().await;
    let (a, runner) = env.make(None);
    let r = a.post_apply(CFG, false).await.unwrap();
    assert!(r.ok && r.post_check == PostCheck::Skipped && !r.restarted);
    assert!(runner.called(&["docker", "restart"]).is_empty());
}

#[tokio::test]
async fn test_post_apply_restarts_and_reloads() {
    let env = GrEnv::new().await;
    let r = standard_runner(None, None);
    r.on_prefix(&["docker", "restart"], fixed(ok(CONTAINER)));
    let (a, _) = env.make(Some(Arc::clone(&r)));
    let res = a.post_apply(CFG, true).await.unwrap();
    assert!(
        res.ok
            && res.restarted
            && res.post_check == PostCheck::Healthy
            && res.detail.contains("lab-input-output")
    );
    assert_eq!(
        r.called(&["docker", "restart"]),
        vec![vec![
            "docker".to_owned(),
            "restart".into(),
            "--time".into(),
            "10".into(),
            CONTAINER.into()
        ]]
    );
    let live = a
        .config_path(CFG)
        .unwrap()
        .parent()
        .unwrap()
        .display()
        .to_string();
    assert_eq!(
        r.called(&["guardrails-local", "validate"]),
        vec![vec!["guardrails-local".to_owned(), "validate".into(), live]]
    );
}

#[tokio::test]
async fn test_post_apply_unhealthy_when_config_not_served() {
    let env = GrEnv::new().await;
    env.configs.set(200, "[]");
    let r = standard_runner(None, None);
    r.on_prefix(&["docker", "restart"], fixed(ok(CONTAINER)));
    let (a, _) = env.make(Some(r));
    let res = a.post_apply(CFG, true).await.unwrap();
    assert!(!res.ok && res.restarted && res.post_check == PostCheck::Unhealthy);
    assert!(res.detail.contains("not in the served list"));
}

#[tokio::test]
async fn test_post_apply_unhealthy_when_server_never_comes_back() {
    let env = GrEnv::new().await;
    let r = standard_runner(None, None);
    r.on_prefix(&["docker", "restart"], fixed(ok(CONTAINER)));
    let (a, _) = env.make_with(Some(r), true); // /v1/rails/configs refuses connections
    let res = a.post_apply(CFG, true).await.unwrap();
    assert!(!res.ok && res.post_check == PostCheck::Unhealthy);
}

#[tokio::test]
async fn test_post_apply_unhealthy_when_the_live_config_does_not_load() {
    let env = GrEnv::new().await;
    let r = standard_runner(None, None);
    r.on_prefix(&["docker", "restart"], fixed(ok(CONTAINER)));
    r.on_prefix(
        &["guardrails-local", "validate"],
        fixed(rc(1, "", &fx("guardrails", "validate_flow.stderr"))),
    );
    let (a, _) = env.make(Some(r));
    let res = a.post_apply(CFG, true).await.unwrap();
    assert!(
        !res.ok && res.post_check == PostCheck::Unhealthy && res.detail.contains("does not load")
    );
}

#[tokio::test]
async fn test_post_apply_restart_failure() {
    let env = GrEnv::new().await;
    let r = standard_runner(None, None);
    r.on_prefix(
        &["docker", "restart"],
        fixed(rc(1, "", "Error response from daemon")),
    );
    let (a, _) = env.make(Some(r));
    let res = a.post_apply(CFG, true).await.unwrap();
    assert!(!res.ok && !res.restarted && res.post_check == PostCheck::Unhealthy);
}

// ── actions ─────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_up_restart_stop_argv() {
    let env = GrEnv::new().await;
    let r = standard_runner(None, None);
    r.on_prefix(&["docker", "compose"], fixed(ok("done")));
    r.on_prefix(&["docker", "restart"], fixed(ok(CONTAINER)));
    let (a, _) = env.make(Some(Arc::clone(&r)));
    let compose = env
        .tmp()
        .join("repo/security/nemo-guardrails/docker-compose.yml")
        .display()
        .to_string();
    let up = a.run_action("up", &Map::new()).await.unwrap();
    assert!(up.ok && up.output.contains("served"));
    a.run_action("stop", &Map::new()).await.unwrap();
    let rs = a.run_action("restart", &Map::new()).await.unwrap();
    assert!(rs.ok);
    let v = |xs: &[&str]| xs.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        r.called(&["docker", "compose"]),
        vec![
            v(&[
                "docker",
                "compose",
                "-f",
                &compose,
                "up",
                "-d",
                "--no-build"
            ]),
            v(&["docker", "compose", "-f", &compose, "stop"])
        ]
    );
    assert_eq!(
        r.called(&["docker", "restart"]),
        vec![v(&["docker", "restart", "--time", "10", CONTAINER])]
    );
    let flat: Vec<String> = r.argvs().into_iter().flatten().collect();
    assert!(
        !flat.contains(&"down".to_owned())
            && !flat.contains(&"-v".to_owned())
            && !flat.contains(&"--volumes".to_owned())
    ); // never down -v
    let first = r
        .call_log()
        .into_iter()
        .find(|c| c.argv.len() >= 3 && c.argv[..3] == ["docker", "compose", "-f"])
        .unwrap();
    assert_eq!(
        first.cwd.unwrap(),
        std::path::Path::new(&compose).parent().unwrap()
    );
}

#[tokio::test]
async fn test_up_failure_points_at_install() {
    let env = GrEnv::new().await;
    let r = standard_runner(None, None);
    r.on_prefix(
        &["docker", "compose"],
        fixed(rc(1, "", "No such image: ai-lab-nemo-guardrails:0.24.1")),
    );
    let (a, _) = env.make(Some(r));
    let res = a.run_action("up", &Map::new()).await.unwrap();
    assert!(
        !res.ok
            && res.problems[0].code == "guardrails.up.failed"
            && res.problems[0].fix.clone().unwrap().contains("install.sh")
    );
}

#[tokio::test]
async fn test_unknown_action() {
    let env = GrEnv::new().await;
    let (a, _) = env.make(None);
    assert!(matches!(
        a.run_action("down", &Map::new()).await,
        Err(AdapterError::UnknownId(_))
    ));
}

#[tokio::test]
async fn test_check_all_as_expected() {
    let env = GrEnv::new().await;
    env.checks.verdicts(&[
        ("benign", "passed"),
        ("jailbreak", "blocked"),
        ("harmful", "blocked"),
    ]);
    let (a, _) = env.make(None);
    let res = a.run_action("check", &Map::new()).await.unwrap();
    assert!(res.ok && res.problems.is_empty(), "{}", res.output);
    let bodies = env.checks.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 3);
    assert_eq!(
        bodies[0],
        json!({"model": "aitop/nemotron-lightning", "messages": [{"role": "user", "content": "What is the capital of France?"}],
               "guardrails": {"config_id": "lab-input-output"}})
    );
    assert!(
        res.output.contains("benign     allowed") && res.output.contains("jailbreak  blocked"),
        "{}",
        res.output
    );
    let ev = a.recent_events(100).await.unwrap().remove(0);
    assert!(
        ev.kind == "guardrails.check"
            && ev.system.as_str() == "guardrails"
            && ev.data["ok"] == true
    );
    assert!(
        !serde_json::to_string(&ev)
            .unwrap()
            .contains("What is the capital")
    ); // metadata only
    assert_eq!(
        mode(
            &env.tmp()
                .join("state")
                .join("guardrails")
                .join("checks.jsonl")
        ),
        0o600
    );
}

#[tokio::test]
async fn test_check_reports_a_jailbreak_that_got_through() {
    let env = GrEnv::new().await;
    env.checks.verdicts(&[
        ("benign", "passed"),
        ("jailbreak", "passed"),
        ("harmful", "blocked"),
    ]);
    let (a, _) = env.make(None);
    let res = a.run_action("check", &Map::new()).await.unwrap();
    assert!(!res.ok);
    assert_eq!(
        res.problems
            .iter()
            .map(|p| p.code.as_str())
            .collect::<Vec<_>>(),
        ["guardrails.check.unexpected"]
    );
    assert!(
        res.problems[0].message.contains("jailbreak")
            && res.problems[0].fix.clone().unwrap().contains("prompts.yml")
    );
}

#[tokio::test]
async fn test_check_stops_on_server_error() {
    let env = GrEnv::new().await;
    *env.checks.fixed.lock().unwrap() = Some((400, fx("guardrails", "chat-badconfig.json")));
    let (a, _) = env.make(None);
    let res = a
        .run_action("check", &params(json!({"config": "lab-input-output"})))
        .await
        .unwrap();
    assert!(!res.ok && res.problems[0].code == "guardrails.check.failed");
    assert_eq!(env.post_count().await, 1);
}

#[tokio::test]
async fn test_check_param_choice_is_validated_against_disk() {
    let env = GrEnv::new().await;
    env.checks.verdicts(&[
        ("benign", "passed"),
        ("jailbreak", "blocked"),
        ("harmful", "blocked"),
    ]);
    let (a, _) = env.make(None);
    let spec = a
        .actions()
        .unwrap()
        .into_iter()
        .find(|s| s.id == "check")
        .unwrap();
    assert!(
        spec.params[0].choices == Some(vec!["lab-input-output".into()])
            && spec.params[0].default == json!("lab-input-output")
    );
    a.run_action("check", &params(json!({"config": "../../etc"})))
        .await
        .unwrap(); // unknown -> the default config
    assert_eq!(
        env.checks.bodies.lock().unwrap()[0]["guardrails"],
        json!({"config_id": "lab-input-output"})
    );
}

// ── events ──────────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_events_empty_then_followed() {
    let env = GrEnv::new().await;
    let (a, _) = env.make(None);
    assert!(a.recent_events(100).await.unwrap().is_empty());
    env.checks.verdicts(&[
        ("benign", "passed"),
        ("jailbreak", "blocked"),
        ("harmful", "blocked"),
    ]);
    let mut it = Arc::clone(&a).follow_events(Some(0.0));
    a.run_action("check", &Map::new()).await.unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(3), it.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(ev.kind, "guardrails.check");
}

// ── playground ──────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn test_playground_spec() {
    let env = GrEnv::new().await;
    let (a, _) = env.make(None);
    let pg = a.playground().unwrap().unwrap();
    assert!(
        pg.wire == "openai_chat"
            && pg.models == ["aitop/nemotron-lightning"]
            && pg.default_model == "aitop/nemotron-lightning"
    );
    assert!(pg.note.contains("lab-input-output"));
}

#[test]
fn test_playground_body_shape_matches_the_captured_response() {
    // The core posts {model, messages, stream, max_tokens}; the real server answered such a body (fixtures).
    let body: Value = serde_json::from_str(&fx("guardrails", "chat-benign.json")).unwrap();
    assert!(
        body["choices"][0]["message"]["content"] == "Paris"
            && body["model"] == "aitop/nemotron-lightning"
    );
    let blocked: Value = serde_json::from_str(&fx("guardrails", "chat-jailbreak.json")).unwrap();
    assert_eq!(
        blocked["choices"][0]["message"]["content"],
        "I'm sorry, I can't respond to that."
    );
}

// ── M3: weakening the rails must be deliberate ──────────────────────────────────────────
fn first_error(res: &nemo_console::domain::models::ValidationResult) -> String {
    res.diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error)
        .unwrap()
        .message
        .clone()
}

#[tokio::test]
async fn test_removing_an_output_flow_is_refused() {
    let env = GrEnv::new().await;
    let (a, _) = env.make(None);
    let res = a
        .validate_config(
            CFG,
            &live_config().replace("      - self check output\n", ""),
        )
        .await
        .unwrap();
    assert!(!res.ok);
    let msg = first_error(&res);
    assert!(msg.contains("output: self check output") && msg.contains(WEAKEN_MARKER));
}

#[tokio::test]
async fn test_removing_the_whole_rails_section_is_refused() {
    let env = GrEnv::new().await;
    let (a, _) = env.make(None);
    let live = live_config();
    let cand = &live[..live.find("rails:").unwrap()];
    let res = a.validate_config(CFG, cand).await.unwrap();
    assert!(!res.ok);
    let msg = first_error(&res);
    assert!(msg.contains("input: self check input") && msg.contains("output: self check output"));
}

#[tokio::test]
async fn test_the_marker_line_allows_the_weakening() {
    let env = GrEnv::new().await;
    let (a, _) = env.make(None);
    let cand = format!(
        "{WEAKEN_MARKER}\n{}",
        live_config().replace("      - self check output\n", "")
    );
    let res = a.validate_config(CFG, &cand).await.unwrap();
    assert!(res.ok && res.authoritative);
    let sloppy = format!(
        "{}# not {WEAKEN_MARKER} really\n",
        live_config().replace("      - self check output\n", "")
    );
    assert!(!a.validate_config(CFG, &sloppy).await.unwrap().ok); // only the exact line counts
}

#[tokio::test]
async fn test_edits_that_keep_or_add_rails_need_no_marker() {
    let env = GrEnv::new().await;
    let (a, _) = env.make(None);
    let live = live_config();
    assert!(
        a.validate_config(CFG, &live.replace("temperature: 0.2", "temperature: 0.9"))
            .await
            .unwrap()
            .ok
    );
    let more = live.replace(
        "      - self check output\n",
        "      - self check output\n      - extra rail\n",
    );
    assert!(a.validate_config(CFG, &more).await.unwrap().ok);
}

#[tokio::test]
async fn test_weakening_is_refused_even_without_the_wrapper() {
    let env = GrEnv::new().await;
    let r = standard_runner(None, None);
    r.on_prefix(
        &["guardrails-local", "validate"],
        fixed(rc(127, "", "no such file")),
    );
    let (a, _) = env.make(Some(r));
    let res = a
        .validate_config(
            CFG,
            &live_config().replace("      - self check input\n", ""),
        )
        .await
        .unwrap();
    assert!(!res.ok && !res.authoritative);
}

#[tokio::test]
async fn test_spec_description_names_the_marker() {
    let env = GrEnv::new().await;
    let (a, _) = env.make(None);
    let specs: HashMap<String, _> = a
        .config_files()
        .unwrap()
        .into_iter()
        .map(|s| (s.id.clone(), s))
        .collect();
    assert!(
        specs[CFG].description.contains(WEAKEN_MARKER)
            && !specs["lab-input-output--prompts.yml"]
                .description
                .contains(WEAKEN_MARKER)
    );
}

// ── L1: the validation copy never follows symlinks and is capped ────────────────────────
#[tokio::test]
async fn test_validation_copy_skips_symlinks_and_caps() {
    let env = GrEnv::new().await;
    let seen = Arc::new(std::sync::Mutex::new((
        Vec::<String>::new(),
        Vec::<String>::new(),
    )));
    let s2 = Arc::clone(&seen);
    let r = standard_runner(None, None);
    r.on_prefix(
        &["guardrails-local", "validate"],
        with(move |argv| {
            let d = std::path::PathBuf::from(&argv[2]);
            let (mut files, mut links) = (Vec::new(), Vec::new());
            walk(&d, &d, &mut files, &mut links);
            *s2.lock().unwrap() = (files, links);
            ok(&fx("guardrails", "validate_ok.stdout"))
        }),
    );
    let (a, _) = env.make(Some(r));
    let res = a.validate_config(CFG, &live_config()).await.unwrap();
    let (files, links) = seen.lock().unwrap().clone();
    assert!(res.ok && links.is_empty() && !files.contains(&"linked.yml".to_owned()));
    for f in ["config.yml", "prompts.yml", "kb/faq.md"] {
        assert!(files.contains(&f.to_owned()), "{files:?}");
    }
    let big = a
        .config_path(CFG)
        .unwrap()
        .parent()
        .unwrap()
        .join("big.txt");
    std::fs::write(&big, vec![b'x'; 3 * 1024 * 1024]).unwrap();
    let capped = a.validate_config(CFG, &live_config()).await.unwrap();
    assert!(!capped.ok && capped.diagnostics[0].message.contains("exceeds"));
}

fn walk(
    root: &std::path::Path,
    d: &std::path::Path,
    files: &mut Vec<String>,
    links: &mut Vec<String>,
) {
    for e in std::fs::read_dir(d).unwrap().flatten() {
        let p = e.path();
        if p.is_symlink() {
            links.push(e.file_name().to_string_lossy().into_owned());
        } else if p.is_dir() {
            walk(root, &p, files, links);
        } else {
            files.push(p.strip_prefix(root).unwrap().display().to_string());
        }
    }
}

#[tokio::test]
async fn test_isolation_is_checked_right_after_boot() {
    // CI 2026-10-24: a cache stamped 0.0 looked fresh shortly after boot. Rust: a never-filled cache is None (stale at
    // any clock value), so the first status asks docker.
    let env = GrEnv::new().await;
    let (a, runner) = env.make(None);
    a.status().await.unwrap();
    assert!(
        !runner
            .called(&[
                "docker",
                "network",
                "inspect",
                "--format",
                "{{.Internal}}",
                "ai-lab-guardrails"
            ])
            .is_empty()
    );
}

// ── Rust-native: the weakening control (ADR 0015) on its own ───────────────────────────
#[test]
fn security_weakening_needs_the_exact_marker_line() {
    let tmp = tempfile::tempdir().unwrap();
    let live = tmp.path().join("config.yml");
    std::fs::write(&live, live_config()).unwrap();
    let weakened = live_config().replace("      - self check output\n", "");
    // refused without the marker, with a near-miss marker, or with the marker inside other text
    for cand in [
        weakened.clone(),
        format!("#nemo-console: allow-weaken-rails\n{weakened}"),
        format!("{weakened}x: 1 {WEAKEN_MARKER}\n"),
        format!("{weakened}# nemo-console: allow-weaken-rails please\n"),
    ] {
        let d = weaken_findings(&live, &cand).unwrap();
        assert!(d.len() == 1 && d[0].severity == Severity::Error, "{cand:?}");
    }
    // allowed with the exact line, surrounding whitespace ignored (Python strips each line)
    for cand in [
        format!("{WEAKEN_MARKER}\n{weakened}"),
        format!("{weakened}   {WEAKEN_MARKER}  \n"),
    ] {
        assert!(
            weaken_findings(&live, &cand).unwrap().is_empty(),
            "{cand:?}"
        );
    }
    // renaming a flow is a removal too; adding one is not
    assert_eq!(
        weaken_findings(
            &live,
            &live_config().replace("self check input", "self check inputs")
        )
        .unwrap()
        .len(),
        1
    );
    assert!(
        weaken_findings(
            &live,
            &live_config().replace(
                "      - self check output\n",
                "      - self check output\n      - more\n"
            )
        )
        .unwrap()
        .is_empty()
    );
    // moving the flows away from rails.*.flows (the server would not apply them) is a removal
    let moved = live_config().replace("rails:\n", "unused:\n");
    assert_eq!(weaken_findings(&live, &moved).unwrap().len(), 1);
}
