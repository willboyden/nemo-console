//! Port of backend/tests/relay/test_relay_events.py. The gzipped rotations the Python tests write with gzip.open are
//! committed fixtures here (tests/fixtures/relay/gen_gz_fixtures.py makes them with Python's own gzip writer).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value,
    clippy::cloned_ref_to_slice_refs
)]

mod relay_support;
mod switchyard_support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use nemo_console::adapters::base::SystemAdapter;
use nemo_console::adapters::relay::{FollowState, RelayAdapter, parse_atof_line};
use nemo_console::domain::models::SystemEvent;
use relay_support::{Router, fx, fx_bytes, make_adapter, with_path};
use serde_json::json;
use switchyard_support::take_events;

const CANARY: &str = "CANARY-SECRET-PROMPT-TEXT with spaces and sk-abcdefghijklmnopqrstuvwxyz0123";

fn sample_events() -> Vec<SystemEvent> {
    fx("atof_sample.jsonl")
        .lines()
        .filter_map(|l| parse_atof_line(l.as_bytes()))
        .collect()
}

fn name(e: &SystemEvent) -> &str {
    e.data["name"].as_str().unwrap()
}

#[test]
fn test_golden_sample_parses_metadata_only() {
    let evs = sample_events();
    assert_eq!(evs.len(), 39);
    assert!(
        evs.iter()
            .all(|e| e.kind == "relay.atof" && e.system.as_str() == "relay")
    );
    let end = evs
        .iter()
        .find(|e| name(e) == "anthropic.messages" && e.data.get("phase") == Some(&json!("end")))
        .unwrap();
    assert_eq!(end.data["model"], "nemotron-lightning");
    assert_eq!(end.data["input_tokens"], 56);
    assert_eq!(end.data["output_tokens"], 400);
    assert_eq!(end.data["stop_reason"], "max_tokens");
    assert_eq!(end.data["gateway_path"], "/v1/messages");
    let dec = evs
        .iter()
        .find(|e| name(e) == "switchyard.routing.decision")
        .unwrap();
    assert_eq!(dec.data["algorithm"], "stage_router");
    assert_eq!(dec.data["served_model"], "nemotron-lightning");
    assert_eq!(dec.data["fallback_used"], false);
    let tok = evs
        .iter()
        .find(|e| name(e) == "switchyard.routing.llm_tokens")
        .unwrap();
    assert_eq!(tok.data["input_tokens"], 56);
    assert_eq!(tok.data["model"], "nemotron-lightning");
    let err = evs
        .iter()
        .find(|e| name(e) == "switchyard.routing.error")
        .unwrap();
    assert_eq!(err.data["upstream_status"], 400);
    assert_eq!(err.data["error_category"], "upstream_http");
    assert_eq!(err.data["severity"], "error");
    assert!(
        evs.iter()
            .all(|e| e.data["size_bytes"].as_u64().unwrap() > 0)
    );
}

#[test]
fn test_no_text_can_reach_an_event() {
    let line = json!({
        "atof_version": "0.1", "kind": "scope", "name": "claude-code-turn", "scope_category": "start", "category": "custom",
        "timestamp": "2026-09-24T01:02:03.123456789+00:00", "uuid": "u-1",
        "category_profile": {"annotated_request": {"messages": [{"content": CANARY}]}, "model_name": CANARY},
        "metadata": {"session_id": CANARY, "gateway_path": CANARY, "agent_kind": CANARY},
        "data": {"prompt": CANARY, "content": {"messages": [{"role": "user", "content": CANARY}], "model": CANARY},
                 "model": CANARY, "outcome": CANARY, "headers": {"x-nemo-relay-session-id": CANARY, "authorization": CANARY},
                 "choices": [{"finish_reason": CANARY, "message": {"content": CANARY}}],
                 "usage": {"input_tokens": 5, "output_tokens": 7}},
    });
    let ev = parse_atof_line(line.to_string().as_bytes()).unwrap();
    let blob = serde_json::to_string(&ev).unwrap();
    assert!(!blob.contains("CANARY") && !blob.contains("sk-"));
    assert_eq!(ev.data["input_tokens"], 5);
    assert_eq!(ev.data["output_tokens"], 7);
    assert!(ev.ts > 0.0 && ev.summary.starts_with("claude-code-turn"));
}

