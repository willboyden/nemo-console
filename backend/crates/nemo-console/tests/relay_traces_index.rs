//! The Relay trace index (addendum 9.12): how events become spans, traces and sessions, how a refresh reads only
//! what is new (appends, copytruncate, rotations, gzip), the bounds, symlink refusal and trajectory ids.
//! Hermetic: every capture is written into a temp dir by the test; nothing is read from the real home.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
mod relay_traces_support;

use std::sync::Arc;

use nemo_console::container::AppState;
use relay_traces_support::{
    API, Lab, app, at_secs, config, get, gzip_stored, llm_pair, ok, scope, traj_doc, uid,
};
use serde_json::{Value, json};

fn find<'a>(items: &'a [Value], key: &str, id: &str) -> Option<&'a Value> {
    items.iter().find(|v| v[key] == id)
}

fn span(detail: &Value, id: u64) -> &Value {
    find(detail["spans"].as_array().unwrap(), "id", &uid(id))
        .unwrap_or_else(|| panic!("span {} not in {detail}", uid(id)))
}

fn file_row<'a>(state: &'a Value, name: &str) -> &'a Value {
    find(state["files"].as_array().unwrap(), "name", name)
        .unwrap_or_else(|| panic!("{name} not in {}", state["files"]))
}

fn has_problem(state: &Value, code: &str) -> bool {
    state["problems"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["code"] == code)
}

/// All trace ids in the index (window `all`).
async fn trace_ids(st: &Arc<AppState>) -> Vec<String> {
    let v = ok(st, "/traces?window=all&limit=200").await;
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["trace_id"].as_str().unwrap().to_owned())
        .collect()
}

/// One small trace: a root agent scope `id` with one tool span `id + 1`, starting at `secs`.
fn small_trace(id: u64, secs: f64) -> Vec<String> {
    vec![
        scope("start", "agent", "agent", id, None, secs, json!({})),
        scope(
            "start",
            "tool",
            "Read",
            id + 1,
            Some(id),
            secs + 0.1,
            json!({}),
        ),
        scope(
            "end",
            "tool",
            "Read",
            id + 1,
            Some(id),
            secs + 0.2,
            json!({}),
        ),
        scope("end", "agent", "agent", id, None, secs + 0.3, json!({})),
    ]
}

/// `n` small traces, one a minute, ids `base`, `base + 10`, ...
fn many_traces(base: u64, n: u64) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..n {
        #[allow(clippy::cast_precision_loss)]
        out.extend(small_trace(base + i * 10, i as f64 * 60.0));
    }
    out
}

