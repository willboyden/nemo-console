//! The Relay trace explorer routes (addendum 9.12) over HTTP: status codes, the two switches, roles, the content
//! routes (scrubbing, caps, header allowlist) and the marker test: a string planted in every payload field never
//! reaches a viewer route, and does reach the admin content route. Hermetic: temp dirs, closed ports, no network.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

mod common;
mod relay_traces_support;

use std::sync::Arc;

use common::TestReq;
use nemo_console::container::AppState;
use nemo_console::domain::models::Role;
use relay_traces_support::{
    API, Lab, app, at, config, get, llm_pair, mark, ok, scope, traj_doc, uid,
};
use serde_json::{Value, json};

/// The planted string. Identifier-shaped on purpose: the index keeps identifier-shaped values, so only the field
/// rules (not the alphabet) can keep it out.
const MARK: &str = "ZZPAYLOADZZ";
const TRAJ: &str = "01a0f1e0-aaaa-7000-8000-0000000000aa";

async fn viewer_key(st: &Arc<AppState>) -> String {
    st.auth.mint(Role::Viewer, "").unwrap().1
}

async fn as_viewer(st: &Arc<AppState>, key: &str, sub: &str) -> common::TestResp {
    TestReq::new(Arc::clone(st), "GET", &format!("{API}{sub}"))
        .client("192.0.2.10")
        .token(key)
        .send()
        .await
}

async fn status(st: &Arc<AppState>, sub: &str) -> (u16, String) {
    let r = get(st, &format!("{API}{sub}")).await;
    (r.status, r.code())
}

// ── a capture with content in every field ─────────────────────────────────────────────────────

/// One trace (root 1) with an LLM span (2), a tool span (3) and a chunk mark, every payload field carrying `MARK`;
/// plus an error trace (10). Ids, names, models and allowlisted headers are plain identifiers without it.
fn marked_capture() -> Vec<String> {
    let m = MARK;
    let sentence = format!("please read {m} and answer");
    let md = json!({"metadata": {"agent_kind": "claude-code", "session_id": "relay-sess-m",
                                 "tool_correlation_arguments": {"command": m}}});
    let mut lines = vec![scope(
        "start",
        "agent",
        "claude-code",
        1,
        None,
        0.0,
        md.clone(),
    )];
    let [start, end] = llm_pair(
        2,
        Some(1),
        1.0,
        3.0,
        json!({
            "user-agent": "opencode/2.0.0", "content-type": "application/json",
            "x-opencode-session-id": "ses_marked", "x-nemo-relay-session-id": "relay-sess-m",
            "x-custom-header": m, "x-trace-note": format!("note {m}"),
            "authorization": format!("Bearer {m}abcdefgh"), "cookie": format!("sid={m}"),
            "x-api-key": m, "x-team-key": m, "x-session-token": m,
        }),
        json!({
            "model": "local-small", "stream": true, "temperature": 0.2,
            "messages": [
                {"role": "system", "content": sentence},
                {"role": "user", "content": m},
                {"role": "user", "content": [{"type": "text", "text": sentence}, {"type": "text", "text": m}]},
                {"role": "assistant", "content": null,
                 "tool_calls": [{"id": "call_a", "type": "function", "function": {"name": "read_file", "arguments": format!("{{\"path\":\"{m}\"}}")}}]},
                {"role": "tool", "tool_call_id": "call_a", "content": m},
            ],
            "tools": [{"type": "function", "function": {"name": "read_file", "description": sentence,
                       "parameters": {"type": "object", "properties": {"path": {"type": "string", "description": m}}}}}],
        }),
        json!({
            "model": "local-small",
            "choices": [{"finish_reason": "tool_calls", "message": {"role": "assistant", "content": sentence,
                         "reasoning_content": m,
                         "tool_calls": [{"id": "call_b", "type": "function", "function": {"name": "read_file", "arguments": m}}]}}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120},
        }),
    );
    // Relay's annotated copies of the request and answer carry the same payloads
    let mut start: Value = serde_json::from_str(&start).unwrap();
    start["category_profile"] = json!({"model_name": "local-small", "annotated_request": {
        "messages": [{"role": "user", "content": m}], "instructions": [{"type": "text", "text": sentence}], "model": "local-small"}});
    let mut end: Value = serde_json::from_str(&end).unwrap();
    end["category_profile"] = json!({"model_name": "local-small", "annotated_response": {
        "message": sentence, "model": "local-small",
        "tool_calls": [{"id": "call_b", "name": "read_file", "arguments": m}],
        "api_specific": {"content_blocks": [{"type": "text", "text": m}]}}});
    lines.push(start.to_string());
    lines.push(mark(
        "llm.chunk",
        50,
        Some(2),
        1.5,
        json!({"chunk_index": 0, "event_type": "content", "delta": {"content": m}, "text": m}),
    ));
    lines.push(end.to_string());
    lines.push(scope(
        "start",
        "tool",
        "Bash",
        3,
        Some(1),
        4.0,
        json!({"data": {"command": m, "args": [sentence], "nested": {"deep": m}}, "metadata": md["metadata"].clone(),
               "category_profile": {"tool_call_id": "call_b"}}),
    ));
    lines.push(scope(
        "end",
        "tool",
        "Bash",
        3,
        Some(1),
        5.0,
        json!({"data": {"stdout": sentence, "stderr": m, "exit": 0}, "metadata": md["metadata"].clone()}),
    ));
    // a routing mark with an allowlisted field and a free-text one
    lines.push(mark(
        "switchyard.routing.error",
        51,
        Some(1),
        5.5,
        json!({"phase": "upstream", "failure_kind": "timeout", "detail": m, "message": sentence}),
    ));
    lines.push(scope("end", "agent", "claude-code", 1, None, 6.0, md));
    // a failed call: its status description is free text
    lines.push(scope("start", "agent", "codex", 10, None, 10.0, json!({})));
    lines.extend(llm_pair(
        11,
        Some(10),
        10.5,
        11.0,
        json!({"user-agent": "codex/1.0"}),
        json!({"model": "local-small", "messages": [{"role": "user", "content": m}]}),
        Value::Null,
    ));
    let mut failed: Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    failed["metadata"] = json!({"error.type": "internal_error", "otel.status_code": "ERROR",
                                "otel.status_description": format!("upstream said {m}")});
    *lines.last_mut().unwrap() = failed.to_string();
    lines.push(scope("end", "agent", "codex", 10, None, 12.0, json!({})));
    lines
}

