//! Port of backend/tests/dynamo/test_compose_files.py (addendum 9.6): the compose files IN USE come from the
//! containers' labels (the lab's Super VL variant runs from docker-compose.yml + docker-compose.super-vl.yml), the
//! label is untrusted input, Params and the console override are refused beside a repo override file, the version is a
//! Dynamo tag (never the gateway's digest), the real frontend behind the gateway is a component and a log source, and
//! the worker's GPUs come from the merged config.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value,
    clippy::cloned_ref_to_slice_refs
)]

mod dynamo_support;
mod switchyard_support;

use std::path::{Path, PathBuf};

use dynamo_support::*;
use nemo_console::adapters::base::{CommandResult, SystemAdapter};
use nemo_console::adapters::dynamo::{
    CardPolicy, ComposeFiles, image_version, merged_worker_uuids,
};
use nemo_console::api::pyjson;
use nemo_console::domain::models::{Health, PostCheck, Severity};
use serde_json::{Map, Value, json};
use switchyard_support::{FakeRunner, ok};

const CARD_A: &str = "GPU-00000000-0000-0000-0000-000000000001";
const CARD_B: &str = "GPU-00000000-0000-0000-0000-000000000002";
const SV_IMAGE: &str = "nvcr.io/nvidia/ai-dynamo/vllm-runtime:1.6.0-nemotron-3.5-super-vl-dev.1@sha256:3a55cf2d16db5ffc026326a70bf0c56ac09661fdca5508404520382a49403e4d";
const NGINX: &str =
    "nginx:1.31.6-alpine@sha256:df221db836e1754089190208cee7eeda94f233197056426eda74a43ab1abeac2";
const CHATTERBOX: &str = "ai-lab-chatterbox-tts-tts-1";
const BOLT: &str = "ai-lab-vllm-nemotron-bolt-nemotron-bolt-1";

fn none() -> Map<String, Value> {
    Map::new()
}

fn canon(p: &Path) -> String {
    std::fs::canonicalize(p).unwrap().display().to_string()
}

fn sv_override(compose: &Path) -> PathBuf {
    let p = compose
        .parent()
        .unwrap()
        .join("docker-compose.super-vl.yml");
    std::fs::write(&p, "services:\n  dynamo-worker:\n    restart: \"no\"\n").unwrap();
    p
}