// ── pairing, grouping, sessions, client ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_scopes_pair_by_uuid_into_spans_and_group_into_traces() {
    let lab = Lab::empty(config());
    let md = json!({"metadata": {"agent_kind": "claude-code", "session_id": "relay-sess-1"}});
    let mut lines = vec![
        scope("start", "agent", "claude-code", 1, None, 0.0, md.clone()),
        // an end without its start: kept, start unknown
        scope("end", "tool", "Bash", 4, Some(1), 5.0, json!({})),
    ];
    lines.extend(llm_pair(
        2,
        Some(1),
        1.0,
        2.0,
        json!({"user-agent": "opencode/2.0.0 (linux)", "x-opencode-session-id": "ses_client_a"}),
        json!({"model": "local-small", "messages": [{"role": "user", "content": "hello"}]}),
        json!({"model": "local-small", "choices": [{"finish_reason": "stop", "message": {"role": "assistant", "content": "hi"}}],
               "usage": {"prompt_tokens": 12, "completion_tokens": 3, "total_tokens": 15}}),
    ));
    lines.extend([
        // a start without its end: an open span
        scope("start", "tool", "Read", 3, Some(1), 3.0, json!({})),
        scope("end", "agent", "claude-code", 1, None, 10.0, md),
        // a parent that is not in the index: this span is the root of its own (open) trace
        scope("start", "agent", "orphan", 5, Some(99), 20.0, json!({})),
    ]);
    lab.write("relay-events.jsonl", &lines);
    lab.refresh();
    let st = app(&lab);

    let ids = trace_ids(&st).await;
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(ids.contains(&uid(1)) && ids.contains(&uid(5)));

    let d = ok(&st, &format!("/traces/{}", uid(1))).await;
    assert_eq!(d["spans"].as_array().unwrap().len(), 4, "{d}");
    // start and end of the same uuid became ONE span
    let root = span(&d, 1);
    assert_eq!(root["start_ts"], json!(at_secs(0.0)));
    assert_eq!(root["end_ts"], json!(at_secs(10.0)));
    assert_eq!(root["parent_id"], Value::Null);
    let llm = span(&d, 2);
    assert_eq!(llm["status"], "ok");
    assert_eq!(llm["parent_id"], json!(uid(1)));
    assert_eq!(llm["llm"]["input_tokens"], 12);
    assert_eq!(llm["llm"]["output_tokens"], 3);
    assert_eq!(llm["llm"]["model"], "local-small");
    let open = span(&d, 3);
    assert_eq!(open["status"], "open");
    assert_eq!(open["end_ts"], Value::Null);
    assert_eq!(open["start_ts"], json!(at_secs(3.0)));
    let end_only = span(&d, 4);
    assert_eq!(end_only["start_ts"], Value::Null);
    assert_eq!(end_only["end_ts"], json!(at_secs(5.0)));

    let t = &d["trace"];
    assert_eq!(t["trace_id"], json!(uid(1)));
    assert_eq!(t["root_name"], "claude-code");
    // the root ended and nothing failed: ok, even though a child is still open
    assert_eq!(t["status"], "ok");
    assert_eq!(t["spans"], 4);
    assert_eq!(t["llm_calls"], 1);
    assert_eq!(t["kind"], "claude-code");
    assert_eq!(t["client"], "opencode");
    assert_eq!(t["session_key"], "ses_client_a");
    assert_eq!(t["relay_session_id"], "relay-sess-1");

    let orphan = ok(&st, &format!("/traces/{}", uid(5))).await;
    assert_eq!(orphan["trace"]["status"], "open");
    assert_eq!(orphan["trace"]["kind"], "unknown");
    assert_eq!(span(&orphan, 5)["parent_id"], Value::Null);

    let idx = lab.index_state();
    assert_eq!(idx["state"], "ready");
    assert_eq!(idx["traces"], 2);
    assert_eq!(idx["spans"], 5);
    assert_eq!(idx["open_spans"], 2);
}

#[tokio::test]
async fn test_session_key_precedence_and_client_from_user_agent() {
    let lab = Lab::empty(config());
    let req = json!({"model": "m", "messages": [{"role": "user", "content": "x"}]});
    let resp = json!({"choices": [{"finish_reason": "stop", "message": {"content": "y"}}]});
    let mut lines = Vec::new();
    // trace 10: all three client headers and a Relay session: the opencode header wins
    lines.push(scope(
        "start",
        "agent",
        "a",
        10,
        None,
        0.0,
        json!({"metadata": {"session_id": "relay-a"}}),
    ));
    lines.extend(llm_pair(
        11,
        Some(10),
        0.1,
        0.2,
        json!({"user-agent": "claude-cli/1.0.80 (external, cli)", "x-claude-code-session-id": "ses_cc",
               "x-session-id": "ses_plain", "x-opencode-session-id": "ses_oc"}),
        req.clone(),
        resp.clone(),
    ));
    // trace 20: x-session-id beats x-claude-code-session-id
    lines.push(scope("start", "agent", "a", 20, None, 1.0, json!({})));
    lines.extend(llm_pair(
        21,
        Some(20),
        1.1,
        1.2,
        json!({"x-claude-code-session-id": "ses_cc2", "x-session-id": "ses_plain2", "user-agent": "(compatible) x"}),
        req.clone(),
        resp.clone(),
    ));
    // trace 30: no client header: Relay's metadata.session_id
    lines.push(scope(
        "start",
        "agent",
        "a",
        30,
        None,
        2.0,
        json!({"metadata": {"session_id": "relay-c", "agent_kind": "gateway"}}),
    ));
    lines.extend(llm_pair(
        31,
        Some(30),
        2.1,
        2.2,
        json!({"user-agent": "opencode"}),
        req.clone(),
        resp.clone(),
    ));
    // trace 40: nothing at all: the trace id
    lines.push(scope("start", "agent", "a", 40, None, 3.0, json!({})));
    lines.extend(llm_pair(41, Some(40), 3.1, 3.2, json!({}), req, resp));
    lab.write("relay-events.jsonl", &lines);
    lab.refresh();
    let st = app(&lab);

    let a = ok(&st, &format!("/traces/{}", uid(10))).await["trace"].clone();
    assert_eq!(a["session_key"], "ses_oc");
    assert_eq!(a["relay_session_id"], "relay-a");
    assert_eq!(a["client"], "claude-cli");
    let b = ok(&st, &format!("/traces/{}", uid(20))).await["trace"].clone();
    assert_eq!(b["session_key"], "ses_plain2");
    // a user-agent whose product token is not identifier-shaped gives no client
    assert_eq!(b["client"], Value::Null);
    let c = ok(&st, &format!("/traces/{}", uid(30))).await["trace"].clone();
    assert_eq!(c["session_key"], "relay-c");
    assert_eq!(c["client"], "opencode");
    assert_eq!(c["kind"], "gateway");
    let d = ok(&st, &format!("/traces/{}", uid(40))).await["trace"].clone();
    assert_eq!(d["session_key"], json!(uid(40)));
    assert_eq!(d["client"], Value::Null);
    assert_eq!(d["kind"], "unknown");

    // the sessions list is keyed the same way
    let s = ok(&st, "/sessions?window=all").await;
    let keys: Vec<&str> = s["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["session_key"].as_str().unwrap())
        .collect();
    for k in ["ses_oc", "ses_plain2", "relay-c", uid(40).as_str()] {
        assert!(keys.contains(&k), "{k} not in {keys:?}");
    }
    let one = ok(&st, "/sessions/ses_oc").await;
    assert_eq!(one["session"]["traces"], 1);
    assert_eq!(one["traces"][0]["trace_id"], json!(uid(10)));
    assert_eq!(one["calls"][0]["span_id"], json!(uid(11)));
}

// ── incremental refresh ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_refresh_reads_appended_lines() {
    let lab = Lab::empty(config());
    lab.write("relay-events.jsonl", &small_trace(100, 0.0));
    lab.refresh();
    assert_eq!(lab.index_state()["events"], 4);
    let mut more = small_trace(200, 10.0).join("\n");
    more.push('\n');
    lab.append("relay-events.jsonl", &more);
    lab.refresh();
    let idx = lab.index_state();
    assert_eq!(idx["events"], 8, "{idx}");
    assert_eq!(file_row(&idx, "relay-events.jsonl")["events"], 8);
    assert_eq!(trace_ids(&app(&lab)).await.len(), 2);
    // a pass with nothing new reads nothing again
    lab.refresh();
    assert_eq!(lab.index_state()["events"], 8);
}