/// A gateway trajectory whose every content field carries `MARK`, linked to span 2.
fn marked_trajectory() -> Value {
    let m = MARK;
    json!({
        "schema_version": "ATIF-v1.7", "session_id": TRAJ, "trajectory_id": TRAJ,
        "agent": {"name": "example-agent", "version": "1.0.0", "model_name": "local-small"},
        "steps": [
            {"step_id": 1, "source": "user", "timestamp": at(1.0), "message": m,
             "extra": {"ancestry": {"function_id": uid(2), "function_name": "openai.chat_completions"},
                       "llm_request": {"messages": [{"role": "user", "content": m}]}}},
            {"step_id": 2, "source": "agent", "timestamp": at(3.0), "model_name": "local-small",
             "message": [{"type": "text", "text": m}], "reasoning_content": m,
             "tool_calls": [{"tool_call_id": "call_b", "function_name": "read_file", "arguments": {"path": m}}],
             "observation": {"results": [{"source_call_id": "call_b", "content": m}]},
             "metrics": {"prompt_tokens": 100, "completion_tokens": 20},
             "extra": {"ancestry": {"function_id": uid(2)}, "llm_response": {"choices": [{"message": {"content": m}}]}}},
        ],
        "final_metrics": {"total_prompt_tokens": 100, "total_completion_tokens": 20},
    })
}

fn marked_lab(cfg: nemo_console::adapters::relay_traces::TraceConfig) -> Lab {
    let lab = Lab::empty(cfg);
    lab.write("relay-events.jsonl", &marked_capture());
    std::fs::write(
        lab.atif.join(format!("nemo-relay-atif-{TRAJ}.json")),
        marked_trajectory().to_string(),
    )
    .unwrap();
    // a plugins.toml for the capture view (no endpoint: nothing is probed)
    std::fs::write(
        lab.cfg.join("plugins.toml"),
        "[[components]]\nkind = \"observability\"\nenabled = true\n[components.config]\nversion = 1\n\
         [components.config.atof]\nenabled = true\n",
    )
    .unwrap();
    lab.refresh();
    lab
}

