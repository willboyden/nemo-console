//! Addendum 9.17, Relay runtime (PORTING.md D-44): the new Relay metric keys from Prometheus, the capture answer's
//! `resources` object and `resource_metrics` level, resource marks kept out of the trace index, and the admin route
//! `GET /systems/relay/diagnostics`. Hermetic: a fake Prometheus on an ephemeral port (wiremock), fake runners that
//! answer with the sanitized 0.11.1 fixtures (`tests/fixtures/relay_diagnostics`, every event detail and correlation
//! a SENTINEL string), closed ports for everything else, files under a temp dir only.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::cast_precision_loss
)]

mod common;
mod relay_support;
mod relay_traces_support;
mod switchyard_support;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use nemo_console::adapters::base::{AdapterContext, CommandResult, CommandRunner, SystemAdapter};
use nemo_console::adapters::relay::RelayAdapter;
use nemo_console::adapters::relay_runtime::{
    self, GAUGE_QUERY, MAX_COUNT, OVERHEAD_QUERY, PRESSURE_QUERY, QUERIES, ROUTING_QUERY, Reason,
};
use nemo_console::adapters::relay_traces::{Request, TraceConfig};
use nemo_console::config::settings_for_test;
use nemo_console::container::{AppState, Overrides, build_container};
use nemo_console::domain::models::{MetricPoint, Role, SystemId};
use nemo_console::services::environ::Environ;
use nemo_console::services::http::Http;
use relay_traces_support::{Lab, at, config, mark, scope};
use serde_json::{Value, json};
use switchyard_support::PrefixRunner;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DIAG: &str = "/api/v1/systems/relay/diagnostics";
/// Text of the fixtures that must never reach an answer.
const FORBIDDEN: [&str; 11] = [
    "SENTINEL",
    "00000000-0000-7000-8000-000000000000",
    "4242",
    "/home/",
    "\"correlation\"",
    "\"detail\"",
    "\"origin\"",
    "\"context\"",
    "instance_id",
    "process_id",
    "ownership",
];

fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/relay_diagnostics")
        .join(name);
    std::fs::read_to_string(p).unwrap()
}

fn fixture_json(name: &str) -> Value {
    serde_json::from_str(&fixture(name)).unwrap()
}

fn ok(stdout: &str) -> CommandResult {
    CommandResult::new(0, stdout, "")
}

/// A runner that answers `nemo-relay diagnostics` with `snap` and `... --memory` with `mem`.
fn diag_runner(snap: CommandResult, mem: CommandResult) -> Arc<PrefixRunner> {
    let r = PrefixRunner::new();
    r.on_fn(&["nemo-relay", "diagnostics"], move |a| {
        if a.iter().any(|x| x == "--memory") {
            mem.clone()
        } else {
            snap.clone()
        }
    });
    r
}

fn ctx_with(
    runner: Arc<dyn CommandRunner>,
    urls: &[(&str, &str)],
) -> (AdapterContext, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = AdapterContext {
        runner,
        http: Http::new().unwrap(),
        urls: urls
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        paths: IndexMap::new(),
        secret_env: IndexMap::new(),
        state_dir: tmp.path().join("state"),
        environ: Environ::from_pairs(&[]),
    };
    (ctx, tmp)
}

/// A Relay adapter over the given runner; nothing listens on port 9.
fn diag_adapter(runner: Arc<PrefixRunner>) -> (Arc<RelayAdapter>, tempfile::TempDir) {
    diag_adapter_with(runner, "http://127.0.0.1:9", Duration::from_secs(30))
}

fn diag_adapter_with(
    runner: Arc<dyn CommandRunner>,
    relay: &str,
    deadline: Duration,
) -> (Arc<RelayAdapter>, tempfile::TempDir) {
    let (ctx, tmp) = ctx_with(
        runner,
        &[("relay", relay), ("litellm", "http://127.0.0.1:9")],
    );
    let mut a = RelayAdapter::with_traces(ctx, config());
    a.diag_deadline = deadline;
    (Arc::new(a), tmp)
}

fn fixture_runner() -> Arc<PrefixRunner> {
    diag_runner(ok(&fixture("snapshot.json")), ok(&fixture("memory.json")))
}

fn keys(v: &Value) -> BTreeSet<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn assert_clean(v: &Value) {
    let text = v.to_string();
    for bad in FORBIDDEN {
        assert!(!text.contains(bad), "{bad} reached the answer");
    }
}

const TOP_KEYS: [&str; 12] = [
    "available",
    "reason",
    "relay_version",
    "captured_at",
    "elapsed_s",
    "totals",
    "last_failure_at",
    "components",
    "events",
    "loss",
    "retention",
    "memory",
];
const EVENT_KEYS: [&str; 8] = [
    "seq",
    "at",
    "component",
    "code",
    "failure_class",
    "severity",
    "outcome",
    "count",
];
const COMPONENT_KEYS: [&str; 6] = [
    "key",
    "instances",
    "successes",
    "failures",
    "occurrences",
    "last_failure_at",
];

// ── diagnostics: the mapping of the real 0.11.1 shapes ─────────────────────────────────────────────

#[tokio::test]
async fn diagnostics_from_the_fixtures_follow_the_contract_and_drop_every_private_field() {
    let run = fixture_runner();
    let (a, _tmp) = diag_adapter(Arc::clone(&run));
    let v = a.relay_diagnostics().await.unwrap();
    assert_eq!(keys(&v), set(&TOP_KEYS));
    assert_eq!(v["available"], true);
    assert_eq!(v["reason"], Value::Null);
    assert_eq!(v["relay_version"], "0.11.1");
    assert_eq!(v["captured_at"], "2026-01-01T08:27:37.295079873Z");
    assert_eq!(v["elapsed_s"], 18.141);
    assert_eq!(
        v["totals"],
        json!({"recorded": 150, "successes": 150, "failures": 0, "unspecified": 0, "evictions": 0})
    );
    assert_eq!(v["last_failure_at"], Value::Null);
    // components: grouped by key (23 entries, 21 keys), failures first, then by key
    let comps = v["components"].as_array().unwrap();
    assert_eq!(comps.len(), 21);
    for c in comps {
        assert_eq!(keys(c), set(&COMPONENT_KEYS));
    }
    assert_eq!(comps[0]["key"], "cli.upstream");
    assert_eq!(comps[0]["failures"], 2);
    assert_eq!(
        comps[0]["last_failure_at"],
        "2026-01-01T08:27:37.295079873Z"
    );
    let rest: Vec<&str> = comps[1..]
        .iter()
        .map(|c| c["key"].as_str().unwrap())
        .collect();
    let mut sorted = rest.clone();
    sorted.sort_unstable();
    assert_eq!(rest, sorted);
    assert_eq!(
        comps
            .iter()
            .map(|c| c["instances"].as_u64().unwrap())
            .sum::<u64>(),
        23
    );
    // events: newest first, only the allowlisted fields
    let evs = v["events"].as_array().unwrap();
    assert_eq!(evs.len(), 150);
    for e in evs {
        assert_eq!(keys(e), set(&EVENT_KEYS));
    }
    let seqs: Vec<u64> = evs.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[0] > w[1]), "newest first");
    let failure = evs.iter().find(|e| e["outcome"] == "failure").unwrap();
    assert_eq!(failure["failure_class"], "unavailable");
    assert_eq!(failure["severity"], "error");
    assert_eq!(failure["component"], "cli.upstream");
    assert!(
        failure["code"]
            .as_str()
            .unwrap()
            .starts_with("cli.upstream.")
    );
    assert_eq!(evs.last().unwrap()["code"], "logging.sink.initialized");
    assert_eq!(evs.last().unwrap()["failure_class"], Value::Null);
    // loss: the allowlisted names plus the console's own count
    let loss = v["loss"].as_object().unwrap();
    assert_eq!(loss.len(), relay_runtime::LOSS_NAMES.len() + 1);
    assert_eq!(loss["projections_suppressed"], 143);
    assert_eq!(loss["console_dropped"], 0);
    assert!(!loss.contains_key("sequence_saturated"));
    assert_eq!(
        v["retention"],
        json!({"events_retained": 150, "oldest_seq": 10, "latest_seq": 187, "truncated": false})
    );
    assert_eq!(
        v["memory"],
        json!({"reserved_bytes": 2_595_089, "events": 150, "event_capacity": 256, "detail_bytes": 38400,
               "detail_capacity_bytes": 262_144})
    );
    assert_clean(&v);
    // the two CLI calls: no shell, the gateway URL and the response caps of the contract
    let calls = run.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0],
        [
            "nemo-relay",
            "diagnostics",
            "--gateway-url=http://127.0.0.1:9",
            "--max-response-bytes",
            "1048576"
        ]
    );
    assert_eq!(
        calls[1],
        [
            "nemo-relay",
            "diagnostics",
            "--gateway-url=http://127.0.0.1:9",
            "--memory",
            "--max-response-bytes",
            "262144"
        ]
    );
}