#[tokio::test]
async fn test_refresh_after_copytruncate_rereads_from_zero_and_skips_the_rotated_copy() {
    let lab = Lab::empty(config());
    lab.write("relay-events.jsonl", &small_trace(100, 0.0));
    lab.refresh();
    assert_eq!(lab.index_state()["events"], 4);
    // logrotate copytruncate: copy to .1 (same first 4 KiB, same size, new name), then truncate the live file
    std::fs::copy(
        lab.atof.join("relay-events.jsonl"),
        lab.atof.join("relay-events.jsonl.1"),
    )
    .unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(lab.atof.join("relay-events.jsonl"))
        .unwrap()
        .set_len(0)
        .unwrap();
    // the live file then gets new content, SHORTER than before (it shrank)
    lab.append(
        "relay-events.jsonl",
        &(small_trace(200, 10.0)[..2].join("\n") + "\n"),
    );
    lab.refresh();
    let idx = lab.index_state();
    // 4 old events once (the copy is not read again) + 2 new ones read from offset 0
    assert_eq!(idx["events"], 6, "{idx}");
    assert_eq!(file_row(&idx, "relay-events.jsonl")["events"], 2);
    let ids = trace_ids(&app(&lab)).await;
    assert!(
        ids.contains(&uid(100)) && ids.contains(&uid(200)),
        "{ids:?}"
    );
    assert_eq!(ids.len(), 2);
}

#[tokio::test]
async fn test_a_rotated_copy_under_a_new_name_is_not_counted_twice() {
    let lab = Lab::empty(config());
    lab.write("relay-events.jsonl", &small_trace(100, 0.0));
    lab.refresh();
    std::fs::rename(
        lab.atof.join("relay-events.jsonl"),
        lab.atof.join("relay-events.jsonl.1"),
    )
    .unwrap();
    // the same content once more, as a gzip rotation
    std::fs::write(
        lab.atof.join("relay-events.jsonl.2.gz"),
        gzip_stored(&std::fs::read(lab.atof.join("relay-events.jsonl.1")).unwrap()),
    )
    .unwrap();
    lab.write("relay-events.jsonl", &[]);
    lab.refresh();
    let idx = lab.index_state();
    assert_eq!(idx["events"], 4, "{idx}");
    assert_eq!(idx["spans"], 2);
    assert_eq!(trace_ids(&app(&lab)).await.len(), 1);
}

