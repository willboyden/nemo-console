//! Bearer keys: stored as sha256 only, key files written 0600 with fchmod, roles admin/viewer, loopback trust.
//! Port of `services/auth.py`. Contract (PORTING.md section 5.2): key format `nck_` + 43 url-safe base64 chars, id =
//! 12 hex chars, `keys.sha256` = hex sha256 of the key; verification is constant-time over EVERY row.

use std::fs::{OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use subtle::ConstantTimeEq;

use crate::domain::models::{Role, now_ts};
use crate::services::common::{KNOWN, sha256_hex};
use crate::services::store::{Store, StoreError};

pub const LOOPBACK_CLIENTS: [&str; 3] = ["127.0.0.1", "::1", "localhost"];

/// The caller's role, `public` when unauthenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalRole {
    Public,
    Viewer,
    Admin,
}

impl PrincipalRole {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            PrincipalRole::Public => "public",
            PrincipalRole::Viewer => "viewer",
            PrincipalRole::Admin => "admin",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub name: String,
    pub role: PrincipalRole,
}

impl Principal {
    #[must_use]
    pub fn public() -> Principal {
        Principal {
            name: "public".to_owned(),
            role: PrincipalRole::Public,
        }
    }
}

/// Create exclusively with 0600 and fchmod (umask cannot widen it).
pub fn write_private_file(path: &Path, data: &str) -> std::io::Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.set_permissions(Permissions::from_mode(0o600))?;
    f.write_all(data.as_bytes())
}

/// `secrets.token_urlsafe(n)`.
#[must_use]
pub fn token_urlsafe(n: usize) -> String {
    let mut buf = vec![0u8; n];
    if getrandom::fill(&mut buf).is_err() {
        // never fall back to a predictable key
        #[allow(clippy::panic)]
        {
            panic!("the OS random source failed")
        }
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// `uuid.uuid4().hex`.
#[must_use]
pub fn uuid4_hex() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

pub struct AuthService {
    pub store: Arc<Store>,
    pub state_dir: PathBuf,
    pub trust_loopback: bool,
}

impl AuthService {
    /// Returns (key_id, plaintext). The plaintext is shown once by the caller and never stored.
    pub fn mint(&self, role: Role, note: &str) -> Result<(String, String), StoreError> {
        let key = format!("nck_{}", token_urlsafe(32));
        let kid: String = uuid4_hex().chars().take(12).collect();
        KNOWN.add(&key);
        self.store.execute(
            "INSERT INTO keys(id, role, sha256, created, note) VALUES (?,?,?,?,?)",
            rusqlite::params![
                kid,
                role.as_str(),
                sha256_hex(key.as_bytes()),
                now_ts(),
                note
            ],
        )?;
        Ok((kid, key))
    }

    /// First start: mint an admin key and write it to `<state>/bootstrap-admin.key` (0600). None when keys exist.
    pub fn ensure_bootstrap_key(&self) -> Result<Option<PathBuf>, StoreError> {
        let any: bool = self.store.with(|db| {
            let mut s = db.prepare("SELECT 1 FROM keys LIMIT 1")?;
            s.exists([])
        })?;
        if any {
            return Ok(None);
        }
        let (_, key) = self.mint(Role::Admin, "bootstrap")?;
        let path = self.state_dir.join("bootstrap-admin.key");
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        write_private_file(&path, &format!("{key}\n"))?;
        Ok(Some(path))
    }

    /// Constant-ish time: every row is compared, no early exit.
    #[must_use]
    pub fn authenticate(&self, token: &str) -> Option<Principal> {
        let digest = sha256_hex(token.as_bytes());
        let rows: Vec<(String, String, String)> = self
            .store
            .with(|db| {
                let mut s = db.prepare("SELECT id, role, sha256 FROM keys")?;
                s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect()
            })
            .unwrap_or_default();
        let mut found = None;
        for (id, role, sha) in rows {
            if bool::from(sha.as_bytes().ct_eq(digest.as_bytes())) {
                let role = if role == "admin" {
                    PrincipalRole::Admin
                } else {
                    PrincipalRole::Viewer
                };
                found = Some(Principal {
                    name: format!("key:{id}"),
                    role,
                });
            }
        }
        found
    }

    #[must_use]
    pub fn principal_for(
        &self,
        authorization: Option<&str>,
        client_host: Option<&str>,
        forwarded: bool,
    ) -> Option<Principal> {
        if let Some(auth) = authorization.filter(|a| !a.is_empty()) {
            let (scheme, tok) = auth.split_once(' ').unwrap_or((auth, ""));
            if !scheme.eq_ignore_ascii_case("bearer") || tok.trim().is_empty() {
                return None;
            }
            return self.authenticate(tok.trim());
        }
        if self.trust_loopback
            && client_host.is_some_and(|h| LOOPBACK_CLIENTS.contains(&h))
            && !forwarded
        {
            return Some(Principal {
                name: "loopback".to_owned(),
                role: PrincipalRole::Admin,
            });
        }
        None
    }
}