#[test]
fn the_fixture_really_carries_the_sentinels_the_answer_drops() {
    // a guard on the test above: the fields it proves absent are present in the input
    let raw = fixture("snapshot.json");
    assert!(raw.matches("SECRET-DETAIL-SENTINEL").count() >= 150);
    assert!(raw.contains("SECRET-CORRELATION-SENTINEL"));
    assert!(raw.contains("\"process_id\": 4242") || raw.contains("\"process_id\":4242"));
}

#[tokio::test]
async fn a_failed_memory_call_leaves_memory_null_and_the_rest_available() {
    let run = diag_runner(
        ok(&fixture("snapshot.json")),
        CommandResult::new(1, "", "launcher error: something"),
    );
    let (a, _tmp) = diag_adapter(run);
    let v = a.relay_diagnostics().await.unwrap();
    assert_eq!(v["available"], true);
    assert_eq!(v["memory"], Value::Null);
    assert_eq!(v["events"].as_array().unwrap().len(), 150);
}

// ── diagnostics: hostile and broken input ──────────────────────────────────────────────────────────

fn hostile_snapshot() -> Value {
    let mut s = fixture_json("snapshot.json");
    let comps = s["components"].as_array_mut().unwrap();
    let template = comps[0].clone();
    let mut bad_key = template.clone();
    bad_key["identity"]["key"] = json!("Bad Key; rm -rf /");
    let mut long_key = template.clone();
    long_key["identity"]["key"] = json!("a".repeat(97));
    let mut num_key = template.clone();
    num_key["identity"]["key"] = json!(42);
    let mut huge = template;
    huge["identity"]["key"] = json!("huge.counts");
    huge["lifetime"]["totals"]["failures_total"] =
        json!({"value": "9".repeat(4000), "saturated": false});
    huge["lifetime"]["totals"]["successes_total"] = json!({"value": "12", "saturated": true});
    huge["lifetime"]["totals"]["occurrences_total"] = json!({"value": -5, "saturated": "yes"});
    huge["lifetime"]["last_failure_at"] = json!("not a time SENTINEL");
    comps.extend([bad_key, long_key, num_key, huge]);
    let events = s["events"].as_array_mut().unwrap();
    let mut e = events[0].clone();
    e["sequence"] = json!("99999");
    e["classification"]["canonical_code"] = json!("DROP TABLE x; --");
    e["classification"]["failure_class"] = json!({"nested": "SENTINEL"});
    e["severity"] = json!(7);
    e["outcome"] = json!("Failure With Spaces");
    e["component"]["key"] = json!("Not.A.Key");
    e["count"] = json!("1e9");
    e["observed_at"] = json!("2026-02-30T25:61:61Z");
    e["extra_field"] = json!("SENTINEL extra");
    events.push(e);
    events.push(json!("not an event"));
    events.push(json!(null));
    s["source"]["relay_version"] = json!("0.11.1; curl evil");
    s["captured_at"] = json!(12345);
    s["lifetime"]["totals"] = json!(["wrong", "type"]);
    s["lifetime"]["elapsed_millis"] = json!("-3");
    s["coverage"]["loss"]["secret_leak"] = json!({"value": "1", "saturated": false});
    s["coverage"]["loss"]["events_evicted"] =
        json!({"value": "18446744073709551616", "saturated": false});
    s["coverage"]["loss"]["events_omitted"] = json!("garbage");
    s["coverage"]["retention"] = json!("nope");
    s
}

#[test]
fn hostile_values_are_checked_saturated_or_dropped() {
    let v = relay_runtime::map_diagnostics(&hostile_snapshot(), None);
    assert_eq!(v["available"], true);
    assert_eq!(v["relay_version"], Value::Null);
    assert_eq!(v["captured_at"], Value::Null);
    assert_eq!(v["elapsed_s"], Value::Null);
    assert_eq!(
        v["totals"],
        json!({"recorded": null, "successes": null, "failures": null, "unspecified": null, "evictions": null})
    );
    let comps = v["components"].as_array().unwrap();
    // three entries with a bad key are dropped and counted
    assert_eq!(v["loss"]["console_dropped"], 3);
    let keys: Vec<&str> = comps.iter().map(|c| c["key"].as_str().unwrap()).collect();
    assert!(!keys.iter().any(|k| k.contains(' ') || k.len() > 96));
    let huge = &comps[0];
    assert_eq!(huge["key"], "huge.counts", "most failures first");
    assert_eq!(huge["failures"], MAX_COUNT);
    assert_eq!(huge["successes"], MAX_COUNT);
    assert_eq!(huge["occurrences"], 0);
    assert_eq!(huge["last_failure_at"], Value::Null);
    let evs = v["events"].as_array().unwrap();
    assert_eq!(evs.len(), 151, "non-object entries are skipped");
    let e = &evs[0];
    assert_eq!(e["seq"], 99999);
    assert_eq!(e["code"], "other");
    assert_eq!(e["failure_class"], "other");
    assert_eq!(e["severity"], "other");
    assert_eq!(e["outcome"], "other");
    assert_eq!(e["component"], "other");
    assert_eq!(e["count"], Value::Null);
    assert_eq!(e["at"], Value::Null);
    assert_eq!(keys_of(e), set(&EVENT_KEYS));
    let loss = v["loss"].as_object().unwrap();
    assert!(!loss.contains_key("secret_leak"));
    assert!(!loss.contains_key("events_omitted"));
    assert_eq!(loss["events_evicted"], MAX_COUNT);
    assert_eq!(
        v["retention"],
        // Relay's figures are unreadable: what was gathered (150 events + the hostile one) stands in
        json!({"events_retained": 151, "oldest_seq": 10, "latest_seq": 99999, "truncated": false})
    );
    assert_clean(&v);
}

fn keys_of(v: &Value) -> BTreeSet<String> {
    keys(v)
}

#[test]
fn caps_keep_64_components_and_the_newest_256_events() {
    let mut s = fixture_json("snapshot.json");
    let template = s["components"][0].clone();
    let comps: Vec<Value> = (0..100)
        .map(|i| {
            let mut c = template.clone();
            c["identity"]["key"] = json!(format!("comp.{i:03}"));
            c["lifetime"]["totals"]["failures_total"] =
                json!({"value": (i % 7).to_string(), "saturated": false});
            c
        })
        .collect();
    s["components"] = json!(comps);
    let template = s["events"][0].clone();
    let events: Vec<Value> = (0..1000)
        .map(|i| {
            let mut e = template.clone();
            e["sequence"] = json!(((i * 7919) % 1000).to_string());
            e
        })
        .collect();
    s["events"] = json!(events);
    let v = relay_runtime::map_diagnostics(&s, None);
    let comps = v["components"].as_array().unwrap();
    assert_eq!(comps.len(), relay_runtime::MAX_COMPONENTS);
    assert_eq!(v["loss"]["console_dropped"], 36);
    let fails: Vec<u64> = comps
        .iter()
        .map(|c| c["failures"].as_u64().unwrap())
        .collect();
    assert!(fails.windows(2).all(|w| w[0] >= w[1]));
    let evs = v["events"].as_array().unwrap();
    assert_eq!(evs.len(), relay_runtime::MAX_EVENTS);
    assert_eq!(evs[0]["seq"], 999);
    assert_eq!(evs[255]["seq"], 744);
}