#[tokio::test]
async fn test_plain_and_gzip_rotations_are_both_read() {
    let lab = Lab::empty(config());
    let mut old = small_trace(300, 0.0).join("\n");
    old.push('\n');
    std::fs::write(
        lab.atof.join("relay-events.jsonl.2.gz"),
        gzip_stored(old.as_bytes()),
    )
    .unwrap();
    lab.write("relay-events.jsonl.1", &small_trace(200, 10.0));
    lab.write("relay-events.jsonl", &small_trace(100, 20.0));
    // names that are not ATOF captures are not read
    lab.write("notes.txt", &small_trace(400, 30.0));
    lab.write("relay-events.jsonl.bak", &small_trace(500, 40.0));
    lab.refresh();
    let idx = lab.index_state();
    assert_eq!(idx["events"], 12, "{idx}");
    let gz = file_row(&idx, "relay-events.jsonl.2.gz");
    assert_eq!(gz["compressed"], true);
    assert_eq!(gz["events"], 4);
    assert_eq!(gz["state"], "indexed");
    assert_eq!(file_row(&idx, "relay-events.jsonl.1")["events"], 4);
    assert_eq!(idx["files"].as_array().unwrap().len(), 3, "{idx}");
    let ids = trace_ids(&app(&lab)).await;
    for id in [100, 200, 300] {
        assert!(ids.contains(&uid(id)), "{id} not in {ids:?}");
    }
    assert_eq!(ids.len(), 3);
}

#[tokio::test]
async fn test_a_truncated_gzip_rotation_yields_what_it_could_decode() {
    let lab = Lab::empty(config());
    let mut text = String::new();
    for (i, id) in [100_u64, 200, 300].iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        for l in small_trace(*id, i as f64 * 10.0) {
            text.push_str(&l);
            text.push('\n');
        }
    }
    let gz = gzip_stored(text.as_bytes());
    // still being compressed: the trailer and the end of the data are missing (stored blocks keep the data in
    // clear, so the cut lands inside the last trace's lines)
    let cut = gz.len() - 8 - 200;
    std::fs::write(lab.atof.join("relay-events.jsonl.1.gz"), &gz[..cut]).unwrap();
    lab.refresh();
    let idx = lab.index_state();
    let row = file_row(&idx, "relay-events.jsonl.1.gz");
    assert_eq!(row["state"], "partial", "{row}");
    let events = row["events"].as_u64().unwrap();
    assert!((8..12).contains(&events), "{row}");
    let ids = trace_ids(&app(&lab)).await;
    assert!(
        ids.contains(&uid(100)) && ids.contains(&uid(200)),
        "{ids:?}"
    );

    // the same with a rotation larger than its 4 KiB head (several stored blocks, the last one cut)
    let lab = Lab::empty(config());
    let mut text = many_traces(10_000, 200).join("\n");
    text.push('\n');
    let mut gz = Vec::new();
    // one gzip member per chunk: a cut in the last member leaves the earlier ones whole
    for part in text.as_bytes().chunks(30_000) {
        gz.extend_from_slice(&gzip_stored(part));
    }
    let cut = gz.len() - 8 - 500;
    std::fs::write(lab.atof.join("relay-events.jsonl.2.gz"), &gz[..cut]).unwrap();
    lab.refresh();
    let idx = lab.index_state();
    let row = file_row(&idx, "relay-events.jsonl.2.gz");
    assert_eq!(row["state"], "partial", "{row}");
    let events = row["events"].as_u64().unwrap();
    assert!((790..800).contains(&events), "{row}");
    let ids = trace_ids(&app(&lab)).await;
    assert!(ids.contains(&uid(10_000)), "the first trace is there");
}

