//! The Model Optimizer adapter: status (launcher, recipes, the last job), metrics (the mo.* keys), the recipes
//! schema, the actions (run a job, show a job) and the events. Offline: a fake wrapper, no GPU, no clock.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

#[path = "fixtures/modelopt/env.rs"]
mod env;
#[path = "fixtures/dcgm/support.rs"]
mod support;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use env::{LAUNCHER, MoEnv, Opts};
use nemo_console::adapters::base::SystemAdapter;
use nemo_console::adapters::modelopt::{parse_recipes, render_argv};
use nemo_console::domain::models::{
    Danger, Health, MetricsSnapshot, ParamType, Severity, SystemStatus,
};
use nemo_console::services::runner::{MODELOPT_BINARIES, max_timeout};
use serde_json::{Map, json};
use support::fx;

fn codes(st: &SystemStatus) -> HashSet<String> {
    st.problems.iter().map(|p| p.code.clone()).collect()
}

fn pts(snap: &MetricsSnapshot, key: &str) -> Vec<(HashMap<String, String>, f64)> {
    snap.points
        .iter()
        .filter(|p| p.key == key)
        .map(|p| {
            let l: HashMap<String, String> = p
                .labels
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            (l, p.value)
        })
        .collect()
}

#[tokio::test]
async fn modelopt_status_and_metrics() {
    let e = MoEnv::new().await;
    e.seed_jobs(2);
    let (a, _r) = e.make(None, &Opts::default());
    let st = a.status().await.unwrap();
    assert_eq!(st.system, nemo_console::domain::models::SystemId::Modelopt);
    assert_eq!(st.health, Health::Up);
    assert!(st.summary.contains("recipe"));
    // the launcher is a component and the problems are empty
    assert!(st.components.iter().any(|c| c.name == LAUNCHER));
    assert!(codes(&st).is_empty());
    let m = a.metrics().await.unwrap();
    assert!(m.available);
    let keys: HashSet<String> = m.points.iter().map(|p| p.key.clone()).collect();
    for k in [
        "mo.jobs",
        "mo.jobs_by_verdict",
        "mo.live_running",
        "mo.output_bytes",
        "mo.compression_ratio",
        "mo.duration_s",
    ] {
        assert!(keys.contains(k), "the mo.* key set");
    }
    assert_eq!(m.points.iter().filter(|p| p.key == "mo.jobs").count(), 1);
    // the per-verdict numbers carry the verdict as a label
    let verdicts = pts(&m, "mo.jobs_by_verdict");
    // one point per verdict, each carrying the verdict as its label
    assert_eq!(verdicts.len(), 4);
    assert!(
        verdicts
            .iter()
            .all(|(l, _)| l.len() == 1 && l.keys().all(|k| k == "verdict"))
    );
    assert!(verdicts.iter().any(|(l, v)| {
        l.iter()
            .any(|(k, val)| k == "verdict" && val == "succeeded")
            && *v == 2.0
    }));
    // the finished job's numbers, in their own units
    let out = m
        .points
        .iter()
        .find(|p| p.key == "mo.output_bytes")
        .unwrap();
    assert_eq!(out.unit, "B");
    assert!(out.value > 0.0);
    let ratio = m
        .points
        .iter()
        .find(|p| p.key == "mo.compression_ratio")
        .unwrap();
    assert_eq!(ratio.unit, "ratio");
    assert!(ratio.value > 0.0 && ratio.value < 1.0);
    let dur = m.points.iter().find(|p| p.key == "mo.duration_s").unwrap();
    assert_eq!(dur.unit, "s");
}

#[tokio::test]
async fn modelopt_without_the_wrapper_is_down_with_a_fix() {
    let e = MoEnv::new().await;
    let (a, _r) = e.make(
        None,
        &Opts {
            launcher: false,
            ..Opts::default()
        },
    );
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Down);
    let c = codes(&st);
    assert!(c.contains("modelopt.not_installed"));
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "modelopt.not_installed")
        .unwrap();
    assert_eq!(p.severity, Severity::Error);
    assert!(p.fix.is_some());
    // without a launcher there are no jobs either
    let m = a.metrics().await.unwrap();
    assert_eq!(m.points.len(), 1);
    assert_eq!(m.points[0].key, "mo.jobs");
    assert_eq!(m.points[0].value, 0.0);
}

#[tokio::test]
async fn modelopt_broken_recipes_are_a_warning_with_a_line() {
    let e = MoEnv::new().await;
    let bad = "version: 1\nrecipes:\n  reap-x:\n    method: nope\n    base_model: GLM\n";
    let (a, _r) = e.make(
        None,
        &Opts {
            recipes: Some(bad.to_owned()),
            ..Opts::default()
        },
    );
    let st = a.status().await.unwrap();
    assert_eq!(st.health, Health::Degraded);
    let c = codes(&st);
    assert!(c.contains("modelopt.recipes_invalid"));
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "modelopt.recipes_invalid")
        .unwrap();
    assert!(p.message.contains("method"));
    assert!(p.fix.is_some());
}