#[test]
fn garbage_or_foreign_output_is_an_error_and_never_echoed() {
    for out in [
        "",
        "not json SENTINEL",
        "{\"truncated\": ",
        "[1, 2, 3]",
        "{\"error\": \"SENTINEL\"}",
        &"[".repeat(10_000),
    ] {
        let v = relay_runtime::answer(&ok(out), None);
        assert_eq!(v["available"], false, "{out:.40}");
        assert_eq!(v["reason"], "error", "{out:.40}");
        assert_clean(&v);
    }
    // output cut at the runner's cap (SIGKILL, -9): a truncated document is an error too
    let full = fixture("snapshot.json");
    let cut = CommandResult::new(-9, &full[..full.len() / 2], "");
    assert_eq!(relay_runtime::answer(&cut, None)["reason"], "error");
    // an empty but well-formed snapshot is available with nulls
    let v = relay_runtime::answer(&ok("{\"lifetime\": {}}"), None);
    assert_eq!(v["available"], true);
    assert_eq!(v["components"], json!([]));
    assert_eq!(v["memory"], Value::Null);
}

#[test]
fn failures_map_to_fixed_reasons() {
    let disabled = CommandResult::new(
        0,
        "",
        "launcher error: diagnostic inspection unavailable (gateway did not establish an authenticated Relay TLS \
         tunnel); an existing listener, private operator credentials, HTTP read policy, and a sufficient \
         --max-response-bytes are required\n",
    );
    let refused = CommandResult::new(
        1,
        "",
        "launcher error: diagnostic inspection unavailable (failed to connect to verified gateway \
         http://127.0.0.1:1: Connection refused (os error 111)); ...\n",
    );
    let old = CommandResult::new(
        2,
        "",
        "error: unrecognized subcommand 'diagnostics'\n\nUsage: nemo-relay [OPTIONS] [COMMAND]\n",
    );
    let missing = CommandResult::new(127, "", "FileNotFoundError: No such file or directory");
    let mut slow = CommandResult::new(-9, "", "");
    slow.timed_out = true;
    let cases = [
        (&disabled, Reason::Disabled),
        (&refused, Reason::Unreachable),
        (&old, Reason::UnsupportedVersion),
        (&missing, Reason::Error),
        (&slow, Reason::Unreachable),
        (&CommandResult::new(3, "", "SENTINEL boom"), Reason::Error),
    ];
    for (r, want) in cases {
        assert_eq!(relay_runtime::failure_reason(r), want, "{}", r.stderr);
        let v = relay_runtime::answer(r, None);
        assert_eq!(keys(&v), set(&TOP_KEYS));
        assert_eq!(v["available"], false);
        assert_eq!(v["reason"], want.as_str());
        for k in [
            "relay_version",
            "captured_at",
            "elapsed_s",
            "totals",
            "last_failure_at",
            "retention",
            "memory",
        ] {
            assert_eq!(v[k], Value::Null, "{k}");
        }
        assert_eq!(v["components"], json!([]));
        assert_eq!(v["events"], json!([]));
        assert_eq!(v["loss"], json!({}));
        assert_clean(&v);
    }
    // a JSON answer with a failing exit code is not trusted either
    let v = relay_runtime::answer(&CommandResult::new(1, &fixture("snapshot.json"), ""), None);
    assert_eq!(v["available"], false);
}

#[tokio::test]
async fn a_failed_snapshot_skips_the_memory_call() {
    let run = diag_runner(
        CommandResult::new(2, "", "error: unrecognized subcommand 'diagnostics'"),
        ok(&fixture("memory.json")),
    );
    let (a, _tmp) = diag_adapter(Arc::clone(&run));
    let v = a.relay_diagnostics().await.unwrap();
    assert_eq!(v["reason"], "unsupported_version");
    assert_eq!(run.calls().len(), 1);
}

// ── diagnostics: cache and single flight ───────────────────────────────────────────────────────────