#[test]
fn test_tool_name_name_with_text_is_dropped_and_bad_lines_are_none() {
    assert!(parse_atof_line(b"not json").is_none());
    assert!(parse_atof_line(b"").is_none());
    assert!(parse_atof_line(br#"{"name":"ok","timestamp":"garbage"}"#).is_none());
    let two = json!({"name": "two words secret", "timestamp": "2026-09-24T01:02:03Z"}).to_string();
    assert!(parse_atof_line(two.as_bytes()).is_none());
    assert!(parse_atof_line(&vec![b'x'; 2 * 1024 * 1024]).is_none());
}

fn sorted_ts(evs: &[SystemEvent]) -> bool {
    evs.windows(2).all(|w| w[0].ts <= w[1].ts)
}

#[tokio::test]
async fn test_recent_events_newest_file_skips_chunks_and_bounds() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    let evs = m.a.recent_events(5).await.unwrap();
    assert_eq!(evs.len(), 5);
    assert!(sorted_ts(&evs));
    assert!(evs.iter().all(|e| name(e) != "llm.chunk"));
    assert_eq!(m.a.recent_events(1000).await.unwrap().len(), 38); // 39 sample lines minus the one llm.chunk
    assert!(!m.a.recent_events(0).await.unwrap().is_empty()); // clamped to 1
}

#[tokio::test]
async fn test_recent_events_ignores_symlinks_and_garbage_and_partial_first_line() {
    let r = Router::new().await;
    let good = fx("atof_sample.jsonl").lines().nth(2).unwrap().to_owned();
    let lines = format!("garbage-not-json\n{good}\n{{truncated");
    let m = make_adapter(&r, None, None, Some(&lines), |_| {});
    let atof = m.path("relay_atof_dir");
    let secret = m.tmp.path().join("outside.jsonl");
    std::fs::write(&secret, format!("{good}\n")).unwrap();
    std::os::unix::fs::symlink(&secret, atof.join("relay-events.jsonl.0")).unwrap(); // never followed
    assert_eq!(m.a.recent_events(10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn test_recent_events_missing_dir_is_empty() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    let a = with_path(&m, "relay_atof_dir", m.path("relay_atof_dir").join("nope"));
    assert!(a.recent_events(100).await.unwrap().is_empty());
}

#[tokio::test]
async fn test_follow_events_tails_appends_and_terminates_on_cancel() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    let active = m.path("relay_atof_dir").join("relay-events.jsonl");
    let lines: Vec<String> = fx("atof_sample.jsonl").lines().map(str::to_owned).collect();
    let got: Arc<Mutex<Vec<String>>> = Arc::default();
    let got2 = Arc::clone(&got);
    let a = Arc::clone(&m.a);
    let task = tokio::spawn(async move {
        let mut s = a.follow_events(None);
        while let Some(Ok(ev)) = s.next().await {
            got2.lock().unwrap().push(name(&ev).to_owned());
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let append = |s: &str| {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&active)
            .unwrap();
        f.write_all(s.as_bytes()).unwrap();
    };
    let l3: Vec<char> = lines[3].chars().collect();
    let (head, tail): (String, String) = (l3[..40].iter().collect(), l3[40..].iter().collect());
    append(&format!("{}\n{head}", lines[2])); // one full line + a partial one
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(got.lock().unwrap().len(), 1);
    append(&format!("{tail}\n"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(got.lock().unwrap().len(), 2);
    task.abort();
    let done = tokio::time::timeout(Duration::from_secs(1), task).await;
    assert!(done.is_ok());
}

#[tokio::test]
async fn test_follow_events_since_ts_replays_newer_only() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    let all = m.a.recent_events(1000).await.unwrap();
    let cutoff = all[all.len() - 3].ts;
    let seen = take_events(Arc::clone(&m.a).follow_events(Some(cutoff)), 2, 10).await;
    assert_eq!(seen.len(), 2);
    assert!(seen.iter().all(|e| e.ts > cutoff));
}

// ── gzipped rotations (live 2026-10-17: relay-events.jsonl, .1 and .2.gz) ───────────────────────

#[tokio::test]
async fn test_recent_events_read_a_gzipped_rotation_when_newer_files_are_short() {
    let r = Router::new().await;
    let lines: Vec<String> = fx("atof_sample.jsonl").lines().map(str::to_owned).collect();
    let newest = lines[lines.len() - 5..].join("\n") + "\n";
    let m = make_adapter(&r, None, None, Some(&newest), |_| {}); // .1 holds only the newest five lines
    let atof = m.path("relay_atof_dir");
    // the older ones were rotated away (lines[:-5], gzipped by Python)
    std::fs::write(
        atof.join("relay-events.jsonl.2.gz"),
        fx_bytes("atof_head.jsonl.gz"),
    )
    .unwrap();
    let names: Vec<String> =
        m.a.atof_files_newest_first()
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
    assert_eq!(
        names,
        [
            "relay-events.jsonl",
            "relay-events.jsonl.1",
            "relay-events.jsonl.2.gz"
        ]
    );
    let evs = m.a.recent_events(1000).await.unwrap();
    assert_eq!(evs.len(), 38); // the same 38 events as the plain sample
    assert!(sorted_ts(&evs));
}

#[tokio::test]
async fn test_gzip_rotation_caps_symlinks_and_garbage() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, Some(""), |_| {});
    let atof = m.path("relay_atof_dir");
    std::fs::write(
        atof.join("relay-events.jsonl.2.gz"),
        fx_bytes("atof_all.jsonl.gz"),
    )
    .unwrap();
    std::fs::write(atof.join("relay-events.jsonl.3.gz"), b"not gzip at all").unwrap(); // unreadable: skipped
    let outside = m.tmp.path().join("outside.jsonl.gz");
    std::fs::write(&outside, fx_bytes("atof_line2.jsonl.gz")).unwrap();
    std::os::unix::fs::symlink(&outside, atof.join("relay-events.jsonl.4.gz")).unwrap(); // never followed
    assert_eq!(m.a.recent_events(1000).await.unwrap().len(), 38);
    let mk = |f: &dyn Fn(&mut RelayAdapter)| {
        let mut a = RelayAdapter::new(m.a.ctx.clone());
        f(&mut a);
        a
    };
    let bomb = mk(&|a| a.max_gz_bytes = 1024); // a "bomb": decompresses past the cap
    assert!(bomb.recent_events(1000).await.unwrap().is_empty());
    let big = mk(&|a| {
        a.max_gz_bytes = 256 * 1024 * 1024;
        a.max_gz_file_bytes = 10; // compressed file too large: not read
    });
    assert!(big.recent_events(1000).await.unwrap().is_empty());
}

#[tokio::test]
async fn test_follow_never_tails_a_gzipped_rotation() {
    let r = Router::new().await;
    let m = make_adapter(&r, None, None, None, |_| {});
    let atof = m.path("relay_atof_dir");
    std::fs::remove_file(atof.join("relay-events.jsonl")).unwrap();
    // rotation 0, but compressed
    std::fs::write(
        atof.join("relay-events.jsonl.gz"),
        fx_bytes("atof_sample.jsonl.gz"),
    )
    .unwrap();
    let mut state = FollowState::default();
    assert!(m.a.follow_step(&mut state).unwrap().is_empty());
    assert!(state.ino.is_some()); // the plain .1 is the active file
}

/// Rust-native: the gzip reader against Python's own writer (several members, zero padding, FNAME headers).
#[test]
fn gzip_reader_matches_python_gzip() {
    use nemo_console::services::gzip::read_tail;
    let sample = fx("atof_sample.jsonl");
    let t = read_tail(&fx_bytes("atof_sample.jsonl.gz"), usize::MAX, u64::MAX).unwrap();
    assert_eq!(t.data, sample.as_bytes());
    let lines: Vec<&str> = sample.lines().collect();
    let head = lines[..lines.len() - 5].join("\n") + "\n";
    assert_eq!(
        read_tail(&fx_bytes("atof_head.jsonl.gz"), usize::MAX, u64::MAX)
            .unwrap()
            .data,
        head.as_bytes()
    );
    let two = read_tail(&fx_bytes("two_members.gz"), 9, u64::MAX).unwrap();
    assert_eq!((two.data.as_slice(), two.total), (&b"r\nsecond\n"[..], 20));
    let w = read_tail(&fx_bytes("atof_all.jsonl.gz"), 100, u64::MAX).unwrap();
    let all = lines.join("\n") + "\n";
    assert_eq!(w.data, all.as_bytes()[all.len() - 100..]);
}