// ── THE marker test ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_a_planted_marker_never_reaches_a_viewer_route_and_does_reach_content() {
    let lab = marked_lab(config());
    let st = app(&lab);
    let viewer = viewer_key(&st).await;
    // the capture really is indexed (else absence would prove nothing)
    let idx = lab.index_state();
    assert_eq!(idx["state"], "ready", "{idx}");
    assert_eq!(idx["traces"], 2, "{idx}");
    assert_eq!(idx["trajectories"], 1, "{idx}");

    let session = ok(&st, &format!("/traces/{}", uid(1))).await["trace"]["session_key"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(session, "ses_marked");
    let mut routes: Vec<String> = vec![
        "/traces/index".into(),
        "/traces".into(),
        "/traces?window=all&limit=200".into(),
        "/traces?window=all&sort=tokens&order=asc".into(),
        "/traces?window=all&status=error".into(),
        "/traces?window=all&client=opencode&model=local-small&kind=claude-code".into(),
        "/traces?window=all&q=ses_marked".into(),
        format!("/traces/{}", uid(1)),
        format!("/traces/{}", uid(10)),
        "/sessions".into(),
        "/sessions?window=all&sort=tokens".into(),
        format!("/sessions/{session}"),
        format!("/sessions/{}", uid(10)),
        "/analytics".into(),
        "/analytics?window=all&buckets=240".into(),
        "/analytics?window=7d&anchor=latest&model=local-small".into(),
        "/trajectories".into(),
        format!("/trajectories?trace={}", uid(1)),
        format!("/trajectories/{TRAJ}"),
        format!("/trajectories/{TRAJ}?source=gateway"),
        "/capture".into(),
    ];
    // and every span of both traces, through the detail answers above; the list is long enough to be sure the views
    // carry data: each must answer 200 with a non-trivial body
    routes.dedup();
    let mut seen_data = 0;
    for sub in &routes {
        for (who, r) in [
            ("admin", get(&st, &format!("{API}{sub}")).await),
            ("viewer", as_viewer(&st, &viewer, sub).await),
        ] {
            assert_eq!(r.status, 200, "{who} {sub}: {}", r.text());
            let body = r.text();
            assert!(!body.contains(MARK), "{who} {sub} leaks the marker: {body}");
            if body.contains("local-small") || body.contains("read_file") {
                seen_data += 1;
            }
        }
    }
    assert!(
        seen_data >= 20,
        "the views answered without data ({seen_data})"
    );
    // the detail answers did list the spans and the trajectory's steps
    let d = ok(&st, &format!("/traces/{}", uid(1))).await;
    assert_eq!(d["spans"].as_array().unwrap().len(), 3, "{d}");
    assert!(!d["marks"].as_array().unwrap().is_empty());
    let llm = d["spans"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == json!(uid(2)))
        .unwrap();
    assert_eq!(llm["content_available"], true);
    assert_eq!(llm["llm"]["chunks"], 1);
    assert_eq!(llm["llm"]["requested_tools"], json!(["read_file"]));
    let t = ok(&st, &format!("/trajectories/{TRAJ}")).await;
    assert_eq!(t["steps"].as_array().unwrap().len(), 2, "{t}");
    assert_eq!(t["trajectory"]["trace_id"], json!(uid(1)));

    // positive control: the admin content routes DO return the payloads
    for sub in [
        format!("/traces/{}/spans/{}/content", uid(1), uid(2)),
        format!("/traces/{}/spans/{}/content", uid(1), uid(3)),
        format!("/traces/{}/spans/{}/content", uid(10), uid(11)),
        format!("/trajectories/{TRAJ}/steps/1/content"),
        format!("/trajectories/{TRAJ}/steps/2/content"),
    ] {
        let r = get(&st, &format!("{API}{sub}")).await;
        assert_eq!(r.status, 200, "{sub}: {}", r.text());
        assert!(
            r.text().contains(MARK),
            "{sub} lacks the marker: {}",
            r.text()
        );
        // and a viewer is refused there by the role check
        let v = as_viewer(&st, &viewer, &sub).await;
        assert_eq!(v.status, 403, "{sub}");
        assert_ne!(v.code(), "content_disabled");
        assert!(!v.text().contains(MARK));
    }
}

// ── content: scrubbing, caps, headers ─────────────────────────────────────────────────────────

fn content_lab(messages: Vec<Value>, headers: Value, tool_args: Value) -> Lab {
    let lab = Lab::empty(config());
    let mut lines = vec![scope("start", "agent", "a", 1, None, 0.0, json!({}))];
    lines.extend(llm_pair(
        2,
        Some(1),
        1.0,
        2.0,
        headers,
        json!({"model": "local-small", "messages": messages, "temperature": 0.5, "api_key": "sk-shouldneverbeseen123456"}),
        json!({"choices": [{"finish_reason": "stop", "message": {"role": "assistant",
               "content": "the key is sk-ABCDEFGHIJKLMNOPQRSTUV and that is all"}}]}),
    ));
    lines.push(scope(
        "start",
        "tool",
        "Bash",
        3,
        Some(1),
        3.0,
        json!({"data": tool_args}),
    ));
    lines.push(scope(
        "end",
        "tool",
        "Bash",
        3,
        Some(1),
        4.0,
        json!({"data": {"stdout": "done"}}),
    ));
    lines.push(scope("end", "agent", "a", 1, None, 5.0, json!({})));
    lab.write("relay-events.jsonl", &lines);
    lab.refresh();
    lab
}

fn all_texts(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| all_texts(x, out)),
        Value::Object(m) => m.values().for_each(|x| all_texts(x, out)),
        _ => {}
    }
}