// ── bounds ────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_a_line_over_the_line_limit_is_skipped_and_counted() {
    let mut cfg = config();
    cfg.limits.max_line_bytes = 64 * 1024;
    let lab = Lab::empty(cfg);
    let mut lines = small_trace(100, 0.0);
    let huge = scope(
        "start",
        "tool",
        "Big",
        150,
        Some(100),
        0.15,
        json!({"data": {"blob": "x".repeat(70 * 1024)}}),
    );
    lines.insert(2, huge);
    lab.write("relay-events.jsonl", &lines);
    lab.refresh();
    let idx = lab.index_state();
    let row = file_row(&idx, "relay-events.jsonl");
    assert_eq!(row["skipped_lines"], 1, "{row}");
    assert_eq!(row["events"], 4);
    assert!(has_problem(&idx, "relay.traces.lines_skipped"), "{idx}");
    // the lines after the long one were read
    let d = ok(&app(&lab), &format!("/traces/{}", uid(100))).await;
    assert_eq!(d["trace"]["status"], "ok");
    assert_eq!(d["spans"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_the_span_limit_drops_the_oldest_traces() {
    let mut cfg = config();
    cfg.limits.max_spans = 10;
    let lab = Lab::empty(cfg);
    lab.write("relay-events.jsonl", &many_traces(1000, 20));
    lab.refresh();
    let idx = lab.index_state();
    assert_eq!(idx["truncated"], true, "{idx}");
    assert!(idx["spans"].as_u64().unwrap() <= 10, "{idx}");
    assert!(has_problem(&idx, "relay.traces.span_limit"), "{idx}");
    let ids = trace_ids(&app(&lab)).await;
    assert!(!ids.is_empty());
    assert!(
        ids.contains(&uid(1190)),
        "the newest trace is kept: {ids:?}"
    );
    assert!(
        !ids.contains(&uid(1000)),
        "the oldest trace is gone: {ids:?}"
    );
    // what is kept is the newest ones, without a gap
    let kept: Vec<u64> = (0..20)
        .map(|i| 1000 + i * 10)
        .filter(|id| ids.contains(&uid(*id)))
        .collect();
    assert_eq!(kept.len(), ids.len());
    assert!(kept.windows(2).all(|w| w[1] == w[0] + 10), "{kept:?}");
}

#[tokio::test]
async fn test_the_scan_budget_reads_the_newest_file_first() {
    let mk = |budget: u64| {
        let mut cfg = config();
        cfg.limits.scan_bytes = budget;
        let lab = Lab::empty(cfg);
        lab.write("relay-events.jsonl.1", &many_traces(1000, 10));
        lab.write("relay-events.jsonl", &small_trace(5000, 10_000.0));
        lab
    };
    let size = |lab: &Lab, name: &str| std::fs::metadata(lab.atof.join(name)).unwrap().len();

    // the budget covers the live file and half of the rotation: the rotation is read from its tail, in part
    let probe = mk(1);
    let budget = size(&probe, "relay-events.jsonl") + size(&probe, "relay-events.jsonl.1") / 2;
    let lab = mk(budget);
    lab.refresh();
    let idx = lab.index_state();
    assert_eq!(idx["truncated"], true, "{idx}");
    assert_eq!(file_row(&idx, "relay-events.jsonl")["state"], "indexed");
    let rot = file_row(&idx, "relay-events.jsonl.1");
    assert_eq!(rot["state"], "partial", "{rot}");
    let ids = trace_ids(&app(&lab)).await;
    assert!(ids.contains(&uid(5000)), "{ids:?}");
    // the newest traces of the rotation are there, the oldest are not
    assert!(ids.contains(&uid(1090)), "{ids:?}");
    assert!(!ids.contains(&uid(1000)), "{ids:?}");

    // a budget smaller than the live file: the live file is still read, the rotation is left out
    let lab = mk(1);
    lab.refresh();
    let idx = lab.index_state();
    assert_eq!(file_row(&idx, "relay-events.jsonl")["state"], "indexed");
    assert_eq!(file_row(&idx, "relay-events.jsonl.1")["state"], "skipped");
    assert_eq!(idx["truncated"], true);
    assert!(has_problem(&idx, "relay.traces.scan_budget"), "{idx}");
    assert_eq!(trace_ids(&app(&lab)).await, vec![uid(5000)]);
}

// ── symlinks ──────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_symlinked_captures_and_trajectories_are_not_read() {
    let lab = Lab::empty(config());
    let outside = lab.tmp.path().join("outside");
    std::fs::create_dir_all(outside.join("atif")).unwrap();
    // a capture file outside the directory, linked in under a capture name
    let mut text = small_trace(900, 0.0).join("\n");
    text.push('\n');
    std::fs::write(outside.join("real.jsonl"), text).unwrap();
    std::os::unix::fs::symlink(
        outside.join("real.jsonl"),
        lab.atof.join("relay-events.jsonl.1"),
    )
    .unwrap();
    lab.write("relay-events.jsonl", &small_trace(100, 10.0));
    // a trajectory linked into the gateway directory
    let doc = traj_doc("01a0f1e0-aaaa-7000-8000-000000000001", "linked");
    std::fs::write(outside.join("t.json"), doc.to_string()).unwrap();
    std::os::unix::fs::symlink(outside.join("t.json"), lab.atif.join("linked.json")).unwrap();
    // a whole agent directory linked into the Hermes one, and an agent whose atif/ is a link
    std::fs::write(outside.join("atif").join("t2.json"), doc.to_string()).unwrap();
    std::os::unix::fs::symlink(&outside, lab.hermes.join("linkedagent")).unwrap();
    std::fs::create_dir_all(lab.hermes.join("agentx")).unwrap();
    std::os::unix::fs::symlink(outside.join("atif"), lab.hermes.join("agentx").join("atif"))
        .unwrap();
    // and a trajectory file linked into a real agent's atif/
    std::fs::create_dir_all(lab.hermes.join("agenty").join("atif")).unwrap();
    std::os::unix::fs::symlink(
        outside.join("t.json"),
        lab.hermes.join("agenty").join("atif").join("t.json"),
    )
    .unwrap();
    lab.refresh();
    let st = app(&lab);
    assert_eq!(trace_ids(&st).await, vec![uid(100)]);
    let idx = lab.index_state();
    assert_eq!(idx["events"], 4, "{idx}");
    assert_eq!(idx["trajectories"], 0, "{idx}");
    let tj = ok(&st, "/trajectories").await;
    assert_eq!(tj["total"], 0, "{tj}");
}

// ── trajectories: Hermes ids ──────────────────────────────────────────────────────────────────

const HERMES_ID: &str = "chat_0a1b2c3d4e_20261110_093000";

fn hermes_lab() -> Lab {
    let lab = Lab::empty(config());
    let dir = lab.hermes.join("hermes-a").join("atif");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{HERMES_ID}.json")),
        traj_doc(HERMES_ID, "hermes says hello").to_string(),
    )
    .unwrap();
    lab
}

