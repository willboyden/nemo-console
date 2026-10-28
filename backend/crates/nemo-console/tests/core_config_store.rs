//! Port of backend/tests/core/test_config_store.py: CAS + backup + atomic write + auto-rollback, refusals.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test helpers outside #[test] fns
mod common;

use std::path::PathBuf;

use common::{Env, TestResp, apply_body, mode_of};
use nemo_console::domain::models::SystemId;
use nemo_console::services::common::find_secret;
use serde_json::{Value, json};

const URL: &str = "/api/v1/systems/switchyard/config/routes";

async fn apply(env: &Env, content: &str, base: Option<&str>, extra: Value) -> (u16, Value) {
    let sha = env.sha();
    let mut body = apply_body(content, base.unwrap_or(&sha));
    if let (Some(b), Value::Object(e)) = (body.as_object_mut(), extra) {
        b.extend(e);
    }
    let r: TestResp = env.post(&format!("{URL}/apply"), body).send().await;
    (r.status, r.json())
}

fn read(env: &Env) -> String {
    std::fs::read_to_string(&env.cfg_file).unwrap()
}

fn post_calls(env: &Env) -> Vec<bool> {
    env.a(SystemId::Switchyard)
        .post_calls
        .lock()
        .unwrap()
        .clone()
}

#[tokio::test]
async fn test_apply_happy_path_backs_up_and_writes_atomically() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let old = read(&env);
    let (code, body) = apply(&env, "a = 2\n", None, json!({})).await;
    assert_eq!(code, 200, "{body}");
    assert!(
        body["ok"].as_bool().unwrap()
            && body["post_check"] == "healthy"
            && body["restarted"] == true
    );
    assert_eq!(body["rolled_back"], false);
    assert_eq!(read(&env), "a = 2\n");
    assert_eq!(mode_of(&env.cfg_file), 0o640); // mode preserved
    let litter: Vec<PathBuf> = std::fs::read_dir(env.cfg_file.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap() != "routes.toml")
        .collect();
    assert!(litter.is_empty(), "{litter:?}"); // no temp litter
    assert_eq!(body["new_sha256"], env.sha());
    assert_eq!(post_calls(&env), [true]);
    let hist = env.get(&format!("{URL}/history")).send().await.json();
    assert_eq!(hist.as_array().unwrap().len(), 1);
    assert_eq!(hist[0]["id"], body["revision_id"]);
    let rid = body["revision_id"].as_str().unwrap().to_owned();
    let content: Vec<u8> = env
        .state
        .store
        .with(|db| {
            db.query_row("SELECT content FROM revisions WHERE id=?", [&rid], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert_eq!(String::from_utf8(content).unwrap(), old);
}

#[tokio::test]
async fn test_get_config_sha_is_of_raw_file_bytes() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let r = env.get(URL).send().await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["sha256"], env.sha());
}

#[tokio::test]
async fn test_stale_base_sha_is_409_and_file_untouched() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let (code, body) = apply(&env, "a = 2\n", Some(&"0".repeat(64)), json!({})).await;
    assert_eq!(code, 409);
    assert_eq!(body["error"]["code"], "stale_base");
    assert_eq!(read(&env), "a = 1\n");
    assert!(post_calls(&env).is_empty());
}

#[tokio::test]
async fn test_concurrent_edit_between_load_and_apply_is_409() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let base = env.sha();
    std::fs::write(&env.cfg_file, "a = 99\n").unwrap(); // someone else edits
    let (code, _) = apply(&env, "a = 2\n", Some(&base), json!({})).await;
    assert_eq!(code, 409);
}

#[tokio::test]
async fn test_confirm_is_required() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let (code, body) = apply(&env, "a = 2\n", None, json!({"confirm": false})).await;
    assert_eq!(code, 400);
    assert_eq!(body["error"]["code"], "confirm_required");
    assert_eq!(read(&env), "a = 1\n");
}

#[tokio::test]
async fn test_bad_sha_format_is_422() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let (code, _) = apply(&env, "a = 2\n", Some("XYZ"), json!({})).await;
    assert_eq!(code, 422);
}

