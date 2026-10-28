//! Config files (ADR 4/5): CAS + backup + atomic write + adapter post_apply + AUTO-ROLLBACK. The adapter never
//! writes. Port of `services/config_store.py`. Contract (PORTING.md section 6.8): revisions table format, atomic write
//! (`.<name>.<random>.tmp` in the same dir, fchmod, fsync, rename, fsync dir), mode preservation.

use std::collections::HashMap;
use std::fs::Permissions;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use tokio::sync::Semaphore;

use crate::adapters::base::{AdapterError, AdapterRef};
use crate::domain::models::{
    ApplyResult, ConfigFile, ConfigFileSpec, ConfigRevision, PostCheck, SystemId, ValidationResult,
    now_ts,
};
use crate::services::auth::uuid4_hex;
use crate::services::common::RecoverLock as _;
use crate::services::common::{cap, find_secret, redact, sha256_hex};
use crate::services::errors::ApiError;
use crate::services::pyfmt::char_prefix;
use crate::services::settings::resolve_lenient;
use crate::services::store::Store;

/// Write `data` to `path` atomically with `mode` (temp file in the same dir, fsync, rename, fsync the dir).
pub fn atomic_write(path: &Path, data: &[u8], mode: u32) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut tmp = tempfile::Builder::new()
        .prefix(&format!(".{name}."))
        .suffix(".tmp")
        .tempfile_in(dir)?;
    tmp.write_all(data)?;
    tmp.as_file()
        .set_permissions(Permissions::from_mode(mode))?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    std::fs::File::open(dir)?.sync_all()
}

type FileLocks = HashMap<(SystemId, String), Arc<tokio::sync::Mutex<()>>>;

fn api_internal<E>(_: E) -> ApiError {
    ApiError::internal()
}

pub struct ConfigStore {
    pub store: Arc<Store>,
    pub adapters: IndexMap<SystemId, AdapterRef>,
    pub cap_bytes: usize,
    keep: Mutex<i64>,
    pub validate_sem: Arc<Semaphore>,
    locks: Mutex<FileLocks>,
    /// allowlist fixed at startup: (system, file_id) -> resolved absolute path
    allow: HashMap<(SystemId, String), PathBuf>,
}

impl ConfigStore {
    #[must_use]
    pub fn new(
        store: Arc<Store>,
        adapters: IndexMap<SystemId, AdapterRef>,
        cap_bytes: usize,
        keep: i64,
        validate_sem: Arc<Semaphore>,
    ) -> Self {
        let mut allow = HashMap::new();
        for (sid, a) in &adapters {
            let built: Result<(), AdapterError> = (|| {
                for spec in a.config_files()? {
                    let p = a.config_path(&spec.id)?;
                    if p.is_absolute() {
                        allow.insert((*sid, spec.id.clone()), resolve_lenient(&p));
                    }
                }
                Ok(())
            })();
            if built.is_err() {
                tracing::warn!("adapter {sid} config allowlist could not be built");
            }
        }
        ConfigStore {
            store,
            adapters,
            cap_bytes,
            keep: Mutex::new(keep),
            validate_sem,
            locks: Mutex::new(HashMap::new()),
            allow,
        }
    }

    /// Test seam (Python `config_store.keep = 3`).
    pub fn set_keep(&self, keep: i64) {
        if let Ok(mut k) = self.keep.lock_recovering() {
            *k = keep;
        }
    }

    pub fn spec(&self, a: &AdapterRef, file_id: &str) -> Result<ConfigFileSpec, ApiError> {
        let specs = a.config_files().map_err(api_internal)?;
        specs
            .into_iter()
            .find(|s| s.id == file_id)
            .ok_or_else(|| ApiError::new(404, "not_found", "no such config file"))
    }