#[tokio::test]
async fn test_hermes_trajectories_with_identifier_ids_get_a_stable_derived_uuid() {
    let lab = hermes_lab();
    lab.refresh();
    let st = app(&lab);
    let list = ok(&st, "/trajectories").await;
    assert_eq!(list["total"], 1, "{list}");
    let t = &list["items"][0];
    assert_eq!(t["source"], "hermes-a");
    assert_eq!(t["session_id"], HERMES_ID);
    let id = t["id"].as_str().unwrap().to_owned();
    // a UUIDv8: version 8, variant 10xx
    assert_eq!(id.len(), 36);
    assert_eq!(&id[14..15], "8", "{id}");
    assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"), "{id}");
    assert!(!has_problem(
        &lab.index_state(),
        "relay.traces.trajectories_skipped"
    ));
    // the same id in a second, independent build
    let again = hermes_lab();
    again.refresh();
    let list2 = ok(&app(&again), "/trajectories").await;
    assert_eq!(list2["items"][0]["id"], json!(id));
    // filtered by its source
    let by_source = ok(&st, "/trajectories?source=hermes-a").await;
    assert_eq!(by_source["total"], 1);
    assert_eq!(ok(&st, "/trajectories?source=gateway").await["total"], 0);

    let d = ok(&st, &format!("/trajectories/{id}?source=hermes-a")).await;
    assert_eq!(d["trajectory"]["id"], json!(id));
    assert_eq!(d["steps"].as_array().unwrap().len(), 2);
    let c = ok(
        &st,
        &format!("/trajectories/{id}/steps/1/content?source=hermes-a"),
    )
    .await;
    assert_eq!(c["message"]["text"], "hermes says hello");
}

#[tokio::test]
async fn test_a_trajectory_whose_ids_are_neither_uuid_nor_identifier_is_unreadable() {
    let lab = Lab::empty(config());
    let dir = lab.hermes.join("hermes-b").join("atif");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("weird.json"),
        traj_doc("not an id!", "x").to_string(),
    )
    .unwrap();
    lab.refresh();
    let idx = lab.index_state();
    assert_eq!(idx["trajectories"], 0, "{idx}");
    let p = idx["problems"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["code"] == "relay.traces.trajectories_skipped")
        .unwrap_or_else(|| panic!("{idx}"));
    assert!(
        p["message"].as_str().unwrap().contains("1 not readable"),
        "{p}"
    );
}