#[tokio::test]
async fn test_secret_looking_content_is_refused() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    for bad in [
        "api_key = \"sk-abcdefghijklmnop1234\"\n",
        "h = \"Authorization: Bearer abcdefghijklmnopqrstuv\"\n",
        "api_key = \"literalsecretvalue\"\n",
        "token: hunter2hunter2\n",
        "k = \"hf_abcdefghijklmnopqrstuvwx\"\n",
        "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----\n",
    ] {
        let (code, body) = apply(&env, bad, None, json!({})).await;
        assert_eq!(code, 422, "{bad}");
        assert_eq!(body["error"]["code"], "secret_in_content", "{bad}");
        let s = body.to_string();
        assert!(
            !s.contains("abcdefghijklmnop") && !s.contains("hunter2"),
            "{s}"
        );
    }
    assert_eq!(read(&env), "a = 1\n");
    // validate never accepts it either
    let r = env
        .post(
            &format!("{URL}/validate"),
            json!({"content": "api_key = \"sk-abcdefghijklmnop1234\""}),
        )
        .send()
        .await;
    assert_eq!(r.status, 422);
}

#[tokio::test]
async fn test_env_var_names_are_fine() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let good = "api_key_env = \"LITELLM_MASTER_KEY\"\napi_key = \"LITELLM_MASTER_KEY\"\nmax_tokens = 4096\nkey: os.environ/LITELLM_KEY\n";
    let (code, _) = apply(&env, good, None, json!({})).await;
    assert_eq!(code, 200);
}

#[test]
fn test_real_repo_route_files_pass_the_secret_scan() {
    // skipped (like Python's pytest.skip) when this checkout is not inside a lab repository
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let Some(root) = here
        .ancestors()
        .find(|p| p.join("inference").join("switchyard").is_dir())
    else {
        return;
    };
    for rel in [
        "inference/switchyard/routes.toml",
        "clients/nemo-relay/routes.toml",
    ] {
        if let Ok(text) = std::fs::read_to_string(root.join(rel)) {
            assert!(find_secret(&text).is_none(), "{rel}");
        }
    }
}

#[tokio::test]
async fn test_symlink_is_refused() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let real = env.cfg_file.parent().unwrap().join("real.toml");
    std::fs::write(&real, "a = 1\n").unwrap();
    std::fs::remove_file(&env.cfg_file).unwrap();
    std::os::unix::fs::symlink(&real, &env.cfg_file).unwrap();
    let (code, body) = apply(&env, "a = 2\n", Some(&"0".repeat(64)), json!({})).await;
    assert_eq!(code, 403);
    assert_eq!(body["error"]["code"], "symlink_refused");
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "a = 1\n");
}

#[tokio::test]
async fn test_parent_dir_swapped_for_symlink_is_refused() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let elsewhere = env.tmp().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("routes.toml"), "a = 1\n").unwrap();
    std::fs::rename(env.cfg_file.parent().unwrap(), env.tmp().join("cfg_moved")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, env.tmp().join("cfg")).unwrap();
    let (code, body) = apply(&env, "a = 2\n", Some(&"0".repeat(64)), json!({})).await;
    assert_eq!(code, 403);
    assert_eq!(body["error"]["code"], "path_changed");
    assert_eq!(
        std::fs::read_to_string(elsewhere.join("routes.toml")).unwrap(),
        "a = 1\n"
    );
}

#[tokio::test]
async fn test_size_cap() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let (code, body) = apply(
        &env,
        &format!("a = '{}'\n", "x".repeat(300_000)),
        None,
        json!({}),
    )
    .await;
    assert_eq!(code, 413);
    assert_eq!(body["error"]["code"], "config_too_large");
}

#[tokio::test]
async fn test_validation_failure_blocks_write() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let (code, body) = apply(&env, "INVALID = 1\n", None, json!({})).await;
    assert_eq!(code, 422);
    assert_eq!(body["error"]["code"], "validation_failed");
    assert_eq!(read(&env), "a = 1\n");
    assert!(post_calls(&env).is_empty());
}