enum Files<'a> {
    List(&'a [PathBuf]),
    Raw(&'a str),
}

/// A Dynamo container shaped like the live inspect of 2026-10-17 (labels carry BOTH files, comma-separated).
fn labelled(
    mut d: Value,
    files: Files<'_>,
    workdir: &str,
    name: Option<&str>,
    service: Option<&str>,
    image: Option<&str>,
) -> Value {
    let raw = match files {
        Files::List(fs) => fs
            .iter()
            .map(|f| f.display().to_string())
            .collect::<Vec<_>>()
            .join(","),
        Files::Raw(s) => s.to_owned(),
    };
    let lb = &mut d["Config"]["Labels"];
    lb["com.docker.compose.project.config_files"] = Value::from(raw);
    lb["com.docker.compose.project.working_dir"] = Value::from(workdir);
    if let Some(n) = name {
        d["Name"] = Value::from(format!("/{n}"));
    }
    if let Some(sv) = service {
        d["Config"]["Labels"]["com.docker.compose.service"] = Value::from(sv);
    }
    if let Some(i) = image {
        d["Config"]["Image"] = Value::from(i);
    }
    d
}

fn dir(compose: &Path) -> String {
    compose.parent().unwrap().display().to_string()
}

/// The Super VL stack: nginx at dynamo-frontend (no GPU), the real frontend, the worker on BOTH cards.
fn sv_stack(compose: &Path) -> Vec<Value> {
    let files = vec![compose.to_path_buf(), sv_override(compose)];
    let d = dir(compose);
    let fe = labelled(
        frontend_doc(),
        Files::List(&files),
        &d,
        None,
        None,
        Some(NGINX),
    );
    let sv = labelled(
        frontend_doc(),
        Files::List(&files),
        &d,
        Some(SV),
        Some("dynamo-sv-frontend"),
        Some(SV_IMAGE),
    );
    let mut wk = labelled(
        worker_doc(),
        Files::List(&files),
        &d,
        None,
        None,
        Some(SV_IMAGE),
    );
    wk["HostConfig"]["DeviceRequests"][0]["DeviceIDs"] = json!([CARD_B, CARD_A]);
    vec![fe, sv, wk]
}

fn is_merged_probe(a: &[String]) -> bool {
    a.len() >= 2
        && a[0] == "docker"
        && a[1] == "compose"
        && a[a.len() - 3..] == ["config", "--format", "json"]
}

fn wire_docs(runner: &FakeRunner, docs: Vec<Value>, ps: &str, merged: Option<String>) {
    match merged {
        Some(m) => runner.on(is_merged_probe, ok(&m)),
        None => runner.on(
            is_merged_probe,
            CommandResult::new(1, "", "no merged config"),
        ),
    }
    wire(runner, docs, ps);
    runner.on(is_docker("compose"), ok("done"));
}

fn merged() -> Option<String> {
    Some(fx("merged_config_super_vl.json"))
}

// ── lifecycle with the files in use ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_super_vl_stack_lifecycle_uses_exactly_the_label_files() {
    let f = Fix::new().await;
    let docs = sv_stack(&f.compose);
    let ov = f
        .compose
        .parent()
        .unwrap()
        .join("docker-compose.super-vl.yml");
    wire_docs(&f.runner, docs, "", merged());
    let ad = f.make();
    ad.write_override("services:\n  dynamo-worker:\n    command: [python3, -m, dynamo.vllm]\n")
        .unwrap(); // active console override
    http_live(&f.mock, true);
    for action in ["down", "restart", "up"] {
        f.runner.clear_calls();
        let res = ad.run_action(action, &none()).await.unwrap();
        let calls = compose_calls(&f.runner);
        assert_eq!(calls.len(), 1, "{action}");
        let call = &calls[0];
        assert_eq!(files_of(call), [canon(&f.compose), canon(&ov)], "{action}"); // never the console override
        let i = call
            .iter()
            .position(|x| x == "--project-directory")
            .unwrap();
        assert_eq!(call[i + 1], dir(&f.compose));
        if action == "down" {
            assert!(
                res.ok && call.last().unwrap() == "down" && call.iter().any(|x| x == "--profile")
            );
            assert!(!call.iter().any(|x| x == "-v"));
        } else {
            assert_eq!(
                call[call.len() - 2..],
                s(&["dynamo-frontend", "dynamo-worker"])
            );
            assert_eq!(
                call.iter().any(|x| x == "--force-recreate"),
                action == "restart"
            );
        }
    }
    // with no Dynamo container at all, the configured base applies (plus the console override, as before)
    f.runner.clear_rules();
    wire_docs(&f.runner, vec![], "", None);
    f.runner.clear_calls();
    ad.run_action("down", &none()).await.unwrap();
    let call = compose_calls(&f.runner)[0].clone();
    assert_eq!(
        files_of(&call),
        [canon(&f.compose), ad.override_path().display().to_string()]
    );
}

#[tokio::test]
async fn test_console_override_in_the_label_is_the_base_stack() {
    let f = Fix::new().await;
    let ad = f.make();
    let own = ad.override_path();
    std::fs::create_dir_all(own.parent().unwrap()).unwrap();
    std::fs::write(
        &own,
        "services:\n  dynamo-worker:\n    command: [python3, -m, dynamo.vllm]\n",
    )
    .unwrap();
    let wk = labelled(
        worker_doc(),
        Files::List(&[f.compose.clone(), own.clone()]),
        &dir(&f.compose),
        None,
        None,
        None,
    );
    wire_docs(&f.runner, vec![wk], "", None);
    let cf = ad.compose_files(None).await.unwrap();
    assert_eq!(cf.files, [std::fs::canonicalize(&f.compose).unwrap()]);
    assert!(cf.overrides().is_empty() && cf.error.is_none() && cf.from_label);
    ad.run_action("down", &none()).await.unwrap();
    assert_eq!(
        files_of(&compose_calls(&f.runner)[0]),
        [canon(&f.compose), own.display().to_string()]
    );
}