// ── the committed fixtures ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_the_fixture_captures_index_all_three_files() {
    let lab = Lab::indexed();
    let idx = lab.index_state();
    assert_eq!(idx["state"], "ready", "{idx}");
    for name in [
        "relay-events.jsonl",
        "relay-events.jsonl.1",
        "relay-events.jsonl.2.gz",
    ] {
        let row = file_row(&idx, name);
        assert_eq!(row["state"], "indexed", "{row}");
        assert!(row["events"].as_u64().unwrap() > 0, "{row}");
        assert_eq!(row["skipped_lines"], 0, "{row}");
    }
    assert_eq!(idx["trajectories"], 3);
    assert!(idx["traces"].as_u64().unwrap() > 0);
}

/// Opt-in: write the list views of the fixture index as JSON into `$DUMP_DIR` (to compare with the frontend mock).
#[tokio::test]
async fn dump_fixture_views_when_asked() {
    let Ok(out) = std::env::var("DUMP_DIR") else {
        return; // only with DUMP_DIR set
    };
    let lab = Lab::indexed();
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/relay/plugins.toml"),
        lab.cfg.join("plugins.toml"),
    )
    .unwrap();
    let st = app(&lab);
    for (name, path) in [
        ("index", "/traces/index"),
        ("traces", "/traces?window=all&limit=200&order=asc"),
        ("sessions", "/sessions?window=all"),
        ("analytics", "/analytics?window=all"),
        ("trajectories", "/trajectories"),
        ("capture", "/capture"),
    ] {
        let r = get(&st, &format!("{API}{path}")).await;
        std::fs::write(
            format!("{out}/rust_{name}.json"),
            serde_json::to_string_pretty(&r.json()).unwrap(),
        )
        .unwrap();
    }
}

// ── audit F2: the scan budget holds for gzip rotations ───────────────────────────────────────────

#[tokio::test]
async fn test_the_scan_budget_stops_a_gzip_bomb_with_a_lying_trailer() {
    let mut cfg = config();
    cfg.limits.scan_bytes = 1024 * 1024;
    let lab = Lab::empty(cfg);
    lab.write("relay-events.jsonl", &small_trace(100, 0.0));
    // 512 MiB of one byte, no newline, about 3.3 MB on disk; the trailer says 0 bytes
    let bomb = relay_traces_support::gzip_bomb(b'a', 512 * 1024 * 1024, 0);
    let on_disk = bomb.len() as u64;
    std::fs::write(lab.atof.join("relay-events.jsonl.1.gz"), &bomb).unwrap();
    let t0 = std::time::Instant::now();
    lab.refresh();
    let first = t0.elapsed();
    let idx = lab.index_state();
    let row = file_row(&idx, "relay-events.jsonl.1.gz");
    assert_eq!(row["state"], "partial", "{row}");
    // what was decompressed stayed near the budget: the compressed bytes read are a small part of the file
    let scanned = row["scanned_bytes"].as_u64().unwrap();
    assert!(scanned < on_disk / 10, "{scanned} of {on_disk}");
    assert_eq!(idx["truncated"], true);
    assert!(has_problem(&idx, "relay.traces.scan_budget"), "{idx}");
    assert_eq!(idx["events"], 4, "the live file is indexed: {idx}");
    // a second pass does not go on with it
    lab.refresh();
    let idx2 = lab.index_state();
    let row2 = file_row(&idx2, "relay-events.jsonl.1.gz");
    assert_eq!(row2["state"], "partial");
    assert!(
        row2["scanned_bytes"].as_u64().unwrap() < on_disk / 10,
        "{row2}"
    );
    assert!(first < std::time::Duration::from_secs(20), "{first:?}");
}

// ── audit F3: marks are bounded like spans ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_ten_times_the_mark_cap_stays_within_the_cap() {
    let mut cfg = config();
    cfg.limits.max_spans = 1000;
    let cap = 4 * 1000;
    let lab = Lab::empty(cfg);
    let mut lines = Vec::new();
    // 40,000 marks without a parent, oldest first
    for i in 0..(10 * cap as u64) {
        #[allow(clippy::cast_precision_loss)]
        lines.push(relay_traces_support::mark(
            "custom.note",
            1_000_000 + i,
            None,
            i as f64 * 0.01,
            json!({"phase": "x"}),
        ));
    }
    // then the newest trace, with marks of its own
    lines.extend(small_trace(100, 1000.0));
    for i in 0..10_u64 {
        #[allow(clippy::cast_precision_loss)]
        lines.push(relay_traces_support::mark(
            "custom.note",
            5_000_000 + i,
            Some(100),
            1000.1 + i as f64 * 0.001,
            json!({"phase": "y"}),
        ));
    }
    lab.write("relay-events.jsonl", &lines);
    lab.refresh();
    let idx = lab.index_state();
    let marks = idx["marks"].as_u64().unwrap();
    assert!(marks <= cap as u64, "{marks} marks kept, cap {cap}");
    assert!(marks >= (cap as u64) / 2, "{marks}");
    assert_eq!(idx["truncated"], true, "{idx}");
    assert!(has_problem(&idx, "relay.traces.mark_limit"), "{idx}");
    // the newest trace keeps its marks
    let d = ok(&app(&lab), &format!("/traces/{}", uid(100))).await;
    assert_eq!(d["marks"].as_array().unwrap().len(), 10, "{d}");
    // the store itself, not only the derived view, is bounded
    let snap = lab.a.traces.snapshot();
    assert!(snap.core.store.marks.len() <= cap);
    assert!(snap.core.store.mark_fields.len() <= cap * 4);
}

