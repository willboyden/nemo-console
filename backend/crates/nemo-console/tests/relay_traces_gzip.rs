//! Content reads from gzip rotations through seek points (addendum 9.12): the index records a checkpoint about every
//! `checkpoint_every` bytes of a `.gz` file's content while it reads it, and the content route resumes the decoder at
//! the nearest one instead of decompressing everything before the line. Hermetic: temp dirs only.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::cast_precision_loss
)]

mod common;
mod relay_traces_support;

use std::sync::Arc;
use std::time::Instant;

use nemo_console::adapters::relay_traces::TraceConfig;
use nemo_console::adapters::relay_traces::scan::{CHECKPOINT_CAP, GzPoints};
use relay_traces_support::{API, Lab, app, config, get, gzip_stored, llm_pair, ok, scope, uid};
use serde_json::{Value, json};

/// `n` traces, each a root (id `10 * i + 1000`) with one LLM span (id + 1) whose request carries `pad` bytes of text.
fn traces(n: u64, pad: usize) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..n {
        let id = 1000 + i * 10;
        let t = i as f64 * 10.0;
        out.push(scope("start", "agent", "agent", id, None, t, json!({})));
        out.extend(llm_pair(
            id + 1,
            Some(id),
            t + 1.0,
            t + 2.0,
            json!({"user-agent": "opencode/2.0.0"}),
            json!({"model": "local-small", "messages": [
                {"role": "user", "content": format!("question {i} {}", "x".repeat(pad))}]}),
            json!({"choices": [{"finish_reason": "stop", "message": {"content": format!("answer {i}")}}]}),
        ));
        out.push(scope("end", "agent", "agent", id, None, t + 3.0, json!({})));
    }
    out
}

fn text_of(lines: &[String]) -> String {
    let mut t = lines.join("\n");
    t.push('\n');
    t
}

/// The seek points the published index holds for its one compressed source.
fn points(lab: &Lab) -> Option<Arc<GzPoints>> {
    let snap = lab.a.traces.snapshot();
    snap.core
        .sources
        .iter()
        .flatten()
        .find(|s| s.gz)
        .and_then(|s| s.points.clone())
}

fn with_every(every: u64) -> TraceConfig {
    let mut cfg = config();
    cfg.limits.checkpoint_every = every;
    cfg
}

fn content_path(i: u64) -> String {
    let id = 1000 + i * 10;
    format!("/traces/{}/spans/{}/content", uid(id), uid(id + 1))
}