#[tokio::test]
async fn test_untrusted_labels_refuse_every_lifecycle_action() {
    let f = Fix::new().await;
    let base_dir = f.compose.parent().unwrap().to_path_buf();
    let outside = f.tmp.path().join("elsewhere");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("docker-compose.evil.yml"), "services: {}\n").unwrap();
    std::os::unix::fs::symlink(
        outside.join("docker-compose.evil.yml"),
        base_dir.join("escape.yml"),
    )
    .unwrap();
    std::fs::write(base_dir.join("notes.txt"), "x\n").unwrap();
    std::fs::create_dir(base_dir.join("sub")).unwrap();
    std::fs::write(base_dir.join("sub/docker-compose.yml"), "services: {}\n").unwrap();
    std::fs::write(base_dir.join("stray.yml"), "services: {}\n").unwrap();
    let c = f.compose.clone();
    let nine = vec![c.display().to_string(); 9].join(",");
    let bd = base_dir.display().to_string();
    let od = outside.display().to_string();
    let lists: Vec<(Vec<PathBuf>, &str, &str)> = vec![
        (
            vec![c.clone(), base_dir.join("sub/docker-compose.yml")],
            &bd,
            "not a compose file inside",
        ), // deeper
        (
            vec![c.clone(), base_dir.join("stray.yml")],
            &bd,
            "not a compose file inside",
        ), // not a compose name
        (
            vec![c.clone(), outside.join("docker-compose.evil.yml")],
            &bd,
            "not a compose file inside",
        ),
        (
            vec![c.clone(), base_dir.join("escape.yml")],
            &bd,
            "not a compose file inside",
        ), // symlink out
        (
            vec![c.clone(), base_dir.join("gone.yml")],
            &bd,
            "does not exist",
        ),
        (
            vec![c.clone(), base_dir.join("notes.txt")],
            &bd,
            "not a compose file inside",
        ),
        (vec![c.clone()], &od, "project directory"),
    ];
    let raws: Vec<(&str, &str, &str)> = vec![
        ("docker-compose.yml", &bd, "not an absolute path"),
        (&nine, &bd, "more than 8"),
    ];
    let mut all: Vec<(Value, String)> = lists
        .iter()
        .map(|(fs, wd, why)| {
            (
                labelled(worker_doc(), Files::List(fs), wd, None, None, None),
                (*why).to_owned(),
            )
        })
        .collect();
    all.extend(raws.iter().map(|(r, wd, why)| {
        (
            labelled(worker_doc(), Files::Raw(r), wd, None, None, None),
            (*why).to_owned(),
        )
    }));
    for (doc, why) in all {
        f.runner.clear_rules();
        f.runner.clear_calls();
        wire_docs(&f.runner, vec![doc], "", None);
        let ad = f.make();
        for action in ["up", "down", "restart", "up_mock"] {
            let res = ad.run_action(action, &none()).await.unwrap();
            assert!(
                !res.ok && res.problems[0].code == "dynamo.compose.untrusted",
                "{why} {action}"
            );
            assert!(res.output.contains(&why), "{why}: {}", res.output);
        }
        assert!(compose_calls(&f.runner).is_empty()); // nothing ran, no fallback to base
        let p = serde_json::json!({"max_num_seqs": 8});
        assert!(!ad.apply_params(p.as_object().unwrap()).await.unwrap().ok);
        assert!(!ad.override_path().exists());
    }
}

#[tokio::test]
async fn test_containers_started_from_different_files_are_refused() {
    let f = Fix::new().await;
    let ov = sv_override(&f.compose);
    let d = dir(&f.compose);
    let wk = labelled(
        worker_doc(),
        Files::List(&[f.compose.clone(), ov]),
        &d,
        None,
        None,
        None,
    );
    let fe = labelled(
        frontend_doc(),
        Files::List(&[f.compose.clone()]),
        &d,
        None,
        None,
        None,
    );
    wire_docs(&f.runner, vec![wk, fe], "", None);
    let res = f.make().run_action("down", &none()).await.unwrap();
    assert!(
        !res.ok
            && res.output.contains("different compose files")
            && compose_calls(&f.runner).is_empty()
    );
}