#[tokio::test]
async fn test_content_scrubs_secrets_and_counts_the_redactions() {
    let lab = content_lab(
        vec![
            json!({"role": "system", "content": "use sk-abcdefghijklmnopqrstuvwx for the call"}),
            json!({"role": "user", "content": "Authorization: Bearer abcdefgh12345678xyz"}),
            json!({"role": "user", "content": "config:\napi_key = \"hunter2hunter2\"\nother = 1"}),
        ],
        json!({"user-agent": "opencode/2.0.0"}),
        json!({"api_key": "plainsecretvalue", "path": "a.txt"}),
    );
    let st = app(&lab);
    let c = ok(&st, &format!("/traces/{}/spans/{}/content", uid(1), uid(2))).await;
    let body = c.to_string();
    for secret in [
        "sk-abcdefghijklmnopqrstuvwx",
        "abcdefgh12345678xyz",
        "hunter2hunter2",
        "sk-ABCDEFGHIJKLMNOPQRSTUV",
        "sk-shouldneverbeseen123456",
    ] {
        assert!(!body.contains(secret), "{secret} in {body}");
    }
    let msgs = &c["request"]["messages"];
    assert_eq!(
        msgs[0]["parts"][0]["text"], "use [REDACTED] for the call",
        "{c}"
    );
    assert_eq!(
        msgs[1]["parts"][0]["text"],
        "Authorization: Bearer [REDACTED]"
    );
    assert!(
        msgs[2]["parts"][0]["text"]
            .as_str()
            .unwrap()
            .contains("api_key = \"[REDACTED]\""),
        "{c}"
    );
    assert!(
        c["response"]["parts"][0]["text"]
            .as_str()
            .unwrap()
            .contains("[REDACTED]")
    );
    assert!(c["redactions"].as_u64().unwrap() >= 4, "{c}");
    assert_eq!(c["request"]["params"]["temperature"], 0.5);
    assert!(c["request"]["params"].get("api_key").is_none(), "{c}");

    // a structured tool argument with a sensitive key loses its value
    let t = ok(&st, &format!("/traces/{}/spans/{}/content", uid(1), uid(3))).await;
    let args = t["tool"]["arguments"]["text"].as_str().unwrap();
    assert!(!args.contains("plainsecretvalue"), "{t}");
    assert!(args.contains("a.txt") && args.contains("[REDACTED]"), "{t}");
    assert!(t["redactions"].as_u64().unwrap() >= 1, "{t}");
    assert_eq!(
        t["tool"]["result"]["text"]
            .as_str()
            .map(|s| s.contains("done")),
        Some(true)
    );
}

/// Credential pairs inside text (a tool result holding JSON as a string, YAML and env lines in a message) are
/// redacted and counted; placeholders are not.
#[tokio::test]
async fn test_content_redacts_credential_pairs_inside_text() {
    let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let tool_result = json!({"token": hex, "token_id": hex, "ok": true}).to_string();
    let lab = content_lab(
        vec![
            json!({"role": "user", "content": format!("db:\n  password: {hex}\nexport API_TOKEN={hex}")}),
            json!({"role": "tool", "content": tool_result}),
            json!({"role": "user", "content": "keep: {\"api_key\": \"${OPENAI_API_KEY}\", \"apiKey\": \"{env:K}\"}"}),
        ],
        json!({}),
        json!({"stdout": format!("{{\"auth_token\": \"{hex}\"}}")}),
    );
    let st = app(&lab);
    let c = ok(&st, &format!("/traces/{}/spans/{}/content", uid(1), uid(2))).await;
    assert!(!c.to_string().contains(hex), "{c}");
    let m = &c["request"]["messages"];
    assert_eq!(
        m[0]["parts"][0]["text"],
        "db:\n  password: [REDACTED]\nexport API_TOKEN=[REDACTED]"
    );
    assert_eq!(
        m[1]["parts"][0]["text"],
        "{\"token\":\"[REDACTED]\",\"token_id\":\"[REDACTED]\",\"ok\":true}"
    );
    assert_eq!(
        m[2]["parts"][0]["text"],
        "keep: {\"api_key\": \"${OPENAI_API_KEY}\", \"apiKey\": \"{env:K}\"}"
    );
    // the 4 pairs in the messages, and the sk- key in the response
    assert!(c["redactions"].as_u64().unwrap() >= 5, "{c}");
    // the tool's own arguments: JSON inside a JSON string
    let t = ok(&st, &format!("/traces/{}/spans/{}/content", uid(1), uid(3))).await;
    assert!(!t.to_string().contains(hex), "{t}");
    assert!(t["redactions"].as_u64().unwrap() >= 1, "{t}");
}