// ── audit F8: crafted numbers never overflow ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_extreme_timestamps_and_token_counts_do_not_panic() {
    let lab = Lab::empty(config());
    let big = i64::MAX - 10;
    let usage = json!({"prompt_tokens": big, "completion_tokens": big, "total_tokens": big,
                       "input_tokens": big, "output_tokens": big, "cache_read_input_tokens": big,
                       "cache_creation_input_tokens": big, "prompt_tokens_details": {"cached_tokens": big}});
    let mut lines = vec![scope("start", "agent", "a", 1, None, 0.0, json!({}))];
    let [start, end] = llm_pair(
        2,
        Some(1),
        1.0,
        2.0,
        json!({}),
        json!({"model": "m", "messages": [{"role": "user", "content": "x"}], "max_tokens": big}),
        json!({"choices": [{"finish_reason": "stop", "message": {"content": "y"}}], "usage": usage}),
    );
    lines.push(start);
    // chunk marks at the two ends of time, then an end far in the future
    for (i, ts) in [
        json!(1.0e300),
        json!("0001-01-01T00:00:00Z"),
        json!(1),
        json!(9.2e18),
    ]
    .into_iter()
    .enumerate()
    {
        let mut m: Value = serde_json::from_str(&relay_traces_support::mark(
            "llm.chunk",
            50 + i as u64,
            Some(2),
            1.5,
            json!({"chunk_index": i}),
        ))
        .unwrap();
        m["timestamp"] = ts;
        lines.push(m.to_string());
    }
    // the end keeps a plain timestamp: its token counts must be indexed
    lines.push(end);
    // a second trace whose start is at the year 1 and whose end is far in the future
    let mut s3: Value =
        serde_json::from_str(&scope("start", "agent", "b", 3, None, 0.0, json!({}))).unwrap();
    s3["timestamp"] = json!("0001-01-01T00:00:00+00:00");
    let mut e3: Value =
        serde_json::from_str(&scope("end", "agent", "b", 3, None, 0.0, json!({}))).unwrap();
    e3["timestamp"] = json!(9.2e18);
    lines.push(s3.to_string());
    lines.push(e3.to_string());
    lines.push(scope("end", "agent", "a", 1, None, 3.0, json!({})));
    lab.write("relay-events.jsonl", &lines);
    lab.refresh();
    let st = app(&lab);
    for sub in [
        "/traces/index".to_owned(),
        "/traces?window=all&sort=tokens".to_owned(),
        "/traces?window=all&sort=duration".to_owned(),
        format!("/traces/{}", uid(1)),
        "/sessions?window=all&sort=tokens".to_owned(),
        "/analytics?window=all&buckets=240".to_owned(),
        "/analytics".to_owned(),
        format!("/traces/{}/spans/{}/content", uid(1), uid(2)),
    ] {
        let r = get(&st, &format!("{API}{sub}")).await;
        assert_eq!(r.status, 200, "{sub}: {}", r.text());
    }
    // the huge counts were indexed (saturated, not wrapped) and the out-of-range times were not
    let d = ok(&st, &format!("/traces/{}", uid(1))).await;
    let llm = &d["spans"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == json!(uid(2)))
        .unwrap()["llm"];
    assert!(llm["input_tokens"].as_i64().unwrap() > 0, "{llm}");
    assert!(llm["total_tokens"].as_i64().unwrap() > 0, "{llm}");
    let t = ok(&st, "/traces?window=all").await;
    for item in t["items"].as_array().unwrap() {
        for k in ["start_ts", "end_ts"] {
            if let Some(x) = item[k].as_f64() {
                assert!((0.0..=253_402_300_800.0).contains(&x), "{k} {x}");
            }
        }
    }
}