#[tokio::test]
async fn modelopt_recipes_schema() {
    let good = fx("modelopt", "recipes.yaml");
    let (r, d) = parse_recipes(&good).unwrap();
    assert_eq!(r.len(), 2);
    assert!(d.iter().all(|x| x.severity != Severity::Error));
    // a recipe without a method is dropped and reported with its line
    let (r2, d2) = parse_recipes("version: 1\nrecipes:\n  a:\n    base_model: GLM\n").unwrap();
    assert_eq!(r2.len(), 0);
    let e = d2.iter().find(|x| x.severity == Severity::Error).unwrap();
    assert!(e.message.contains("method"));
    // a key that exists in the file gets its line number
    let (_, d4) =
        parse_recipes("version: 1\nrecipes:\n  a:\n    method: reap\n    banana: 1\n").unwrap();
    let u = d4.iter().find(|x| x.message.contains("banana")).unwrap();
    assert!(u.line.is_some());
    // 'version' must be 1
    let (_, d3) = parse_recipes("version: 2\nrecipes:\n  a:\n    method: reap\n").unwrap();
    assert!(d3.iter().any(|x| x.message.contains("version")));
}

#[tokio::test]
async fn modelopt_actions_and_the_argv_the_console_sends() {
    let e = MoEnv::new().await;
    let (a, _r) = e.make(None, &Opts::default());
    let acts = a.actions().unwrap();
    assert_eq!(acts.len(), 2);
    let run = acts.iter().find(|x| x.id == "run").unwrap();
    assert_eq!(run.danger, Danger::Disruptive);
    let report = acts.iter().find(|x| x.id == "report").unwrap();
    assert_eq!(report.danger, Danger::Safe);
    // the recipe parameter is an enum over the recipes file
    let p = run.params.iter().find(|x| x.key == "recipe").unwrap();
    assert_eq!(p.type_, ParamType::Enum);
    assert_eq!(p.choices.as_ref().map(|c| c.len()).unwrap_or(0), 2);
    // the argv: argv only, the job id, the recipe and the console's own run dir
    let argv = render_argv(
        "20261028T090000-123-reap-glm53",
        "reap-glm53",
        &e.state_dir().join("modelopt").join("jobs").join("x"),
    );
    assert_eq!(argv[0], LAUNCHER);
    assert_eq!(argv[1], "run");
    assert!(argv.iter().any(|x| x == "--job-id"));
    assert!(argv.iter().any(|x| x == "--recipe"));
    assert!(argv.iter().any(|x| x == "--run-dir"));
    assert_eq!(argv.len(), 8);
    // the runner allowlist knows the wrapper, and the timeout is the standard one
    assert_eq!(MODELOPT_BINARIES[0].to_string(), argv[0]);
    assert_eq!(max_timeout(&argv), 600.0);
}

#[tokio::test]
async fn modelopt_run_starts_a_job_and_report_reads_it() {
    let e = MoEnv::new().await;
    let (a, r) = e.make(None, &Opts::default());
    let mut params = Map::new();
    params.insert("recipe".to_owned(), json!("reap-glm53"));
    let res = a.run_action("run", &params).await;
    let (ok, out) = match res {
        Ok(r) => (r.ok, r.output),
        Err(_) => (false, String::new()),
    };
    assert!(ok);
    assert!(out.contains("Started compression job"));
    // the wrapper really ran, with the console's argv
    let calls = r.called(&[LAUNCHER, "run"]);
    assert_eq!(calls.len(), 1);
    assert!(calls[0].iter().any(|x| x == "--recipe"));
    // the job dir the console owns holds the run.json the wrapper wrote
    let jobs = e.state_dir().join("modelopt").join("jobs");
    let dirs: Vec<PathBuf> = std::fs::read_dir(jobs)
        .unwrap()
        .flatten()
        .map(|x| x.path())
        .collect();
    assert_eq!(dirs.len(), 1);
    let meta = std::fs::read_to_string(dirs[0].join("run.json")).unwrap();
    assert!(meta.contains("\"verdict\":\"running\""));
    // the report reads the same job back
    let rep = a.run_action("report", &Map::new()).await.unwrap();
    assert!(rep.ok);
    assert!(rep.output.contains("Running"));
    // an unknown recipe is refused, not run
    let mut p = Map::new();
    p.insert("recipe".to_owned(), json!("nope"));
    let bad = a.run_action("run", &p).await.unwrap();
    assert!(!bad.ok);
    assert!(bad.output.contains("unknown or invalid recipe"));
}

#[tokio::test]
async fn modelopt_events_carry_the_job_id() {
    let e = MoEnv::new().await;
    e.seed_jobs(1);
    let (a, _r) = e.make(None, &Opts::default());
    let ev = a.recent_events(10).await.unwrap();
    assert_eq!(ev.len(), 1);
    assert_eq!(
        ev[0].system,
        nemo_console::domain::models::SystemId::Modelopt
    );
    assert_eq!(ev[0].kind, "modelopt.job");
    assert!(ev[0].data.get("job_id").is_some());
    assert!(ev[0].summary.contains("reap-glm53"));
}