#[tokio::test]
async fn test_content_cuts_long_texts_and_returns_at_most_400_messages() {
    let mut messages = vec![json!({"role": "user", "content": "y".repeat(5000)})];
    for i in 1..450 {
        messages.push(json!({"role": "user", "content": format!("m{i}")}));
    }
    let lab = content_lab(messages, json!({}), json!({"path": "a"}));
    let st = app(&lab);
    let base = format!("/traces/{}/spans/{}/content", uid(1), uid(2));

    let head = ok(&st, &format!("{base}?max_chars=1000&messages=head")).await;
    let req = &head["request"];
    assert_eq!(req["messages_total"], 450);
    assert_eq!(req["messages_returned"], 400);
    assert_eq!(req["messages"].as_array().unwrap().len(), 400);
    assert_eq!(head["truncated"], true);
    let first = &req["messages"][0]["parts"][0];
    assert_eq!(first["truncated"], true, "{first}");
    assert_eq!(first["chars"], 5000);
    assert!(first["text"].as_str().unwrap().chars().count() <= 1000);
    assert_eq!(req["messages"][1]["parts"][0]["text"], "m1");

    // tail (the default): the last 400
    let tail = ok(&st, &base).await;
    let msgs = tail["request"]["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 400);
    assert_eq!(msgs[399]["parts"][0]["text"], "m449");
    assert_eq!(msgs[0]["parts"][0]["text"], "m50");
    assert_eq!(tail["truncated"], true);
    assert!(tail.to_string().len() <= 4 * 1024 * 1024);
}

#[tokio::test]
async fn test_content_returns_only_allowlisted_headers() {
    let lab = content_lab(
        vec![json!({"role": "user", "content": "hi"})],
        json!({
            "user-agent": "opencode/2.0.0", "content-type": "application/json",
            "traceparent": "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
            "x-opencode-client": "cli", "x-opencode-session-id": "ses_a", "x-session-id": "ses_b",
            "x-claude-code-session-id": "ses_c", "x-nemo-relay-session-id": "relay-1",
            "authorization": "Bearer abcdefghijklmnop", "proxy-authorization": "Basic abc", "cookie": "sid=1",
            "set-cookie": "sid=2", "x-api-key": "k1", "x-team-key": "k2", "x-session-token": "t1",
            "x-nemo-relay-api-key": "k3", "x-nemo-relay-auth-token": "t2", "x-other": "o",
        }),
        json!({}),
    );
    let st = app(&lab);
    let c = ok(&st, &format!("/traces/{}/spans/{}/content", uid(1), uid(2))).await;
    let headers = c["headers"].as_object().unwrap();
    let mut names: Vec<&str> = headers.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "content-type",
            "traceparent",
            "user-agent",
            "x-claude-code-session-id",
            "x-nemo-relay-session-id",
            "x-opencode-client",
            "x-opencode-session-id",
            "x-session-id",
        ],
        "{c}"
    );
    assert_eq!(headers["user-agent"], "opencode/2.0.0");
    let mut texts = Vec::new();
    all_texts(&c, &mut texts);
    for v in [
        "abcdefghijklmnop",
        "Basic abc",
        "sid=1",
        "k1",
        "k2",
        "k3",
        "t1",
        "t2",
    ] {
        assert!(!texts.iter().any(|t| t == v), "{v} returned: {c}");
    }
}

// ── status codes ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_routes_answer_404_for_other_systems_and_unknown_ids() {
    let lab = marked_lab(config());
    let st = app(&lab);
    for sub in [
        "/traces/index",
        "/traces",
        "/sessions",
        "/analytics",
        "/trajectories",
        "/capture",
    ] {
        for sys in ["switchyard", "dynamo", "modelopt"] {
            let r = get(&st, &format!("/api/v1/systems/{sys}{sub}")).await;
            assert_eq!(r.status, 404, "{sys}{sub}: {}", r.text());
            assert_eq!(r.code(), "not_found");
        }
    }
    let r = get(
        &st,
        &format!("/api/v1/systems/switchyard/traces/{}", uid(1)),
    )
    .await;
    assert_eq!(r.status, 404);
    let unknown = "01a0f1e0-ffff-7000-8000-00000000ffff";
    for sub in [
        format!("/traces/{unknown}"),
        format!("/traces/{unknown}/spans/{}/content", uid(2)),
        // a span of ANOTHER trace is not found under this one
        format!("/traces/{}/spans/{}/content", uid(10), uid(2)),
        "/sessions/no-such-session".to_owned(),
        format!("/trajectories/{unknown}"),
        format!("/trajectories/{unknown}/steps/1/content"),
        format!("/trajectories/{TRAJ}/steps/99/content"),
        format!("/trajectories/{TRAJ}?source=hermes"),
    ] {
        assert_eq!(status(&st, &sub).await, (404, "not_found".into()), "{sub}");
    }
}