#[tokio::test]
async fn answers_are_cached_and_failures_too() {
    let run = fixture_runner();
    let (a, _tmp) = diag_adapter(Arc::clone(&run));
    let first = a.relay_diagnostics().await.unwrap();
    let second = a.relay_diagnostics().await.unwrap();
    assert_eq!(first, second);
    assert_eq!(run.calls().len(), 2, "one snapshot and one memory call");
    let run = diag_runner(CommandResult::new(1, "", "x"), ok(""));
    let (a, _tmp) = diag_adapter(Arc::clone(&run));
    a.relay_diagnostics().await.unwrap();
    a.relay_diagnostics().await.unwrap();
    assert_eq!(run.calls().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_share_one_read() {
    let snap = fixture("snapshot.json");
    let mem = fixture("memory.json");
    let r = PrefixRunner::new();
    r.on_fn(&["nemo-relay", "diagnostics"], move |a| {
        std::thread::sleep(Duration::from_millis(150));
        if a.iter().any(|x| x == "--memory") {
            ok(&mem)
        } else {
            ok(&snap)
        }
    });
    let (a, _tmp) = diag_adapter(Arc::clone(&r));
    let tasks: Vec<_> = (0..6)
        .map(|_| {
            let a = Arc::clone(&a);
            tokio::spawn(async move { a.relay_diagnostics().await.unwrap() })
        })
        .collect();
    let mut answers = Vec::new();
    for t in tasks {
        answers.push(t.await.unwrap());
    }
    assert!(answers.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(r.calls().len(), 2);
}

// ── diagnostics: the route ─────────────────────────────────────────────────────────────────────────

fn app_for(
    a: &Arc<RelayAdapter>,
    runner: Arc<dyn CommandRunner>,
    tmp: &std::path::Path,
) -> Arc<AppState> {
    let mut urls = serde_json::Map::new();
    for k in [
        "switchyard",
        "relay",
        "dynamo",
        "dynamo_worker_metrics",
        "litellm",
        "dcgm",
        "prometheus",
        "guardrails",
    ] {
        urls.insert(k.to_owned(), json!("http://127.0.0.1:9"));
    }
    let settings = settings_for_test(json!({
        "state_dir": tmp.join("app-state").to_string_lossy(),
        "frontend_dir": tmp.join("front").to_string_lossy(),
        "repo_root": tmp.join("repo").to_string_lossy(),
        "background_tasks": false, "trust_loopback": true, "urls": urls,
    }));
    let mut adapters: HashMap<SystemId, Arc<dyn SystemAdapter>> = HashMap::new();
    for sid in SystemId::ALL {
        if sid == SystemId::Relay {
            adapters.insert(sid, Arc::clone(a) as Arc<dyn SystemAdapter>);
        } else {
            adapters.insert(
                sid,
                common::FakeAdapter::new(sid, None) as Arc<dyn SystemAdapter>,
            );
        }
    }
    build_container(
        settings,
        Overrides {
            adapters,
            runner: Some(runner),
            http: Some(Http::new().unwrap()),
            registry: None,
            environ: Some(Environ::fixed(HashMap::new())),
        },
    )
    .unwrap()
}

#[tokio::test]
async fn the_route_is_admin_only_and_relay_only() {
    let run = fixture_runner();
    let (a, tmp) = diag_adapter(Arc::clone(&run));
    let st = app_for(&a, run.clone(), tmp.path());
    let r = common::TestReq::new(Arc::clone(&st), "GET", DIAG)
        .send()
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let v = r.json();
    assert_eq!(v["available"], true);
    assert_clean(&v);
    let viewer = st.auth.mint(Role::Viewer, "").unwrap().1;
    let admin = st.auth.mint(Role::Admin, "").unwrap().1;
    let as_key = |key: &str, p: &str| {
        common::TestReq::new(Arc::clone(&st), "GET", p)
            .client("192.0.2.10")
            .token(key)
    };
    let r = as_key(&viewer, DIAG).send().await;
    assert_eq!((r.status, r.code()), (403, "forbidden".to_owned()));
    let r = as_key(&admin, DIAG).send().await;
    assert_eq!(r.status, 200);
    // the role is checked first: a viewer is refused on every system
    let r = as_key(&viewer, "/api/v1/systems/switchyard/diagnostics")
        .send()
        .await;
    assert_eq!(r.status, 403);
    for sys in ["switchyard", "dynamo", "dcgm", "openshell"] {
        let r = common::TestReq::new(
            Arc::clone(&st),
            "GET",
            &format!("/api/v1/systems/{sys}/diagnostics"),
        )
        .send()
        .await;
        assert_eq!((r.status, r.code()), (404, "not_found".to_owned()), "{sys}");
    }
    let r = common::TestReq::new(Arc::clone(&st), "GET", DIAG)
        .client("192.0.2.10")
        .send()
        .await;
    assert_eq!(r.status, 401);
    // the viewer's and the other systems' requests never ran the CLI; the two admin reads shared one cached read
    assert_eq!(run.calls().len(), 2);
}

#[test]
fn the_route_table_and_openapi_entry_match_the_code() {
    let def = nemo_console::api::ROUTES
        .iter()
        .find(|r| r.path == "/api/v1/systems/{system_id}/diagnostics")
        .unwrap();
    assert_eq!(def.methods, ["GET"]);
    assert_eq!(def.role, nemo_console::api::RoleDep::Admin);
    let doc: Value = serde_json::from_str(nemo_console::api::admin::OPENAPI_JSON).unwrap();
    let e = &doc["paths"]["/api/v1/systems/{system_id}/diagnostics"]["get"];
    let params: Vec<(String, String)> = e["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["name"].as_str().unwrap().to_owned(),
                p["in"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(params, [("system_id".to_owned(), "path".to_owned())]);
    let d = e["description"].as_str().unwrap();
    for word in [
        "admin",
        "available",
        "reason",
        "disabled",
        "unsupported_version",
        "detail",
    ] {
        assert!(d.contains(word), "{word}");
    }
}

// ── Prometheus: the new Relay metric keys ──────────────────────────────────────────────────────────

fn vector(rows: &[(Value, &str)]) -> Value {
    json!({"status": "success", "data": {"resultType": "vector", "result": rows.iter().map(|(m, v)| {
        json!({"metric": m, "value": [1_794_732_037.694, v]})
    }).collect::<Vec<_>>()}})
}

fn res_labels(name: &str) -> Value {
    json!({"__name__": name, "job": "nemo-relay", "nemo_relay_resource_measurement_scope": "application_process",
           "nemo_relay_resource_operating_system": "linux"})
}

fn fs_labels(name: &str, path: &str) -> Value {
    let mut l = res_labels(name);
    l["nemo_relay_resource_disk_filesystem_path"] = json!(path);
    l
}

fn gauges_answer() -> Value {
    let n = |s: &str| format!("nemo_relay_resource_{s}");
    let mut lim1 = res_labels(&n("limit_event_count_events"));
    lim1["nemo_relay_resource_limit_event"] = json!("throttle");
    let mut lim2 = res_labels(&n("limit_event_count_events"));
    lim2["nemo_relay_resource_limit_event"] = json!("oom");
    vector(&[
        (
            res_labels(&n("cpu_consumption_rate_logical_processors")),
            "0.0005",
        ),
        (res_labels(&n("memory_resident_kibibytes")), "70436"),
        (
            res_labels(&n("memory_environment_accounted_kibibytes")),
            "73980",
        ),
        (res_labels(&n("thread_count_threads")), "79"),
        (
            res_labels(&n("process_open_file_descriptor_count_file_descriptors")),
            "40",
        ),
        (res_labels(&n("disk_read_throughput_bytes_per_second")), "0"),
        (
            res_labels(&n("disk_write_throughput_bytes_per_second")),
            "409.5842537449833",
        ),
        (
            fs_labels(
                &n("disk_filesystem_available_capacity_bytes"),
                "/home/someone/.local/share/nemo-relay/atof",
            ),
            "960598917120",
        ),
        (
            fs_labels(
                &n("disk_filesystem_total_capacity_bytes"),
                "/home/someone/.local/share/nemo-relay/atof",
            ),
            "2063083421696",
        ),
        (
            fs_labels(
                &n("disk_filesystem_available_capacity_bytes"),
                "/home/someone/other",
            ),
            "999999999999999",
        ),
        (
            fs_labels(
                &n("disk_filesystem_total_capacity_bytes"),
                "/home/someone/other",
            ),
            "9999999999999999",
        ),
        (lim1, "2"),
        (lim2, "3"),
        (res_labels(&n("some_unrelated_series")), "123"),
    ])
}

async fn fake_prometheus(answers: [Value; 4]) -> MockServer {
    let server = MockServer::start().await;
    for (q, a) in QUERIES.iter().zip(answers) {
        Mock::given(method("GET"))
            .and(path("/api/v1/query"))
            .and(query_param("query", *q))
            .respond_with(ResponseTemplate::new(200).set_body_json(a))
            .mount(&server)
            .await;
    }
    server
}

fn tagged(tag: &str, v: &str) -> (Value, &'static str) {
    // the label is the fixed one the query adds; the value is leaked on purpose to keep the helper simple
    (json!({"k": tag}), Box::leak(v.to_owned().into_boxed_str()))
}

fn good_answers() -> [Value; 4] {
    [
        gauges_answer(),
        vector(&[tagged("cpu", "1.25"), tagged("mem", "0")]),
        vector(&[
            tagged("rps", "0.5"),
            tagged("fail", "0.1"),
            tagged("tok", "250"),
        ]),
        vector(&[(json!({}), "3.75")]),
    ]
}

/// The Relay adapter of the status tests, with `urls.prometheus` set.
async fn metrics_with(prometheus: &str) -> (Vec<MetricPoint>, bool) {
    let r = relay_support::Router::new().await;
    let m = relay_support::make_adapter(&r, None, None, None, |_| {});
    let mut c = m.a.ctx.clone();
    c.urls
        .insert("prometheus".to_owned(), prometheus.to_owned());
    let a = RelayAdapter::new(c);
    let snap = a.metrics().await.unwrap();
    (snap.points, snap.available)
}

fn by_key(points: &[MetricPoint]) -> BTreeMap<String, (f64, String)> {
    points
        .iter()
        .map(|p| (p.key.clone(), (p.value, p.unit.clone())))
        .collect()
}

const BASE_KEYS: [&str; 6] = [
    "relay.gateway_up",
    "relay.plugin_loaded",
    "relay.atof_files",
    "relay.atof_bytes",
    "relay.atif_trajectories",
    "relay.exporters_enabled",
];

#[tokio::test]
async fn the_new_keys_come_from_prometheus_with_their_units_and_no_labels() {
    let prom = fake_prometheus(good_answers()).await;
    let (points, available) = metrics_with(&prom.uri()).await;
    assert!(available);
    let got = by_key(&points);
    let want: [(&str, f64, &str); 16] = [
        ("relay.gw_cpu", 0.0005, "cores"),
        ("relay.gw_rss", 70436.0 * 1024.0, "B"),
        ("relay.gw_mem_accounted", 73980.0 * 1024.0, "B"),
        ("relay.gw_threads", 79.0, "1"),
        ("relay.gw_fds", 40.0, "1"),
        ("relay.gw_disk_read", 0.0, "B/s"),
        ("relay.gw_disk_write", 409.584_253_744_983_3, "B/s"),
        ("relay.atof_fs_free", 960_598_917_120.0, "B"),
        (
            "relay.atof_fs_used",
            1.0 - 960_598_917_120.0 / 2_063_083_421_696.0,
            "ratio",
        ),
        ("relay.gw_cpu_pressure", 1.25, "ms/s"),
        ("relay.gw_mem_pressure", 0.0, "ms/s"),
        ("relay.gw_limit_events", 5.0, "1"),
        ("relay.route_rps", 0.5, "req/s"),
        ("relay.route_failures", 0.1, "req/s"),
        ("relay.route_tokens", 250.0, "tok/s"),
        ("relay.route_overhead_p95", 3.75, "ms"),
    ];
    for (k, v, unit) in want {
        let (gv, gu) = got.get(k).unwrap_or_else(|| panic!("{k} missing"));
        assert!((gv - v).abs() < 1e-9, "{k}: {gv} != {v}");
        assert_eq!(gu, unit, "{k}");
    }
    let allowed: BTreeSet<&str> = want.iter().map(|w| w.0).chain(BASE_KEYS).collect();
    assert!(got.keys().all(|k| allowed.contains(k.as_str())), "{got:?}");
    // labels are never forwarded (the filesystem label holds a home path)
    assert!(points.iter().all(|p| p.labels.is_empty()));
    assert!(!serde_json::to_string(&points).unwrap().contains("/home/"));
    // at most 6 queries per poll, every one of them a fixed expression
    let reqs = prom.received_requests().await.unwrap();
    assert!(reqs.len() <= 6 && reqs.len() == QUERIES.len());
    for r in &reqs {
        let q: String = r
            .url
            .query_pairs()
            .find(|(k, _)| k == "query")
            .map(|(_, v)| v.into_owned())
            .unwrap();
        assert!(QUERIES.contains(&q.as_str()), "{q}");
    }
}

#[tokio::test]
async fn non_finite_and_negative_values_are_dropped() {
    let n = |s: &str| format!("nemo_relay_resource_{s}");
    let answers = [
        vector(&[
            (
                res_labels(&n("cpu_consumption_rate_logical_processors")),
                "NaN",
            ),
            (res_labels(&n("memory_resident_kibibytes")), "+Inf"),
            (
                res_labels(&n("memory_environment_accounted_kibibytes")),
                "-Inf",
            ),
            (res_labels(&n("thread_count_threads")), "-1"),
            (
                res_labels(&n("process_open_file_descriptor_count_file_descriptors")),
                "not a number",
            ),
            (
                fs_labels(&n("disk_filesystem_available_capacity_bytes"), "/x"),
                "500",
            ),
            (
                fs_labels(&n("disk_filesystem_total_capacity_bytes"), "/x"),
                "100",
            ),
            (res_labels(&n("limit_event_count_events")), "NaN"),
        ]),
        vector(&[tagged("cpu", "NaN"), tagged("mem", "-0.5")]),
        vector(&[
            tagged("rps", "Inf"),
            tagged("fail", "-1"),
            tagged("tok", "nan"),
        ]),
        vector(&[(json!({}), "NaN")]),
    ];
    let prom = fake_prometheus(answers).await;
    let (points, _) = metrics_with(&prom.uri()).await;
    let got = by_key(&points);
    // only the free space survives: its used share would be negative (available > total)
    let extra: Vec<&String> = got
        .keys()
        .filter(|k| !BASE_KEYS.contains(&k.as_str()))
        .collect();
    assert_eq!(extra, ["relay.atof_fs_free"]);
    assert!(points.iter().all(|p| p.value.is_finite() && p.value >= 0.0));
}

#[tokio::test]
async fn prometheus_down_or_broken_omits_the_keys_and_keeps_the_snapshot() {
    let closed = switchyard_support::closed_url().await;
    let (points, available) = metrics_with(&closed).await;
    assert!(available);
    assert!(
        by_key(&points)
            .keys()
            .all(|k| BASE_KEYS.contains(&k.as_str()))
    );
    // 500, garbage, a failed answer and an oversized one
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/query"))
        .and(query_param("query", GAUGE_QUERY))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/query"))
        .and(query_param("query", PRESSURE_QUERY))
        .respond_with(ResponseTemplate::new(200).set_body_string("{not json"))
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/query"))
        .and(query_param("query", ROUTING_QUERY))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status": "error", "errorType": "bad_data", "error": "x"})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/api/v1/query"))
        .and(query_param("query", OVERHEAD_QUERY))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(" ".repeat(relay_runtime::PROM_MAX_BYTES + 10)),
        )
        .mount(&server)
        .await;
    let (points, available) = metrics_with(&server.uri()).await;
    assert!(available);
    assert!(
        by_key(&points)
            .keys()
            .all(|k| BASE_KEYS.contains(&k.as_str()))
    );
}

#[tokio::test]
async fn a_prometheus_url_off_this_machine_is_never_asked() {
    // TEST-NET-1: an attempt would wait for the connect timeout; refusing it is immediate
    let t0 = Instant::now();
    for url in [
        "http://192.0.2.1:9090",
        "http://example.org:9090",
        "http://user:pw@127.0.0.1:9",
        "file:///etc/passwd",
    ] {
        let (points, _) = metrics_with(url).await;
        assert!(
            by_key(&points)
                .keys()
                .all(|k| BASE_KEYS.contains(&k.as_str())),
            "{url}"
        );
    }
    assert!(t0.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn without_a_prometheus_url_nothing_is_asked_and_the_old_keys_are_unchanged() {
    // the existing tests' contexts have no `prometheus` URL: they stay hermetic and see only the old keys
    let r = relay_support::Router::new().await;
    let m = relay_support::make_adapter(&r, None, None, None, |_| {});
    let got = by_key(&m.a.metrics().await.unwrap().points);
    assert_eq!(
        got.keys().cloned().collect::<BTreeSet<_>>(),
        set(&BASE_KEYS)
    );
}

#[test]
fn the_queries_are_fixed_and_cover_the_older_histogram_name() {
    assert!(relay_runtime::queries_are_fixed());
    assert!(QUERIES.len() <= 6);
    assert!(OVERHEAD_QUERY.contains("switchyard_routing_overhead_milliseconds_bucket"));
    assert!(OVERHEAD_QUERY.contains("switchyard_routing_overhead_ms_bucket"));
    // freshness is not read from Prometheus (its sample timestamps are export times): no timestamp() query
    assert!(QUERIES.iter().all(|q| !q.contains("timestamp(")));
    // the failure rate is 0, not absent, while requests are counted and no failure has happened yet
    assert!(ROUTING_QUERY.contains(
        r#"sum(rate(switchyard_routing_failures_total{job="nemo-relay"}[5m])) or (sum(rate(switchyard_routing_requests_total{job="nemo-relay"}[5m])) * 0)"#
    ));
    // a scalar answer is read too; anything else is not an answer
    let scalar =
        json!({"status": "success", "data": {"resultType": "scalar", "result": [1.0, "2.5"]}});
    let rows = relay_runtime::prom_rows(scalar.to_string().as_bytes()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, 2.5);
    for bad in [
        "",
        "null",
        "[]",
        "{\"status\":\"success\"}",
        "{\"status\":\"success\",\"data\":{\"result\":7}}",
    ] {
        assert!(relay_runtime::prom_rows(bad.as_bytes()).is_none(), "{bad}");
    }
}

// ── the capture answer and the trace index ─────────────────────────────────────────────────────────

/// The lab's `resources on` component (ops/nemo-relay-levels.py `render_resources`), path replaced.
const RESOURCES_TOML: &str = r#"[[components]]
kind = "resource_metrics"
enabled = true

[components.config]
measurement_scope = "application_process"

[components.config.polling]
enabled = true
interval_millis = 60000

[components.config.disk]
enabled = true
process_io = true
filesystem_paths = ["/home/someone/.local/share/nemo-relay/atof"]

[components.config.gpu]
enabled = false

[components.config.network]
enabled = false
"#;

/// One resource sample mark like Relay 0.11.1 writes (many measurements, attributes that name a home path).
fn resource_mark(id: u64, secs: f64) -> String {
    let measurements: Vec<Value> = (0..120)
        .map(|i| {
            json!({"name": format!("nemo.relay.resource.field_{i}"), "kind": "gauge", "unit": "kibibytes",
                   "value": 44208 + i,
                   "attributes": {"nemo.relay.resource.disk.filesystem.path": "/home/someone/SENTINEL-PATH",
                                  "nemo.relay.resource.field": format!("SENTINELFIELD{i}")}})
        })
        .collect();
    json!({
        "atof_version": "0.1", "kind": "mark", "category": null, "category_profile": null,
        "name": "nemo.relay.resource_metrics", "uuid": relay_traces_support::uid(id),
        "parent_uuid": relay_traces_support::uid(9_999), "timestamp": at(secs),
        "data": {"measurements": measurements},
        "data_schema": {"name": "nemo.relay.metric_measurements", "version": "1"}, "metadata": null,
    })
    .to_string()
}

/// One small trace: an agent root with one routing mark.
fn one_trace() -> Vec<String> {
    vec![
        scope("start", "agent", "claude-code", 1, None, 10.0, json!({})),
        mark(
            "switchyard.routing.decision",
            50,
            Some(1),
            10.5,
            json!({"route": "r1"}),
        ),
        scope("end", "agent", "claude-code", 1, None, 12.0, json!({})),
    ]
}

/// Seconds after the lab's time base (2026-11-10) that are `ago` seconds before now.
fn secs_ago(ago: f64) -> f64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    now - ago - 1_794_268_800.0
}

fn lab_with(plugins: Option<&str>, lines: &[String]) -> Lab {
    let lab = Lab::empty(config());
    if let Some(p) = plugins {
        std::fs::write(lab.cfg.join("plugins.toml"), p).unwrap();
    }
    lab.write("relay-events.jsonl", lines);
    lab.refresh();
    lab
}

async fn capture(lab: &Lab) -> Value {
    lab.ask(Request::Capture).await.unwrap()
}

fn level<'a>(cap: &'a Value, id: &str) -> &'a Value {
    cap["levels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == id)
        .unwrap()
}

fn hint_codes(cap: &Value) -> Vec<String> {
    cap["hints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["code"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn resource_marks_stay_out_of_traces_sessions_analytics_and_orphans() {
    let mut with = one_trace();
    for i in 0..30 {
        with.push(resource_mark(100 + i, 11.0 + i as f64));
    }
    let plain = lab_with(None, &one_trace());
    let marked = lab_with(None, &with);
    let a = plain.index_state();
    let b = marked.index_state();
    for k in ["spans", "marks", "traces", "sessions", "open_spans"] {
        assert_eq!(a[k], b[k], "{k}");
    }
    assert_eq!(
        b["events"].as_u64().unwrap(),
        a["events"].as_u64().unwrap() + 30
    );
    // their newest time is kept for the capture view
    assert_eq!(
        marked.a.traces.snapshot().info.resource_last_us,
        Some((1_794_268_800 + 40) * 1_000_000)
    );
    // no viewer answer carries their payload, and the trace, session and analytics answers are the same as
    // without them
    let (sm, sp) = (
        relay_traces_support::app(&marked),
        relay_traces_support::app(&plain),
    );
    for sub in [
        "/traces?window=all",
        "/sessions?window=all",
        "/analytics?window=all",
        "/traces/index",
    ] {
        let x = relay_traces_support::ok(&sm, sub).await;
        let text = x.to_string();
        assert!(
            !text.contains("SENTINEL") && !text.contains("resource.field_"),
            "{sub}"
        );
        if sub != "/traces/index" {
            let y = relay_traces_support::ok(&sp, sub).await;
            assert_eq!(strip_times(x), strip_times(y), "{sub}");
        }
    }
}

/// An answer without the keys that say when it was built.
fn strip_times(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        for k in [
            "generated_at",
            "now",
            "last_refresh_ts",
            "generation",
            "build_ms",
        ] {
            o.remove(k);
        }
    }
    v
}

#[tokio::test]
async fn the_capture_answer_reports_a_fresh_sampler_as_active() {
    let mut lines = one_trace();
    lines.push(resource_mark(200, secs_ago(30.0)));
    let lab = lab_with(Some(RESOURCES_TOML), &lines);
    let cap = capture(&lab).await;
    assert_eq!(
        cap["resources"],
        json!({"configured": true, "enabled": true, "scope": "application_process", "interval_s": 60.0,
               "gpu": false, "network": false})
    );
    let row = level(&cap, "resource_metrics");
    assert_eq!(row["enabled"], true);
    assert_eq!(row["state"], "active");
    assert_eq!(row["group"], "other");
    assert_eq!(
        row["detail"],
        json!({"polling": true, "interval_ms": 60000})
    );
    assert!(row["observed"]["last_ts"].as_f64().unwrap() > 0.0);
    let codes = hint_codes(&cap);
    assert!(!codes.iter().any(|c| c == "relay.resources.off"));
    assert!(!codes.iter().any(|c| c == "capture.metrics.resource_off"));
    assert!(
        !cap.to_string().contains("/home/"),
        "no path of the config reaches the answer"
    );
    // the event-log row follows the traffic, not the sampler
    assert_ne!(
        level(&cap, "atof")["observed"]["last_ts"],
        row["observed"]["last_ts"]
    );
}

#[tokio::test]
async fn stale_idle_off_and_unknown_states() {
    // older than three intervals
    let lab = lab_with(Some(RESOURCES_TOML), &[resource_mark(1, secs_ago(181.0))]);
    assert_eq!(
        level(&capture(&lab).await, "resource_metrics")["state"],
        "stale"
    );
    // on, nothing seen
    let lab = lab_with(Some(RESOURCES_TOML), &one_trace());
    assert_eq!(
        level(&capture(&lab).await, "resource_metrics")["state"],
        "idle"
    );
    // no component: off, with the hint naming the command
    let lab = lab_with(
        Some("[[components]]\nkind = \"pii_redaction\"\n"),
        &[resource_mark(1, secs_ago(5.0))],
    );
    let cap = capture(&lab).await;
    let row = level(&cap, "resource_metrics");
    assert_eq!(
        (row["enabled"].clone(), row["state"].clone()),
        (json!(false), json!("off"))
    );
    assert_eq!(
        cap["resources"],
        json!({"configured": false, "enabled": false, "scope": null, "interval_s": null, "gpu": false,
               "network": false})
    );
    let hint = cap["hints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["code"] == "relay.resources.off")
        .unwrap();
    assert!(
        hint["message"]
            .as_str()
            .unwrap()
            .contains("python3 -I ops/nemo-relay-levels.py resources on")
    );
    // configured, polling off: off, interval null
    let toml = RESOURCES_TOML.replacen(
        "[components.config.polling]\nenabled = true",
        "[components.config.polling]\nenabled = false",
        1,
    );
    let cap = capture(&lab_with(Some(&toml), &[])).await;
    assert_eq!(cap["resources"]["configured"], true);
    assert_eq!(cap["resources"]["enabled"], false);
    assert_eq!(cap["resources"]["interval_s"], Value::Null);
    assert_eq!(level(&cap, "resource_metrics")["state"], "off");
    assert!(hint_codes(&cap).contains(&"relay.resources.off".to_owned()));
    // switched off (enabled = false) counts as not configured
    let toml = RESOURCES_TOML.replacen("enabled = true", "enabled = false", 1);
    let cap = capture(&lab_with(Some(&toml), &[])).await;
    assert_eq!(cap["resources"]["configured"], false);
    // plugins.toml missing: unknown, no hint
    let cap = capture(&lab_with(None, &[])).await;
    assert_eq!(level(&cap, "resource_metrics")["state"], "unknown");
    assert!(!hint_codes(&cap).contains(&"relay.resources.off".to_owned()));
    assert_eq!(cap["resources"]["configured"], Value::Null);
    // a scope that is not an identifier is not shown
    let toml = RESOURCES_TOML.replace("application_process", "bad scope; SENTINEL");
    let cap = capture(&lab_with(Some(&toml), &[])).await;
    assert_eq!(cap["resources"]["scope"], Value::Null);
    assert!(!cap.to_string().contains("SENTINEL"));
}

#[tokio::test]
async fn index_off_leaves_the_sampler_state_unknown() {
    let mut cfg: TraceConfig = config();
    cfg.enabled = false;
    let lab = Lab::empty(cfg);
    std::fs::write(lab.cfg.join("plugins.toml"), RESOURCES_TOML).unwrap();
    let cap = lab.ask(Request::Capture).await.unwrap();
    let row = level(&cap, "resource_metrics");
    assert_eq!(row["enabled"], true);
    assert_eq!(row["state"], "unknown");
    assert_eq!(row["observed"], json!({"last_ts": null}));
}

// ── diagnostics: snapshot pages (Relay starts a page at the OLDEST event; `continuation` names the next) ──────

const INSTANCE: &str = "00000000-0000-7000-8000-000000000000";

fn continuation(seq: u64) -> Value {
    json!({"source_instance_id": INSTANCE, "sequence": seq.to_string(), "component_offset": "23",
           "event_offset": "150", "scope": "whole"})
}

/// The fixture snapshot as page 1 of several: truncated, with a continuation.
fn first_page(next: &Value) -> Value {
    let mut s = fixture_json("snapshot.json");
    s["coverage"]["retention"]["query_truncated"] = json!(true);
    s["continuation"] = next.clone();
    s
}

/// A later page: `n` newer events from `from`, no components, the given continuation (null = last page).
fn later_page(from: u64, n: u64, next: &Value) -> Value {
    let mut s = fixture_json("snapshot.json");
    let template = s["events"][0].clone();
    s["events"] = json!(
        (from..from + n)
            .map(|q| {
                let mut e = template.clone();
                e["sequence"] = json!(q.to_string());
                e
            })
            .collect::<Vec<_>>()
    );
    s["components"] = json!([]);
    s["coverage"]["retention"]["query_truncated"] = json!(!next.is_null());
    s["continuation"] = next.clone();
    s
}

/// A runner that answers the k-th snapshot call with `pages[k]` (the last one again past the end) and `--memory`
/// with the memory fixture.
fn paged_runner(pages: Vec<CommandResult>) -> Arc<PrefixRunner> {
    let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mem = ok(&fixture("memory.json"));
    let r = PrefixRunner::new();
    r.on_fn(&["nemo-relay", "diagnostics"], move |a| {
        if a.iter().any(|x| x == "--memory") {
            return mem.clone();
        }
        let k = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        pages[k.min(pages.len() - 1)].clone()
    });
    r
}

fn page(v: &Value) -> CommandResult {
    ok(&v.to_string())
}

fn snapshot_calls(r: &PrefixRunner) -> Vec<Vec<String>> {
    r.calls()
        .into_iter()
        .filter(|a| !a.iter().any(|x| x == "--memory"))
        .collect()
}

#[tokio::test]
async fn two_pages_the_newest_events_win_and_the_continuation_goes_back_verbatim() {
    let c = continuation(187);
    let run = paged_runner(vec![
        page(&first_page(&c)),
        page(&later_page(188, 212, &Value::Null)),
    ]);
    let (a, _tmp) = diag_adapter(Arc::clone(&run));
    let v = a.relay_diagnostics().await.unwrap();
    assert_eq!(v["available"], true);
    let evs = v["events"].as_array().unwrap();
    assert_eq!(evs.len(), 256);
    assert_eq!(evs[0]["seq"], 399);
    assert_eq!(evs[255]["seq"], 144);
    // components and lifetime from page 1 (page 2 has none)
    assert_eq!(v["components"].as_array().unwrap().len(), 21);
    assert_eq!(v["totals"]["recorded"], 150);
    assert_eq!(
        v["retention"],
        json!({"events_retained": 362, "oldest_seq": 10, "latest_seq": 399, "truncated": false})
    );
    assert_eq!(v["memory"]["reserved_bytes"], 2_595_089);
    // the continuation object never reaches the answer
    let text = v.to_string();
    assert!(!text.contains("continuation") && !text.contains("event_offset"));
    assert_clean(&v);
    let calls = snapshot_calls(&run);
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1],
        [
            "nemo-relay",
            "diagnostics",
            "--gateway-url=http://127.0.0.1:9",
            "--max-response-bytes",
            "1048576",
            "--query",
            r#"{"continuation":{"source_instance_id":"00000000-0000-7000-8000-000000000000","sequence":"187","component_offset":"23","event_offset":"150","scope":"whole"}}"#,
        ]
    );
    assert_eq!(run.calls().len(), 3, "two pages and the memory call");
}

#[tokio::test]
async fn a_continuation_that_comes_back_again_stops_the_reading() {
    let c = continuation(187);
    // page 2 answers with the same continuation as page 1 (and the same events): a loop
    let run = paged_runner(vec![page(&first_page(&c))]);
    let (a, _tmp) = diag_adapter(Arc::clone(&run));
    let v = a.relay_diagnostics().await.unwrap();
    assert_eq!(snapshot_calls(&run).len(), 2);
    assert_eq!(v["retention"]["truncated"], true);
    // the events read twice count once
    assert_eq!(v["events"].as_array().unwrap().len(), 150);
    assert_eq!(v["retention"]["events_retained"], 150);
}

#[tokio::test]
async fn at_most_four_pages_are_read() {
    let pages: Vec<CommandResult> = (0..10u64)
        .map(|k| {
            let next = continuation(200 + k * 10);
            if k == 0 {
                page(&first_page(&next))
            } else {
                page(&later_page(188 + (k - 1) * 10, 10, &next))
            }
        })
        .collect();
    let run = paged_runner(pages);
    let (a, _tmp) = diag_adapter(Arc::clone(&run));
    let v = a.relay_diagnostics().await.unwrap();
    assert_eq!(snapshot_calls(&run).len(), relay_runtime::DIAG_MAX_PAGES);
    assert_eq!(v["retention"]["truncated"], true);
    assert_eq!(v["events"][0]["seq"], 188 + 3 * 10 - 1);
}

#[tokio::test]
async fn a_failing_second_page_keeps_the_first_and_says_truncated() {
    let run = paged_runner(vec![
        page(&first_page(&continuation(187))),
        CommandResult::new(1, "", "launcher error: SENTINEL boom"),
    ]);
    let (a, _tmp) = diag_adapter(Arc::clone(&run));
    let v = a.relay_diagnostics().await.unwrap();
    assert_eq!(v["available"], true);
    assert_eq!(v["reason"], Value::Null);
    assert_eq!(v["events"].as_array().unwrap().len(), 150);
    assert_eq!(v["retention"]["truncated"], true);
    assert!(!v["memory"].is_null(), "the memory call still runs");
    assert_clean(&v);
    // garbage on page 2 is the same
    let run = paged_runner(vec![page(&first_page(&continuation(187))), ok("{not json")]);
    let (a, _tmp) = diag_adapter(run);
    let v = a.relay_diagnostics().await.unwrap();
    assert_eq!(
        (v["available"].clone(), v["retention"]["truncated"].clone()),
        (json!(true), json!(true))
    );
}

#[tokio::test]
async fn an_oversized_or_odd_continuation_is_never_sent() {
    let mut big = continuation(187);
    big["scope"] = json!("x".repeat(3100));
    // within 3 KiB raw, but past 4096 URI-encoded bytes
    let mut quoted = continuation(187);
    quoted["scope"] = json!("{}".repeat(700));
    for next in [big, quoted, json!("a string"), json!(["a", "list"])] {
        let run = paged_runner(vec![page(&first_page(&next))]);
        let (a, _tmp) = diag_adapter(Arc::clone(&run));
        let v = a.relay_diagnostics().await.unwrap();
        assert_eq!(snapshot_calls(&run).len(), 1, "{next:.60}");
        assert_eq!(v["retention"]["truncated"], true);
        assert_clean(&v);
    }
}

#[test]
fn next_query_is_the_compact_object_and_respects_the_cli_cap() {
    let p = first_page(&continuation(5));
    let q = relay_runtime::next_query(&p).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&q).unwrap(),
        json!({"continuation": continuation(5)})
    );
    assert!(!q.contains(' ') && !q.contains('\n'));
    let mut last = fixture_json("snapshot.json");
    assert!(
        relay_runtime::next_query(&last).is_none(),
        "continuation null"
    );
    last.as_object_mut().unwrap().remove("continuation");
    assert!(relay_runtime::next_query(&last).is_none());
    assert!(!relay_runtime::page_truncated(&last));
}