#[tokio::test]
async fn test_a_foreign_container_label_is_ignored() {
    let f = Fix::new().await;
    let mut wk = labelled(
        worker_doc(),
        Files::Raw("/etc/evil.yml"),
        "/etc",
        None,
        None,
        None,
    );
    wk["Config"]["Labels"]["com.docker.compose.project"] = json!("someone-else"); // not ours: its label is not read
    wire_docs(&f.runner, vec![wk], "", None);
    let cf = f.make().compose_files(None).await.unwrap();
    assert!(cf.error.is_none() && !cf.from_label);
    assert_eq!(cf.files, [std::fs::canonicalize(&f.compose).unwrap()]);
}

// ── Params and the console override beside a repo override ──────────────────────────────────────

#[tokio::test]
async fn test_params_and_console_override_are_refused_while_an_override_file_is_in_use() {
    let f = Fix::new().await;
    wire_docs(&f.runner, sv_stack(&f.compose), "", merged());
    http_live(&f.mock, true);
    let ad = f.make();
    let p = serde_json::json!({"max_num_seqs": 8});
    let p = p.as_object().unwrap();
    let res = ad.apply_params(p).await.unwrap();
    assert!(
        !res.ok
            && res
                .detail
                .contains("docker-compose.yml, docker-compose.super-vl.yml")
    );
    assert!(
        !res.detail.contains("restates")
            && res.detail.contains("would replace that stack's worker")
    );
    assert!(!ad.override_path().exists() && compose_calls(&f.runner).is_empty());
    let v = ad
        .validate_config(
            "override",
            "services:\n  dynamo-worker:\n    command: [python3, -m, dynamo.vllm]\n",
        )
        .await
        .unwrap();
    assert!(
        !v.ok
            && v.diagnostics
                .iter()
                .any(|d| d.message.contains("would replace that stack's worker"))
    );
    assert!(ad.validate_config("override", "").await.unwrap().ok); // emptying it (revert) is always fine
    let st = ad.status().await.unwrap();
    let info = st
        .problems
        .iter()
        .find(|p| p.code == "dynamo.compose.override")
        .unwrap();
    assert!(
        info.severity == Severity::Info && info.message.contains("docker-compose.super-vl.yml")
    );
    let w = ad.param_warnings(p).unwrap().unwrap();
    assert!(
        w.iter()
            .any(|x| x.contains("would replace that stack's worker"))
    );
    let post = ad.post_apply("override", true).await.unwrap(); // re-checked even after validation
    assert!(
        !post.ok && post.post_check == PostCheck::Skipped && compose_calls(&f.runner).is_empty()
    );
    let mock_res = ad.run_action("up_mock", &none()).await.unwrap();
    assert!(!mock_res.ok && mock_res.problems[0].code == "dynamo.mock.override_in_use");
}

#[tokio::test]
async fn test_base_stack_params_still_apply() {
    let f = Fix::new().await;
    let wk = labelled(
        worker_doc(),
        Files::List(&[f.compose.clone()]),
        &dir(&f.compose),
        None,
        None,
        None,
    );
    wire_docs(&f.runner, vec![wk], "", None);
    let ad = f.make();
    let cf = ad.compose_files(None).await.unwrap();
    assert!(cf.overrides().is_empty());
    assert_eq!(ad.refusal(&cf, "Params"), None);
}

// ── status: version, components, start fixes, GPUs ──────────────────────────────────────────────

fn doc(v: Value) -> pyjson::PyJson {
    pyjson::loads(v.to_string().as_bytes()).unwrap()
}