#[tokio::test]
async fn test_content_through_seek_points_equals_the_sequential_read() {
    let lines = traces(400, 9_000);
    let gz = gzip_stored(text_of(&lines).as_bytes());
    let with = Lab::empty(with_every(256 * 1024));
    let without = Lab::empty(with_every(u64::MAX));
    for lab in [&with, &without] {
        std::fs::write(lab.atof.join("relay-events.jsonl.1.gz"), &gz).unwrap();
        lab.refresh();
    }
    let p = points(&with).expect("seek points");
    assert!(p.points.len() >= 10, "{}", p.points.len());
    assert!(
        points(&without).is_none_or(|p| p.points.is_empty()),
        "no points without a spacing"
    );
    let (a, b) = (app(&with), app(&without));
    for i in [0, 1, 57, 133, 200, 271, 350, 398, 399] {
        let x = ok(&a, &content_path(i)).await;
        let y = ok(&b, &content_path(i)).await;
        assert_eq!(x, y, "span {i}");
        assert!(
            x["request"]["messages"][0]["parts"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with(&format!("question {i} ")),
            "{i}"
        );
        assert_eq!(x["response"]["parts"][0]["text"], format!("answer {i}"));
    }
}

#[tokio::test]
async fn test_a_rotation_compressed_after_it_was_read_gets_seek_points() {
    let lab = Lab::empty(with_every(128 * 1024));
    let lines = traces(200, 5_000);
    lab.write("relay-events.jsonl.1", &lines);
    lab.refresh();
    assert!(points(&lab).is_none());
    // logrotate compresses .1 into .2.gz: the content is known, only the seek points are new
    std::fs::write(
        lab.atof.join("relay-events.jsonl.2.gz"),
        gzip_stored(text_of(&lines).as_bytes()),
    )
    .unwrap();
    std::fs::remove_file(lab.atof.join("relay-events.jsonl.1")).unwrap();
    lab.refresh();
    let idx = lab.index_state();
    assert_eq!(idx["events"], 800, "read once: {idx}");
    let p = points(&lab).expect("points for the compressed copy");
    assert!(p.points.len() >= 5, "{}", p.points.len());
    let st = app(&lab);
    let c = ok(&st, &content_path(199)).await;
    assert_eq!(c["source"]["compressed"], true);
    assert_eq!(c["source"]["file"], "relay-events.jsonl.2.gz");
    assert_eq!(c["response"]["parts"][0]["text"], "answer 199");
}

#[tokio::test]
async fn test_a_changed_compressed_file_is_refused() {
    let lab = Lab::empty(with_every(64 * 1024));
    let lines = traces(100, 3_000);
    let name = "relay-events.jsonl.1.gz";
    let path = lab.atof.join(name);
    std::fs::write(&path, gzip_stored(text_of(&lines).as_bytes())).unwrap();
    lab.refresh();
    assert!(points(&lab).is_some_and(|p| p.points.len() >= 3));
    let st = app(&lab);
    let deep = content_path(90);
    assert_eq!(get(&st, &format!("{API}{deep}")).await.status, 200);

    // the same size and time, other bytes inside the span's lines (its id, in its start and end lines): the seek
    // point is used, and the lines read there are not the span's events
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    let id = uid(1000 + 90 * 10 + 1);
    let mut changed = 0;
    let mut at = 0;
    while let Some(p) = bytes[at..]
        .windows(id.len())
        .position(|w| w == id.as_bytes())
    {
        let last = at + p + id.len() - 1;
        bytes[last] = if bytes[last] == b'0' { b'1' } else { b'0' };
        changed += 1;
        at = last + 1;
    }
    assert_eq!(changed, 2);
    std::fs::write(&path, &bytes).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let r = get(&st, &format!("{API}{deep}")).await;
    assert_eq!(
        (r.status, r.code()),
        (409, "content_unavailable".into()),
        "{}",
        r.text()
    );

    // another content under the same name: the points belong to another file, and the head differs
    std::fs::write(&path, gzip_stored(text_of(&traces(5, 10)).as_bytes())).unwrap();
    let r = get(&st, &format!("{API}{deep}")).await;
    assert_eq!((r.status, r.code()), (409, "content_unavailable".into()));
}

#[tokio::test]
async fn test_seek_point_memory_is_bounded() {
    // a spacing so small that the cap is reached: the points thin out and stay under the cap
    let lab = Lab::empty(with_every(4 * 1024));
    let lines = traces(400, 30_000);
    std::fs::write(
        lab.atof.join("relay-events.jsonl.1.gz"),
        gzip_stored(text_of(&lines).as_bytes()),
    )
    .unwrap();
    lab.refresh();
    let p = points(&lab).unwrap();
    assert!(p.points.len() <= CHECKPOINT_CAP, "{}", p.points.len());
    assert!(p.points.len() >= CHECKPOINT_CAP / 2, "{}", p.points.len());
    assert!(
        p.memory() <= CHECKPOINT_CAP * (32 * 1024 + 256),
        "{}",
        p.memory()
    );
    // and still right
    let st = app(&lab);
    let c = ok(&st, &content_path(299)).await;
    assert_eq!(c["response"]["parts"][0]["text"], "answer 299");
}

/// The deepest content read of a ~48 MiB rotation with the production spacing (32 MiB) and without seek points.
/// Prints both times and the memory of the points (run with `--nocapture` to see them).
#[tokio::test]
async fn test_deepest_content_read_timing() {
    let lines = traces(48, 1024 * 1024);
    let gz = gzip_stored(text_of(&lines).as_bytes());
    let with = Lab::empty(config());
    let without = Lab::empty(with_every(u64::MAX));
    for lab in [&with, &without] {
        std::fs::write(lab.atof.join("relay-events.jsonl.1.gz"), &gz).unwrap();
        lab.refresh();
    }
    let p = points(&with).unwrap();
    assert_eq!(p.points.len(), 1, "one point at 32 MiB");
    let (a, b) = (app(&with), app(&without));
    let deepest = content_path(47);
    let t0 = Instant::now();
    let x = ok(&a, &deepest).await;
    let t_with = t0.elapsed();
    let t0 = Instant::now();
    let y = ok(&b, &deepest).await;
    let t_without = t0.elapsed();
    assert_eq!(x, y);
    let _: &Value = &x;
    println!(
        "deepest content read of a {:.1} MiB rotation: {t_with:?} through a seek point, {t_without:?} without; \
         {} point(s), {} bytes of seek-point memory",
        text_of(&lines).len() as f64 / (1024.0 * 1024.0),
        p.points.len(),
        p.memory()
    );
}