// ── audit fixes (F1-F10) ──────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_zero_failure_rate_is_a_point() {
    let mut answers = good_answers();
    answers[2] = vector(&[
        tagged("rps", "0.5"),
        tagged("fail", "0"),
        tagged("tok", "0"),
    ]);
    let prom = fake_prometheus(answers).await;
    let got = by_key(&metrics_with(&prom.uri()).await.0);
    assert_eq!(got["relay.route_failures"], (0.0, "req/s".to_owned()));
    assert!(!got.contains_key("relay.resource_age"));
}

#[tokio::test]
async fn the_points_are_cached_for_many_polls_and_refreshed_once_at_a_time() {
    let server = MockServer::start().await;
    for (q, a) in QUERIES.iter().zip(good_answers()) {
        Mock::given(path("/api/v1/query"))
            .and(query_param("query", *q))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(a)
                    .set_delay(Duration::from_millis(200)),
            )
            .mount(&server)
            .await;
    }
    let r = relay_support::Router::new().await;
    let m = relay_support::make_adapter(&r, None, None, None, |_| {});
    let mut c = m.a.ctx.clone();
    c.urls.insert("prometheus".to_owned(), server.uri());
    let a = Arc::new(RelayAdapter::new(c));
    // concurrent first polls: one refresh
    let tasks: Vec<_> = (0..5)
        .map(|_| {
            let a = Arc::clone(&a);
            tokio::spawn(async move { a.metrics().await.unwrap().points })
        })
        .collect();
    for t in tasks {
        let pts = t.await.unwrap();
        assert!(by_key(&pts).contains_key("relay.gw_rss"));
    }
    // later polls within 15 s: no new queries
    for _ in 0..10 {
        assert!(by_key(&a.metrics().await.unwrap().points).contains_key("relay.gw_rss"));
    }
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        QUERIES.len()
    );
}