    pub fn path(&self, a: &AdapterRef, file_id: &str) -> Result<PathBuf, ApiError> {
        let Some(allowed) = self.allow.get(&(a.id(), file_id.to_owned())) else {
            return Err(ApiError::new(404, "not_found", "no such config file"));
        };
        let p = match a.config_path(file_id) {
            Ok(p) => p,
            Err(AdapterError::UnknownId(_)) => {
                return Err(ApiError::new(404, "not_found", "no such config file"));
            }
            Err(_) => return Err(ApiError::internal()),
        };
        if !p.is_absolute()
            || p.symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err(ApiError::new(
                403,
                "symlink_refused",
                "config path is a symlink or not absolute; refusing",
            ));
        }
        if &resolve_lenient(&p) != allowed {
            return Err(ApiError::new(
                403,
                "path_changed",
                "config path no longer matches the startup allowlist",
            ));
        }
        Ok(p)
    }

    fn read_raw(&self, path: &Path) -> Result<Vec<u8>, ApiError> {
        match std::fs::metadata(path) {
            Ok(md) => {
                if md.len() > self.cap_bytes as u64 {
                    return Err(ApiError::new(
                        413,
                        "config_too_large",
                        format!("file exceeds {} bytes", self.cap_bytes),
                    ));
                }
                match std::fs::read(path) {
                    Ok(d) => Ok(d),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
                    Err(_) => Err(ApiError::internal()),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(_) => Err(ApiError::internal()),
        }
    }

    pub async fn read(&self, a: &AdapterRef, file_id: &str) -> Result<ConfigFile, ApiError> {
        self.spec(a, file_id)?;
        let path = self.path(a, file_id)?;
        let mut cf = match a.read_config(file_id).await {
            Ok(cf) => cf,
            Err(AdapterError::FileNotFound(_)) => {
                return Err(ApiError::new(
                    404,
                    "not_found",
                    "config file does not exist",
                ));
            }
            Err(_) => return Err(ApiError::internal()),
        };
        let data = self.read_raw(&path)?;
        cf.sha256 = sha256_hex(&data);
        cf.mtime = std::fs::metadata(&path)
            .ok()
            .and_then(|m| mtime_f64(&m))
            .unwrap_or(0.0);
        Ok(cf)
    }

    pub async fn validate(
        &self,
        a: &AdapterRef,
        file_id: &str,
        content: &str,
    ) -> Result<ValidationResult, ApiError> {
        self.spec(a, file_id)?;
        self.check_content(content, true)?;
        self.validate_inner(a, file_id, content).await
    }

    async fn validate_inner(
        &self,
        a: &AdapterRef,
        file_id: &str,
        content: &str,
    ) -> Result<ValidationResult, ApiError> {
        let _permit = self.validate_sem.acquire().await.map_err(api_internal)?;
        a.validate_config(file_id, content)
            .await
            .map_err(api_internal)
    }

    fn check_content(&self, content: &str, secrets: bool) -> Result<Vec<u8>, ApiError> {
        let data = content.as_bytes().to_vec();
        if data.len() > self.cap_bytes {
            return Err(ApiError::new(
                413,
                "config_too_large",
                format!("content exceeds {} bytes", self.cap_bytes),
            ));
        }
        if content.contains('\0') {
            return Err(ApiError::new(
                422,
                "invalid_content",
                "content contains a NUL byte",
            ));
        }
        if secrets && let Some(why) = find_secret(content) {
            return Err(ApiError::new(
                422,
                "secret_in_content",
                format!("refusing content that contains {why}"),
            ));
        }
        Ok(data)
    }

    fn file_lock(&self, system: SystemId, file_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut l = match self.locks.lock_recovering() {
            Ok(l) => l,
            Err(p) => p.into_inner(),
        };
        Arc::clone(l.entry((system, file_id.to_owned())).or_default())
    }

    fn backup(
        &self,
        a: &AdapterRef,
        file_id: &str,
        data: &[u8],
        actor: &str,
        note: &str,
    ) -> Result<String, ApiError> {
        let rid: String = uuid4_hex().chars().take(16).collect();
        let sys = a.id().as_str();
        let keep = self.keep.lock_recovering().map(|k| *k).unwrap_or(50);
        self.store
            .execute(
                "INSERT INTO revisions(id, system, file_id, ts, actor, sha256, note, content) VALUES (?,?,?,?,?,?,?,?)",
                rusqlite::params![rid, sys, file_id, now_ts(), actor, sha256_hex(data), note, data],
            )
            .map_err(api_internal)?;
        self.store
            .execute(
                "DELETE FROM revisions WHERE system=? AND file_id=? AND id NOT IN \
                 (SELECT id FROM revisions WHERE system=? AND file_id=? ORDER BY ts DESC, rowid DESC LIMIT ?)",
                rusqlite::params![sys, file_id, sys, file_id, keep],
            )
            .map_err(api_internal)?;
        Ok(rid)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn apply(
        &self,
        a: &AdapterRef,
        file_id: &str,
        content: &str,
        base_sha256: &str,
        restart: bool,
        confirm: bool,
        actor: &str,
    ) -> Result<ApplyResult, ApiError> {
        let spec = self.spec(a, file_id)?;
        if !spec.editable {
            return Err(read_only(&spec));
        }
        if !confirm {
            return Err(ApiError::new(
                400,
                "confirm_required",
                "confirm:true is required to apply",
            ));
        }
        let data = self.check_content(content, true)?;
        let lock = self.file_lock(a.id(), file_id);
        let _held = lock.lock().await;
        let path = self.path(a, file_id)?;
        let vr = self.validate_inner(a, file_id, content).await?;
        if !vr.ok {
            let msgs: Vec<&str> = vr
                .diagnostics
                .iter()
                .take(3)
                .map(|d| d.message.as_str())
                .collect();
            return Err(ApiError::new(
                422,
                "validation_failed",
                char_prefix(
                    &redact(&format!("{}: {}", vr.validator, msgs.join("; "))),
                    500,
                ),
            ));
        }
        self.write_and_check(
            a,
            file_id,
            &path,
            &data,
            base_sha256,
            restart,
            actor,
            "pre-apply backup",
        )
        .await
    }

    pub async fn rollback(
        &self,
        a: &AdapterRef,
        file_id: &str,
        revision_id: &str,
        confirm: bool,
        actor: &str,
    ) -> Result<ApplyResult, ApiError> {
        let spec = self.spec(a, file_id)?;
        if !spec.editable {
            return Err(read_only(&spec));
        }
        if !confirm {
            return Err(ApiError::new(
                400,
                "confirm_required",
                "confirm:true is required to roll back",
            ));
        }
        let sys = a.id().as_str();
        let target: Option<Vec<u8>> = self
            .store
            .with(|db| {
                let mut s = db.prepare(
                    "SELECT content FROM revisions WHERE id=? AND system=? AND file_id=?",
                )?;
                let mut rows = s.query(rusqlite::params![revision_id, sys, file_id])?;
                match rows.next()? {
                    Some(r) => r.get::<_, Vec<u8>>(0).map(Some),
                    None => Ok(None),
                }
            })
            .map_err(api_internal)?;
        let Some(target) = target else {
            return Err(ApiError::new(404, "not_found", "no such revision"));
        };
        let Ok(target_text) = String::from_utf8(target.clone()) else {
            return Err(ApiError::new(
                422,
                "invalid_revision",
                "the stored revision is not valid UTF-8; refusing to restore it",
            ));
        };
        if let Some(why) = find_secret(&target_text) {
            return Err(ApiError::new(
                422,
                "secret_in_content",
                format!("refusing to restore a revision that contains {why}"),
            ));
        }
        let vr = self.validate_inner(a, file_id, &target_text).await?;
        if !vr.ok {
            let msgs: Vec<&str> = vr
                .diagnostics
                .iter()
                .take(3)
                .map(|d| d.message.as_str())
                .collect();
            return Err(ApiError::new(
                422,
                "validation_failed",
                char_prefix(
                    &redact(&format!(
                        "revision no longer validates ({}): {}",
                        vr.validator,
                        msgs.join("; ")
                    )),
                    500,
                ),
            ));
        }
        let lock = self.file_lock(a.id(), file_id);
        let _held = lock.lock().await;
        let path = self.path(a, file_id)?;
        let current = sha256_hex(&self.read_raw(&path)?);
        let note = format!("pre-rollback backup (to {revision_id})");
        self.write_and_check(a, file_id, &path, &target, &current, true, actor, &note)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn write_and_check(
        &self,
        a: &AdapterRef,
        file_id: &str,
        path: &Path,
        data: &[u8],
        base_sha: &str,
        restart: bool,
        actor: &str,
        note: &str,
    ) -> Result<ApplyResult, ApiError> {
        let old = self.read_raw(path)?;
        let old_sha = sha256_hex(&old);
        if old_sha != base_sha {
            return Err(ApiError::new(
                409,
                "stale_base",
                "the file changed since you loaded it; reload and re-apply",
            ));
        }
        let existed = path.exists();
        let mode = if existed {
            std::fs::metadata(path)
                .map(|m| m.permissions().mode() & 0o777)
                .unwrap_or(0o600)
        } else {
            0o600
        };
        if !existed && let Some(parent) = path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .map_err(api_internal)?;
        }
        let rid = self.backup(a, file_id, &old, actor, note)?;
        atomic_write(path, data, mode).map_err(api_internal)?;
        let new_sha = sha256_hex(data);
        let mut result = post(a, file_id, restart).await;
        if result.post_check == PostCheck::Unhealthy {
            if existed {
                atomic_write(path, &old, mode).map_err(api_internal)?;
            } else {
                let _ = std::fs::remove_file(path);
            }
            let again = post(a, file_id, restart).await;
            let mut detail = format!(
                "post-apply health check failed; restored the previous file. {}",
                result.detail
            );
            if again.post_check == PostCheck::Unhealthy {
                detail.push_str(
                    " NOTE: still unhealthy after restoring the backup - investigate manually.",
                );
            }
            result.ok = false;
            result.revision_id = Some(rid);
            result.new_sha256 = Some(old_sha);
            result.rolled_back = true;
            result.detail = cap(&redact(&detail), 2000);
            return Ok(result);
        }
        result.revision_id = Some(rid);
        result.new_sha256 = Some(new_sha);
        result.detail = cap(&redact(&result.detail), 2000);
        Ok(result)
    }

    pub fn history(&self, a: &AdapterRef, file_id: &str) -> Result<Vec<ConfigRevision>, ApiError> {
        self.spec(a, file_id)?;
        self.store
            .with(|db| {
                let mut s = db.prepare(
                    "SELECT id, ts, actor, sha256, note FROM revisions WHERE system=? AND file_id=? ORDER BY ts DESC, rowid DESC",
                )?;
                s.query_map(rusqlite::params![a.id().as_str(), file_id], |r| {
                    Ok(ConfigRevision { id: r.get(0)?, ts: r.get(1)?, actor: r.get(2)?, sha256: r.get(3)?, note: r.get(4)? })
                })?
                .collect()
            })
            .map_err(api_internal)
    }
}

use std::os::unix::fs::DirBuilderExt;

fn read_only(spec: &ConfigFileSpec) -> ApiError {
    ApiError::new(
        403,
        "read_only",
        spec.why_readonly
            .clone()
            .filter(|w| !w.is_empty())
            .unwrap_or_else(|| "this file is read-only in v1".to_owned()),
    )
}

/// A crashing post-apply is an unhealthy apply.
async fn post(a: &AdapterRef, file_id: &str, restart: bool) -> ApplyResult {
    match a.post_apply(file_id, restart).await {
        Ok(r) => r,
        Err(e) => ApplyResult {
            ok: false,
            new_sha256: None,
            revision_id: None,
            restarted: restart,
            post_check: PostCheck::Unhealthy,
            detail: char_prefix(&redact(&format!("post_apply raised {}", e.py_text())), 300)
                .to_owned(),
            rolled_back: false,
        },
    }
}

/// Python's `os.stat().st_mtime`: seconds + nanoseconds * 1e-9 as one double.
#[must_use]
pub fn mtime_f64(md: &std::fs::Metadata) -> Option<f64> {
    use std::os::unix::fs::MetadataExt;
    #[allow(clippy::cast_precision_loss)]
    Some(md.mtime() as f64 + md.mtime_nsec() as f64 * 1e-9)
}