#[tokio::test]
async fn test_version_is_the_dynamo_tag_never_the_gateway_digest() {
    let f = Fix::new().await;
    wire_docs(&f.runner, sv_stack(&f.compose), "", merged());
    http_live(&f.mock, true);
    let st = f.make().status().await.unwrap();
    assert_eq!(
        st.version.as_deref(),
        Some("1.6.0-nemotron-3.5-super-vl-dev.1")
    );
    assert_eq!(
        image_version(Some(&doc(json!({"Config": {"Image": NGINX}})))),
        None
    );
    assert_eq!(
        image_version(Some(&doc(
            json!({"Config": {"Image": "nvcr.io/nvidia/ai-dynamo/vllm-runtime@sha256:abc"}})
        ))),
        None
    );
    assert_eq!(
        image_version(Some(&doc(
            json!({"Config": {"Image": "nvcr.io/nvidia/ai-dynamo/vllm-runtime:1.5.0"}})
        )))
        .as_deref(),
        Some("1.5.0")
    );
    assert_eq!(
        image_version(Some(&doc(
            json!({"Config": {"Image": "localhost:5000/ai-dynamo/x"}})
        ))),
        None
    );
    // only the gateway left (the Dynamo containers are gone): no version at all, not nginx's
    f.runner.clear_rules();
    let gw = sv_stack(&f.compose).remove(0);
    wire_docs(&f.runner, vec![gw], "", None);
    assert_eq!(f.make().status().await.unwrap().version, None);
}

#[tokio::test]
async fn test_real_frontend_is_a_component_and_gateway_detail() {
    let f = Fix::new().await;
    wire_docs(&f.runner, sv_stack(&f.compose), "", merged());
    http_live(&f.mock, true);
    let st = f.make().status().await.unwrap();
    let names: Vec<&str> = st.components.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, [FE, SV, WK]);
    let sv = st.components.iter().find(|c| c.name == SV).unwrap();
    assert!(sv.state == "running" && sv.image.as_deref() == Some(SV_IMAGE));
    let fe = st.components.iter().find(|c| c.name == FE).unwrap();
    assert!(
        fe.detail
            .as_deref()
            .unwrap()
            .starts_with("gateway to dynamo-sv-frontend")
    );
    assert_eq!(st.health, Health::Up);
}

#[tokio::test]
async fn test_base_model_checks_are_not_raised_for_a_variant_worker() {
    // Super VL's worker runs without --dyn-enable-structural-tag and with vLLM's --reasoning-parser (live 2026-10-17)
    let f = Fix::new().await;
    let mut docs = sv_stack(&f.compose);
    let mut cmd: Vec<Value> = docs[2]["Config"]["Cmd"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c.as_str() != Some("--dyn-enable-structural-tag"))
        .cloned()
        .collect();
    cmd.extend([json!("--reasoning-parser"), json!("nemotron_v3")]);
    docs[2]["Config"]["Cmd"] = Value::Array(cmd);
    wire_docs(&f.runner, docs, "", merged());
    http_live(&f.mock, true);
    let c = codes(&f.make().status().await.unwrap());
    assert!(
        !c.iter()
            .any(|x| x == "dynamo.structural_tag.off" || x == "dynamo.parser.dialect")
    );
    assert!(c.iter().any(|x| x == "dynamo.compose.override"));
}

#[tokio::test]
async fn test_start_fixes_never_start_the_base_stack_over_a_variant() {
    let f = Fix::new().await;
    let mut docs = sv_stack(&f.compose);
    docs[1]["State"]["Running"] = json!(false); // the real frontend died behind the gateway
    docs[1]["State"]["Status"] = json!("exited");
    docs[1]["State"]["ExitCode"] = json!(1);
    wire_docs(&f.runner, docs, "", merged());
    http_live(&f.mock, true);
    let st = f.make().status().await.unwrap();
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "dynamo.frontend.down")
        .unwrap();
    let fix = p.fix.clone().unwrap();
    assert!(
        p.message.contains(SV)
            && !fix.contains("use-model.sh")
            && fix.contains("console action `up`")
    );
    assert!(fix.contains("docker-compose.super-vl.yml"));
}