#[tokio::test]
async fn a_slow_prometheus_costs_at_most_the_query_timeout() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/query"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
        .mount(&server)
        .await;
    let t0 = Instant::now();
    let (points, available) = metrics_with(&server.uri()).await;
    assert!(available);
    assert!(
        by_key(&points)
            .keys()
            .all(|k| BASE_KEYS.contains(&k.as_str()))
    );
    assert!(t0.elapsed() < Duration::from_secs(4), "{:?}", t0.elapsed());
}

const OBS_TOML: &str = "[[components]]\nkind = \"observability\"\nenabled = true\n\n[components.config.atof]\nenabled = true\n\n";

#[tokio::test]
async fn the_sampler_alone_does_not_make_an_idle_capture_look_active() {
    // traffic on 2026-11-10 (days old), a resource sample 30 s ago
    let mut lines = one_trace();
    lines.push(resource_mark(300, secs_ago(30.0)));
    let lab = lab_with(Some(&format!("{OBS_TOML}{RESOURCES_TOML}")), &lines);
    let cap = capture(&lab).await;
    assert_eq!(level(&cap, "atof")["enabled"], true);
    assert_ne!(level(&cap, "atof")["state"], "active");
    assert_ne!(level(&cap, "payloads")["state"], "active");
    assert_eq!(level(&cap, "resource_metrics")["state"], "active");
    let traffic = level(&cap, "atof")["observed"]["last_ts"].as_f64().unwrap();
    assert!((traffic - relay_traces_support::at_secs(12.0)).abs() < 1e-3);
}