#[tokio::test]
async fn test_read_only_file_is_403() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.a(SystemId::Switchyard).knobs().not_editable = true;
    let (code, body) = apply(&env, "a = 2\n", None, json!({})).await;
    assert_eq!(code, 403);
    assert_eq!(body["error"]["code"], "read_only");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("policy file")
    );
}

#[tokio::test]
async fn test_unknown_file_is_404() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    assert_eq!(
        env.get("/api/v1/systems/switchyard/config/nope")
            .send()
            .await
            .status,
        404
    );
    assert_eq!(
        env.get("/api/v1/systems/switchyard/config/..%2f..%2fetc")
            .send()
            .await
            .status,
        404
    );
    assert_eq!(
        env.get("/api/v1/systems/relay/config/routes")
            .send()
            .await
            .status,
        404
    );
}

#[tokio::test]
async fn test_auto_rollback_when_unhealthy() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let (code, body) = apply(&env, "a = 'BAD'\n", None, json!({})).await;
    assert_eq!(code, 200);
    assert_eq!(
        (body["ok"].as_bool(), body["rolled_back"].as_bool()),
        (Some(false), Some(true))
    );
    assert_eq!(body["post_check"], "unhealthy");
    assert_eq!(read(&env), "a = 1\n"); // previous file restored
    assert_eq!(body["new_sha256"], env.sha());
    assert_eq!(post_calls(&env), [true, true]); // restarted again after the restore
    assert!(
        body["detail"]
            .as_str()
            .unwrap()
            .contains("restored the previous file")
    );
}

#[tokio::test]
async fn test_auto_rollback_when_post_apply_raises() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.a(SystemId::Switchyard).knobs().post_health_raise = true;
    let (code, body) = apply(&env, "a = 3\n", None, json!({})).await;
    assert_eq!(code, 200);
    assert_eq!(body["rolled_back"], true);
    assert_eq!(read(&env), "a = 1\n");
}

#[tokio::test]
async fn test_manual_rollback_to_a_revision() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    let (_, first) = apply(&env, "a = 2\n", None, json!({})).await;
    let rid = first["revision_id"].as_str().unwrap();
    let r = env
        .post(
            &format!("{URL}/rollback"),
            json!({"revision_id": rid, "confirm": false}),
        )
        .send()
        .await;
    assert_eq!(r.status, 400);
    let r = env
        .post(
            &format!("{URL}/rollback"),
            json!({"revision_id": "deadbeefdeadbeef", "confirm": true}),
        )
        .send()
        .await;
    assert_eq!(r.status, 404);
    let r = env
        .post(
            &format!("{URL}/rollback"),
            json!({"revision_id": rid, "confirm": true}),
        )
        .send()
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["ok"], true);
    assert_eq!(read(&env), "a = 1\n");
    assert_eq!(
        env.get(&format!("{URL}/history"))
            .send()
            .await
            .json()
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn test_history_is_pruned() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    env.state.config_store.set_keep(3);
    for i in 0..6 {
        let (code, _) = apply(&env, &format!("a = {}\n", i + 10), None, json!({})).await;
        assert_eq!(code, 200);
    }
    assert_eq!(
        env.get(&format!("{URL}/history"))
            .send()
            .await
            .json()
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test]
async fn test_file_written_with_expected_perms_after_umask() {
    use nix::sys::stat::{Mode, umask};
    let env = Env::new(Some("http://127.0.0.1:9"));
    let old = umask(Mode::from_bits_truncate(0o077));
    let (code, _) = apply(&env, "a = 5\n", None, json!({})).await;
    umask(old);
    assert_eq!(code, 200);
    assert_eq!(mode_of(&env.cfg_file), 0o640);
}

#[tokio::test]
async fn test_apply_is_audited_without_content() {
    let env = Env::new(Some("http://127.0.0.1:9"));
    apply(&env, "a = 'top secret marker zzz'\n", None, json!({})).await;
    let rows = env
        .get("/api/v1/audit?system=switchyard")
        .send()
        .await
        .json();
    assert_eq!(rows[0]["method"], "POST");
    assert_eq!(rows[0]["status"], 200);
    assert!(!rows.to_string().contains("zzz"));
    assert!(
        rows[0]["params"]["content"]
            .as_str()
            .unwrap()
            .contains("chars]")
    );
}