#[tokio::test]
async fn test_invalid_ids_and_query_values_are_422() {
    let lab = marked_lab(config());
    let st = app(&lab);
    let long_q = "a".repeat(81);
    for sub in [
        "/traces/not-a-uuid".to_owned(),
        format!("/traces/{}/spans/not-a-uuid/content", uid(1)),
        format!("/traces/{}x/spans/{}/content", uid(1), uid(2)),
        "/trajectories/123/steps/1/content".to_owned(),
        format!("/trajectories/{TRAJ}/steps/one/content"),
        format!("/trajectories/{TRAJ}/steps/-1/content"),
        "/sessions/has%20space".to_owned(),
        "/traces?window=2h".to_owned(),
        "/traces?window=ALL".to_owned(),
        "/traces?anchor=yesterday".to_owned(),
        "/traces?limit=0".to_owned(),
        "/traces?limit=201".to_owned(),
        "/traces?limit=ten".to_owned(),
        "/traces?offset=-1".to_owned(),
        "/traces?q=has%20space".to_owned(),
        "/traces?q=%3Cscript%3E".to_owned(),
        format!("/traces?q={long_q}"),
        "/traces?status=failed".to_owned(),
        "/traces?sort=name".to_owned(),
        "/traces?order=up".to_owned(),
        "/traces?min_ms=-5".to_owned(),
        "/traces?client=a%20b".to_owned(),
        "/sessions?limit=500".to_owned(),
        "/sessions?q=x%27y".to_owned(),
        "/sessions?sort=start".to_owned(),
        "/analytics?buckets=11".to_owned(),
        "/analytics?buckets=241".to_owned(),
        "/analytics?window=1y".to_owned(),
        "/trajectories?trace=nope".to_owned(),
        "/trajectories?limit=0".to_owned(),
        "/trajectories?q=a%2Fb%20c".to_owned(),
        format!("/traces/{}/spans/{}/content?max_chars=999", uid(1), uid(2)),
        format!(
            "/traces/{}/spans/{}/content?max_chars=200001",
            uid(1),
            uid(2)
        ),
        format!("/traces/{}/spans/{}/content?messages=all", uid(1), uid(2)),
        format!("/trajectories/{TRAJ}/steps/1/content?max_chars=10"),
    ] {
        assert_eq!(
            status(&st, &sub).await,
            (422, "invalid_request".into()),
            "{sub}"
        );
    }
    // the bounds themselves are accepted
    for sub in [
        "/traces?limit=1&q=".to_owned(),
        "/traces?limit=200".to_owned(),
        format!("/traces?q={}", "a".repeat(80)),
        "/analytics?buckets=12".to_owned(),
        "/analytics?buckets=240".to_owned(),
        format!("/traces/{}/spans/{}/content?max_chars=1000", uid(1), uid(2)),
        format!(
            "/traces/{}/spans/{}/content?max_chars=200000",
            uid(1),
            uid(2)
        ),
    ] {
        assert_eq!(status(&st, &sub).await.0, 200, "{sub}");
    }
}

#[tokio::test]
async fn test_viewer_reads_metadata_and_is_refused_content_by_role() {
    let lab = marked_lab(config());
    let st = app(&lab);
    let viewer = viewer_key(&st).await;
    for sub in ["/traces/index", "/traces?window=all", "/capture"] {
        assert_eq!(as_viewer(&st, &viewer, sub).await.status, 200, "{sub}");
    }
    for sub in [
        format!("/traces/{}/spans/{}/content", uid(1), uid(2)),
        format!("/trajectories/{TRAJ}/steps/1/content"),
    ] {
        let r = as_viewer(&st, &viewer, &sub).await;
        assert_eq!(r.status, 403, "{sub}");
        assert_ne!(r.code(), "content_disabled", "{sub}");
    }
    // no key at all from another host: 401
    let r = TestReq::new(Arc::clone(&st), "GET", &format!("{API}/traces"))
        .client("192.0.2.10")
        .send()
        .await;
    assert_eq!(r.status, 401);
}