/// A runner whose snapshot calls answer `pages[k]` in turn, `None` = never answers (it hangs).
struct HangRunner {
    pages: Vec<Option<CommandResult>>,
    n: std::sync::atomic::AtomicUsize,
    calls: std::sync::Mutex<Vec<Vec<String>>>,
}

#[async_trait::async_trait]
impl CommandRunner for HangRunner {
    async fn run(
        &self,
        argv: &[String],
        _opts: nemo_console::adapters::base::RunOpts,
    ) -> Result<CommandResult, nemo_console::adapters::base::RunnerError> {
        self.calls.lock().unwrap().push(argv.to_vec());
        if argv.iter().any(|x| x == "--memory") {
            return Ok(ok(&fixture("memory.json")));
        }
        let k = self.n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match self.pages.get(k).cloned().flatten() {
            Some(r) => Ok(r),
            None => {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(ok(""))
            }
        }
    }
}

fn hang_runner(pages: Vec<Option<CommandResult>>) -> Arc<HangRunner> {
    Arc::new(HangRunner {
        pages,
        n: std::sync::atomic::AtomicUsize::new(0),
        calls: std::sync::Mutex::new(Vec::new()),
    })
}

#[tokio::test]
async fn the_whole_read_has_a_deadline() {
    // page 1 never answers: unreachable, quickly
    let run = hang_runner(vec![None]);
    let (a, _tmp) = diag_adapter_with(
        run.clone(),
        "http://127.0.0.1:9",
        Duration::from_millis(400),
    );
    let t0 = Instant::now();
    let v = a.relay_diagnostics().await.unwrap();
    assert!(t0.elapsed() < Duration::from_secs(3));
    assert_eq!(
        (v["available"].clone(), v["reason"].clone()),
        (json!(false), json!("unreachable"))
    );
    // page 2 never answers: page 1's data, still truncated; the memory call is not made past the deadline
    let run = hang_runner(vec![Some(page(&first_page(&continuation(187)))), None]);
    let (a, _tmp) = diag_adapter_with(
        run.clone(),
        "http://127.0.0.1:9",
        Duration::from_millis(400),
    );
    let t0 = Instant::now();
    let v = a.relay_diagnostics().await.unwrap();
    assert!(t0.elapsed() < Duration::from_secs(3));
    assert_eq!(v["available"], true);
    assert_eq!(v["events"].as_array().unwrap().len(), 150);
    assert_eq!(v["retention"]["truncated"], true);
    assert_eq!(v["memory"], Value::Null);
    assert!(
        !run.calls
            .lock()
            .unwrap()
            .iter()
            .any(|a| a.iter().any(|x| x == "--memory"))
    );
}

