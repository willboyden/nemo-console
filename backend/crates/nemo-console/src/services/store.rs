//! SQLite (WAL) with tiny forward-only migrations. One connection guarded by a lock. Port of `services/store.py`.
//!
//! Contract (PORTING.md section 5): same file (`<state>/console.db`, 0600), same schema and `schema_version` table, so
//! a database written by the Python console opens unchanged and vice versa.

use std::fs::{OpenOptions, Permissions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::Connection;

use crate::services::common::RecoverLock as _;

/// The migrations, embedded (a copy of `backend/src/nemo_console/migrations/`).
pub const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("../../migrations/0001_init.sql"))];

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Sql(#[from] rusqlite::Error),
    #[error("store lock poisoned")]
    Poisoned,
}

/// `mkdir -p` + chmod 0700.
pub fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, Permissions::from_mode(0o700))
}

pub struct Store {
    pub path: PathBuf,
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(state_dir: &Path) -> Result<Store, StoreError> {
        ensure_private_dir(state_dir)?;
        let path = state_dir.join("console.db");
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&path)?;
        f.set_permissions(Permissions::from_mode(0o600))?;
        drop(f);
        let conn = Connection::open(&path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Store {
            path,
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), StoreError> {
        self.with(|db| {
            db.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY)",
            )?;
            let mut stmt = db.prepare("SELECT version FROM schema_version")?;
            let done: Vec<i64> = stmt
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            for (version, sql) in MIGRATIONS {
                if done.contains(version) {
                    continue;
                }
                db.execute_batch(&format!(
                    "BEGIN;\n{sql}\nINSERT INTO schema_version VALUES ({version});\nCOMMIT;"
                ))?;
            }
            Ok(())
        })
    }

    /// Run `f` with the connection under the store lock.
    pub fn with<T>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, StoreError> {
        let guard = self
            .conn
            .lock_recovering()
            .map_err(|_| StoreError::Poisoned)?;
        Ok(f(&guard)?)
    }

    /// `execute(sql, params)`.
    pub fn execute<P: rusqlite::Params>(&self, sql: &str, params: P) -> Result<usize, StoreError> {
        self.with(|db| db.execute(sql, params))
    }
}
