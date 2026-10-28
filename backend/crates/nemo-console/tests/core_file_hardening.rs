//! Audit finding 9: file handling that a planted symlink or an oversized file cannot turn against the console.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::os::unix::fs::symlink;

use common::Env;
use nemo_console::adapters::aiperf::{MAX_EXPORT_BYTES, request_errors};
use nemo_console::adapters::datadesigner::files::{MAX_LINE_BYTES, copy2, for_each_line};
use nemo_console::services::plumbing::write_private;
use nemo_console::services::support::write_meta;
use serde_json::{Map, json};

#[tokio::test]
async fn a_symlinked_index_html_is_not_served() {
    let env = Env::new(None);
    let front = env.tmp().join("front");
    std::fs::create_dir_all(&front).unwrap();
    let outside = env.tmp().join("outside.html");
    std::fs::write(&outside, "<p>not the console</p>").unwrap();
    symlink(&outside, front.join("index.html")).unwrap();
    let r = env.get("/").send().await;
    assert_eq!(r.status, 404, "{}", r.text());
    std::fs::remove_file(front.join("index.html")).unwrap();
    std::fs::write(front.join("index.html"), "<p>console</p>").unwrap();
    assert_eq!(env.get("/").send().await.status, 200);
}

#[test]
fn private_writes_never_follow_a_planted_temp_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside");
    std::fs::write(&outside, "keep").unwrap();
    // the predictable temp names of the Python originals
    symlink(&outside, tmp.path().join(".x.json.tmp")).unwrap();
    symlink(&outside, tmp.path().join(".run.json.tmp")).unwrap();
    write_private(&tmp.path().join("x.json"), "{}").unwrap();
    write_meta(
        tmp.path(),
        &Map::from_iter([("a".to_owned(), json!(1))]),
        &[],
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
    use std::os::unix::fs::PermissionsExt as _;
    for f in ["x.json", "run.json"] {
        let md = std::fs::metadata(tmp.path().join(f)).unwrap();
        assert_eq!(md.permissions().mode() & 0o777, 0o600, "{f}");
    }
}

#[test]
fn line_reading_refuses_symlinks_and_bounds_a_newline_free_file() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("records.jsonl");
    std::fs::write(&real, "{\"a\": 1}\n").unwrap();
    let link = tmp.path().join("link.jsonl");
    symlink(&real, &link).unwrap();
    assert!(
        for_each_line(&link, |_| true).is_err(),
        "a symlink must not be followed"
    );
    let huge = tmp.path().join("huge.jsonl");
    std::fs::write(
        &huge,
        vec![b'x'; usize::try_from(MAX_LINE_BYTES).unwrap() + 10],
    )
    .unwrap();
    let mut longest = 0;
    for_each_line(&huge, |l| {
        longest = longest.max(l.len());
        true
    })
    .unwrap();
    assert!(
        longest as u64 <= MAX_LINE_BYTES,
        "a {longest}-byte line was handed over"
    );
}

#[test]
fn copy2_copies_a_symlink_as_a_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let secret = tmp.path().join("secret");
    std::fs::write(&secret, "do not copy me").unwrap();
    symlink(&secret, tmp.path().join("batch.parquet")).unwrap();
    copy2(
        &tmp.path().join("batch.parquet"),
        &tmp.path().join("copy.parquet"),
    )
    .unwrap();
    let md = std::fs::symlink_metadata(tmp.path().join("copy.parquet")).unwrap();
    assert!(
        md.file_type().is_symlink(),
        "follow_symlinks=False copies the link"
    );
}

#[test]
fn the_aiperf_export_is_read_up_to_its_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("profile_export.jsonl");
    let mut data = vec![b'x'; usize::try_from(MAX_EXPORT_BYTES).unwrap() + 1024];
    data.push(b'\n');
    data.extend_from_slice(br#"{"error": {"code": 500, "message": "boom"}}"#);
    data.push(b'\n');
    std::fs::write(&p, data).unwrap();
    // the error record lies past the cap: not read
    assert_eq!(request_errors(&p).0, 0);
}