#[tokio::test]
async fn a_gateway_url_off_this_machine_is_never_handed_to_the_cli() {
    for url in [
        "http://192.0.2.1:4040",
        "http://example.org:4040",
        "http://user:pw@127.0.0.1:4040",
        "file:///tmp/x",
    ] {
        let run = fixture_runner();
        let (a, _tmp) = diag_adapter_with(run.clone(), url, Duration::from_secs(30));
        let v = a.relay_diagnostics().await.unwrap();
        assert_eq!(
            (v["available"].clone(), v["reason"].clone()),
            (json!(false), json!("error")),
            "{url}"
        );
        assert!(run.calls().is_empty(), "{url}");
    }
    // localhost and [::1] are this machine
    for url in ["http://localhost:4040", "http://[::1]:4040/"] {
        let run = fixture_runner();
        let (a, _tmp) = diag_adapter_with(run.clone(), url, Duration::from_secs(30));
        assert_eq!(
            a.relay_diagnostics().await.unwrap()["available"],
            true,
            "{url}"
        );
    }
}

#[tokio::test]
async fn cross_site_requests_are_refused_before_the_cli_runs() {
    let run = fixture_runner();
    let (a, tmp) = diag_adapter(Arc::clone(&run));
    let st = app_for(&a, run.clone(), tmp.path());
    let req = |hdrs: &[(&str, &str)]| {
        let mut r = common::TestReq::new(Arc::clone(&st), "GET", DIAG);
        for (k, v) in hdrs {
            r = r.header(k, v);
        }
        r
    };
    for hdrs in [
        vec![("sec-fetch-site", "cross-site")],
        vec![("sec-fetch-site", "same-site")],
        vec![("Sec-Fetch-Site", "Cross-Site")],
        vec![("origin", "http://evil.example")],
        vec![("origin", "null")],
        vec![("origin", "http://127.0.0.1:1")],
        vec![
            ("sec-fetch-site", "same-origin"),
            ("origin", "http://evil.example"),
        ],
    ] {
        let r = req(&hdrs).send().await;
        assert_eq!(
            (r.status, r.code()),
            (403, "cross_origin".to_owned()),
            "{hdrs:?}"
        );
    }
    assert!(run.calls().is_empty(), "nothing ran");
    let host = common::BASE_HOST;
    for hdrs in [
        vec![],
        vec![("sec-fetch-site", "same-origin")],
        vec![("sec-fetch-site", "none")],
        vec![("origin", &*format!("http://{host}"))],
    ] {
        let r = req(&hdrs).send().await;
        assert_eq!(r.status, 200, "{hdrs:?}: {}", r.text());
    }
}