#[tokio::test]
async fn test_worker_gpus_come_from_the_merged_config_and_are_cached() {
    let f = Fix::new().await;
    assert_eq!(
        merged_worker_uuids(&fx("merged_config_super_vl.json"), "dynamo-worker"),
        [CARD_B, CARD_A]
    );
    assert!(merged_worker_uuids("not json", "dynamo-worker").is_empty());
    assert!(merged_worker_uuids(r#"{"services": []}"#, "dynamo-worker").is_empty());
    let ov = sv_override(&f.compose);
    let mut wk = labelled(
        worker_doc(),
        Files::List(&[f.compose.clone(), ov.clone()]),
        &dir(&f.compose),
        None,
        None,
        None,
    );
    wk["State"]["Running"] = json!(false); // stopped: `up` checks the cards
    wk["State"]["Status"] = json!("exited");
    wk["State"]["ExitCode"] = json!(0);
    wk["HostConfig"]["DeviceRequests"] = Value::Null; // no container fallback to lean on
    let bolt = gpu_doc(
        BOLT,
        Some(&[CARD_B]),
        false,
        true,
        "ai-lab-vllm-nemotron-bolt",
    ); // on the OTHER card only
    let tts = gpu_doc(
        CHATTERBOX,
        Some(&[CARD_A]),
        false,
        true,
        "ai-lab-chatterbox-tts",
    );
    let pol = CardPolicy {
        exclusive: s(&[BOLT]),
        coexisting: s(&[CHATTERBOX]),
        ..CardPolicy::default()
    };
    wire_docs(
        &f.runner,
        vec![wk.clone(), bolt.clone(), tts],
        &format!("{BOLT}\n{CHATTERBOX}\n"),
        merged(),
    );
    let p1 = pol.clone();
    let ad = f.make_with(|a| a.policy = p1);
    let res = ad.run_action("up", &none()).await.unwrap();
    assert!(
        !res.ok
            && res.problems[0].code == "dynamo.up.exclusive_engine"
            && res.output.contains(BOLT)
    );
    assert!(!res.output.contains(CHATTERBOX)); // coexisting: allowed beside the worker
    assert_eq!(
        res.problems[0].fix.as_deref(),
        Some(
            format!(
                "stop {BOLT} first (exclusive with this stack), then run the console action `up`"
            )
            .as_str()
        )
    );
    let pf = ad.run_action("preflight", &none()).await.unwrap();
    assert!(
        pf.output.contains(&format!("{CARD_B}, {CARD_A}"))
            && pf.output.contains("docker-compose.super-vl.yml")
    );
    let probes: Vec<Vec<String>> = f
        .runner
        .argvs()
        .into_iter()
        .filter(|a| is_merged_probe(a))
        .collect();
    assert_eq!(probes.len(), 1); // cached
    assert_eq!(files_of(&probes[0]), [canon(&f.compose), canon(&ov)]);
    // a variant never falls back to the BASE file's single card: with no merged config and no container, refuse
    f.runner.clear_rules();
    wire_docs(&f.runner, vec![wk, bolt], &format!("{BOLT}\n"), None);
    let ad2 = f.make_with(|a| a.policy = pol);
    f.runner.clear_calls();
    let res2 = ad2.run_action("up", &none()).await.unwrap();
    assert!(!res2.ok && res2.output.contains("GPU UUID") && compose_calls(&f.runner).is_empty());
}

#[tokio::test]
async fn test_sv_frontend_logs_are_events_and_the_gateway_is_labelled() {
    let f = Fix::new().await;
    wire_docs(&f.runner, sv_stack(&f.compose), "", merged());
    f.runner.on_first(is_logs, |a: &[String]| {
        let line = match a.last().map(String::as_str) {
            Some(FE) => "2026-10-17T09:00:00.000000000Z 10.89.13.2 [17/Oct/2026:09:00:00 +0000] TCP 200 1234 567 0.010\n",
            Some(SV) => "2026-10-17T09:00:01.000000000Z 2026-10-17T09:00:01.000Z INFO dynamo_llm::http::service: request done\n",
            Some(WK) => "2026-10-17T09:00:02.000000000Z INFO worker ready\n",
            _ => "",
        };
        ok(line)
    });
    let evs = f.make().recent_events(50).await.unwrap();
    let by: Vec<(String, String)> = evs
        .iter()
        .map(|e| {
            (
                e.data["component"].as_str().unwrap().to_owned(),
                e.summary.split_whitespace().next().unwrap_or("").to_owned(),
            )
        })
        .collect();
    for want in [
        ("gateway", "10.89.13.2"),
        ("frontend", "INFO"),
        ("worker", "INFO"),
    ] {
        assert!(
            by.iter().any(|(c, s)| c == want.0 && s == want.1),
            "{want:?} in {by:?}"
        );
    }
    assert!(
        f.runner
            .argvs()
            .iter()
            .any(|a| is_logs(a) && a.last().is_some_and(|x| x == SV))
    );
}

#[test]
fn test_compose_files_overrides_property() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().join("docker-compose.yml");
    std::fs::write(&base, "services: {}\n").unwrap();
    let ov = tmp.path().join("docker-compose.super-vl.yml");
    let rb = std::fs::canonicalize(&base).unwrap();
    assert!(
        ComposeFiles::new(vec![rb.clone()], Some(base.clone()))
            .overrides()
            .is_empty()
    );
    assert_eq!(
        ComposeFiles::new(vec![rb.clone(), ov.clone()], Some(base.clone())).overrides(),
        [ov.clone()]
    );
    assert_eq!(
        ComposeFiles::new(vec![rb, ov], Some(base)).names(),
        "docker-compose.yml, docker-compose.super-vl.yml"
    );
}

#[tokio::test]
async fn test_holds_card_is_information_not_a_warning() {
    let f = Fix::new().await;
    wire(&f.runner, vec![frontend_doc(), worker_doc()], "");
    http_live(&f.mock, true);
    let st = f.make().status().await.unwrap();
    let p = st
        .problems
        .iter()
        .find(|p| p.code == "dynamo.holds_card")
        .unwrap();
    assert!(p.severity == Severity::Info && st.health == Health::Up);
    // an empty dark list (setting) means nothing to report
    let st2 = f
        .make_with(|a| {
            a.policy = CardPolicy {
                dark: Vec::new(),
                ..CardPolicy::default()
            }
        })
        .status()
        .await
        .unwrap();
    assert!(!codes(&st2).iter().any(|c| c == "dynamo.holds_card"));
}

#[tokio::test]
async fn test_preflight_beside_a_variant_names_the_engine_not_the_base_start() {
    let f = Fix::new().await;
    let ov = sv_override(&f.compose);
    let mut wk = labelled(
        worker_doc(),
        Files::List(&[f.compose.clone(), ov]),
        &dir(&f.compose),
        None,
        None,
        None,
    );
    wk["State"]["Running"] = json!(false);
    wk["State"]["Status"] = json!("exited");
    wk["State"]["ExitCode"] = json!(0);
    let bolt = gpu_doc(
        BOLT,
        Some(&[CARD_B]),
        false,
        true,
        "ai-lab-vllm-nemotron-bolt",
    );
    wire_docs(&f.runner, vec![wk, bolt], &format!("{BOLT}\n"), merged());
    let pf = f
        .make_with(|a| {
            a.policy = CardPolicy {
                exclusive: s(&[BOLT]),
                ..CardPolicy::default()
            }
        })
        .run_action("preflight", &none())
        .await
        .unwrap();
    let shared: Vec<_> = pf
        .problems
        .iter()
        .filter(|p| p.code == "dynamo.card.shared")
        .collect();
    assert_eq!(shared.len(), 1);
    assert_eq!(
        shared[0].fix.as_deref(),
        Some(format!("stop {BOLT} first, then run the console action `up`").as_str())
    );
    assert!(!pf.output.contains("qwen3.8-27b-dynamo")); // the BASE stack's start command is never offered
    assert!(
        pf.output
            .contains("exclusive with this stack: docker-compose.yml, docker-compose.super-vl.yml")
    );
}