#[tokio::test]
async fn test_content_off_answers_403_content_disabled() {
    let mut cfg = config();
    cfg.content = false;
    let lab = marked_lab(cfg);
    let st = app(&lab);
    assert_eq!(lab.index_state()["content"], "off");
    for sub in [
        format!("/traces/{}/spans/{}/content", uid(1), uid(2)),
        format!("/trajectories/{TRAJ}/steps/1/content"),
    ] {
        let r = get(&st, &format!("{API}{sub}")).await;
        assert_eq!(r.status, 403, "{sub}");
        assert_eq!(r.code(), "content_disabled", "{sub}");
        assert!(!r.text().contains(MARK));
        // a viewer still meets the role check first
        let viewer = viewer_key(&st).await;
        let v = as_viewer(&st, &viewer, &sub).await;
        assert_eq!(v.status, 403);
        assert_ne!(v.code(), "content_disabled");
    }
    // the metadata views are unaffected
    assert_eq!(
        ok(&st, &format!("/traces/{}", uid(1))).await["spans"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test]
async fn test_index_off_every_route_says_disabled() {
    let mut cfg = config();
    cfg.enabled = false;
    let lab = marked_lab(cfg);
    let st = app(&lab);
    let idx = ok(&st, "/traces/index").await;
    assert_eq!(idx["state"], "disabled", "{idx}");
    assert_eq!(idx["events"], 0);
    assert!(
        idx["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["code"] == "relay.traces.disabled")
    );
    // the lists: empty, every key present, and state = disabled
    for (sub, empty) in [
        ("/traces?window=all", "items"),
        ("/sessions?window=all", "items"),
        ("/trajectories", "items"),
    ] {
        let v = ok(&st, sub).await;
        assert_eq!(v["state"], "disabled", "{sub}: {v}");
        assert_eq!(v["total"], 0, "{sub}");
        assert_eq!(v[empty], json!([]), "{sub}");
    }
    let an = ok(&st, "/analytics?window=all").await;
    assert_eq!(an["state"], "disabled");
    assert_eq!(an["totals"]["llm_calls"], 0);
    let cap = ok(&st, "/capture").await;
    assert_eq!(cap["state"], "disabled", "{cap}");
    assert!(cap["levels"].is_array());
    // details: not found, and the message names the switch
    for sub in [
        format!("/traces/{}", uid(1)),
        "/sessions/ses_marked".to_owned(),
        format!("/trajectories/{TRAJ}"),
        format!("/traces/{}/spans/{}/content", uid(1), uid(2)),
        format!("/trajectories/{TRAJ}/steps/1/content"),
    ] {
        let r = get(&st, &format!("{API}{sub}")).await;
        assert_eq!(r.status, 404, "{sub}");
        assert!(
            r.json()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("relay_trace_index = off"),
            "{sub}: {}",
            r.text()
        );
    }
}

#[tokio::test]
async fn test_content_whose_source_line_is_gone_is_409() {
    let lab = marked_lab(config());
    let st = app(&lab);
    let sub = format!("/traces/{}/spans/{}/content", uid(1), uid(2));
    assert_eq!(status(&st, &sub).await.0, 200);
    // the file was rewritten with other content under the same name (no refresh since)
    lab.write(
        "relay-events.jsonl",
        &[scope("start", "agent", "x", 777, None, 0.0, json!({}))],
    );
    assert_eq!(status(&st, &sub).await, (409, "content_unavailable".into()));
    // the file is gone
    std::fs::remove_file(lab.atof.join("relay-events.jsonl")).unwrap();
    assert_eq!(status(&st, &sub).await, (409, "content_unavailable".into()));
    // a trajectory file that went away
    let step = format!("/trajectories/{TRAJ}/steps/1/content");
    std::fs::write(
        lab.atif.join(format!("nemo-relay-atif-{TRAJ}.json")),
        "{\"truncated\": ",
    )
    .unwrap();
    assert_eq!(status(&st, &step).await.0, 409, "{step}");
}

#[tokio::test]
async fn test_trajectory_content_and_detail_of_a_gateway_trajectory() {
    let lab = Lab::empty(config());
    let id = "01a0f1e0-bbbb-7000-8000-0000000000bb";
    std::fs::write(
        lab.atif.join(format!("nemo-relay-atif-{id}.json")),
        traj_doc(id, "use sk-abcdefghijklmnopqrstuvwx please").to_string(),
    )
    .unwrap();
    lab.refresh();
    let st = app(&lab);
    let list = ok(&st, "/trajectories").await;
    assert_eq!(list["items"][0]["source"], "gateway");
    assert_eq!(list["items"][0]["steps"], 2);
    let d = ok(&st, &format!("/trajectories/{id}")).await;
    let steps = d["steps"].as_array().unwrap();
    assert_eq!(steps[1]["tool_calls"][0]["name"], "read_file");
    assert!(steps[0]["message_chars"].as_u64().unwrap() > 0);
    let c = ok(&st, &format!("/trajectories/{id}/steps/1/content")).await;
    assert_eq!(c["message"]["text"], "use [REDACTED] please");
    assert_eq!(c["redactions"], 1);
    let c2 = ok(&st, &format!("/trajectories/{id}/steps/2/content")).await;
    assert_eq!(c2["observations"][0]["content"]["text"], "file body");
    assert!(
        c2["tool_calls"][0]["arguments"]["text"]
            .as_str()
            .unwrap()
            .contains("a.txt")
    );
}

// ── audit F4: a trajectory is read only from the file the index saw, through real directories ──────

fn hermes_trajectory_lab() -> (Lab, String) {
    let lab = Lab::empty(config());
    let dir = lab.hermes.join("hermes-a").join("atif");
    std::fs::create_dir_all(&dir).unwrap();
    let id = "01a0f1e0-cccc-7000-8000-0000000000cc";
    std::fs::write(
        dir.join(format!("{id}.json")),
        traj_doc(id, "hermes says hello").to_string(),
    )
    .unwrap();
    lab.refresh();
    (lab, id.to_owned())
}

#[tokio::test]
async fn test_a_trajectory_rewritten_after_indexing_is_409() {
    let (lab, id) = hermes_trajectory_lab();
    let st = app(&lab);
    let detail = format!("/trajectories/{id}");
    let step = format!("/trajectories/{id}/steps/1/content");
    assert_eq!(status(&st, &detail).await.0, 200);
    assert_eq!(status(&st, &step).await.0, 200);
    // the same name, other text of the same length, written after the index read it
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(
        lab.hermes.join("hermes-a/atif").join(format!("{id}.json")),
        traj_doc(&id, "hermes says HELLO").to_string(),
    )
    .unwrap();
    for sub in [&detail, &step] {
        let r = get(&st, &format!("{API}{sub}")).await;
        assert_eq!(
            (r.status, r.code()),
            (409, "content_unavailable".to_owned()),
            "{sub}: {}",
            r.text()
        );
        assert!(!r.text().contains("HELLO"));
    }
}

#[tokio::test]
async fn test_a_trajectory_directory_swapped_for_a_symlink_is_not_read() {
    for swap in ["atif", "agent"] {
        let (lab, id) = hermes_trajectory_lab();
        let st = app(&lab);
        let detail = format!("/trajectories/{id}");
        assert_eq!(status(&st, &detail).await.0, 200);
        // the very same files, now reached through a link
        let agent = lab.hermes.join("hermes-a");
        if swap == "atif" {
            std::fs::rename(agent.join("atif"), agent.join("atif.real")).unwrap();
            std::os::unix::fs::symlink(agent.join("atif.real"), agent.join("atif")).unwrap();
        } else {
            let real = lab.tmp.path().join("agent.real");
            std::fs::rename(&agent, &real).unwrap();
            std::os::unix::fs::symlink(&real, &agent).unwrap();
        }
        for sub in [
            detail.clone(),
            format!("/trajectories/{id}/steps/1/content"),
        ] {
            let (code, _) = status(&st, &sub).await;
            assert!(code == 404 || code == 409, "{swap} {sub}: {code}");
        }
    }
}

// ── audit F5: an exact header allowlist, identifier-shaped values ─────────────────────────────────

#[tokio::test]
async fn test_relay_headers_are_an_exact_allowlist_with_plain_values() {
    let lab = content_lab(
        vec![json!({"role": "user", "content": "hi"})],
        json!({
            "user-agent": "opencode/2.0.0 (linux; x64)",
            "tracestate": "vendor=abc,other=def",
            "x-nemo-relay-session-id": "relay-1", "x-nemo-relay-subagent-id": "sub-1",
            "x-nemo-relay-owner-id": "owner-1",
            "x-nemo-relay-apikey": "k1value", "x-nemo-relay-api_key": "k2value",
            "x-nemo-relay-bearer": "b1value", "x-nemo-relay-passwd": "p1value",
            "x-nemo-relay-pat": "pat1value", "x-nemo-relay-jwt": "jwt1value", "x-nemo-relay-pwd": "pwd1value",
            "x-nemo-relay-anything": "a1value",
            "x-session-id": "has space <b>",
            "x-opencode-session-id": "x".repeat(201),
        }),
        json!({}),
    );
    let st = app(&lab);
    let c = ok(&st, &format!("/traces/{}/spans/{}/content", uid(1), uid(2))).await;
    let headers = c["headers"].as_object().unwrap();
    let mut names: Vec<&str> = headers.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "tracestate",
            "user-agent",
            "x-nemo-relay-owner-id",
            "x-nemo-relay-session-id",
            "x-nemo-relay-subagent-id",
        ],
        "{c}"
    );
    assert_eq!(headers["user-agent"], "opencode/2.0.0 (linux; x64)");
    for v in [
        "k1value",
        "k2value",
        "b1value",
        "p1value",
        "pat1value",
        "jwt1value",
        "pwd1value",
        "a1value",
    ] {
        assert!(!c.to_string().contains(v), "{v}: {c}");
    }
}
