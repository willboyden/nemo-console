"""SQLite (WAL) with tiny forward-only migrations. One connection guarded by a lock: the console is single-node."""
from __future__ import annotations

import os
import sqlite3
import threading
from collections.abc import Iterable, Sequence
from pathlib import Path
from typing import Any

MIGRATIONS = Path(__file__).resolve().parent.parent / "migrations"


def ensure_private_dir(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)
    os.chmod(path, 0o700)


class Store:
    def __init__(self, state_dir: Path) -> None:
        ensure_private_dir(state_dir)
        self.path = state_dir / "console.db"
        fd = os.open(self.path, os.O_CREAT | os.O_RDWR, 0o600)
        try:
            os.fchmod(fd, 0o600)
        finally:
            os.close(fd)
        self._lock = threading.Lock()
        self._db = sqlite3.connect(self.path, check_same_thread=False, isolation_level=None)
        self._db.row_factory = sqlite3.Row
        self._db.execute("PRAGMA journal_mode=WAL")
        self._db.execute("PRAGMA busy_timeout=5000")
        self._db.execute("PRAGMA foreign_keys=ON")
        self._migrate()

    def _migrate(self) -> None:
        with self._lock:
            self._db.execute("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY)")
            done = {r[0] for r in self._db.execute("SELECT version FROM schema_version")}
            for f in sorted(MIGRATIONS.glob("[0-9][0-9][0-9][0-9]_*.sql")):
                version = int(f.name[:4])
                if version in done:
                    continue
                self._db.executescript("BEGIN;\n" + f.read_text() + f"\nINSERT INTO schema_version VALUES ({version});\nCOMMIT;")

    def execute(self, sql: str, params: Sequence[Any] = ()) -> None:
        with self._lock:
            self._db.execute(sql, params)

    def executemany(self, sql: str, rows: Iterable[Sequence[Any]]) -> None:
        with self._lock:
            self._db.execute("BEGIN")
            try:
                self._db.executemany(sql, rows)
            except Exception:
                self._db.execute("ROLLBACK")
                raise
            self._db.execute("COMMIT")

    def query(self, sql: str, params: Sequence[Any] = ()) -> list[sqlite3.Row]:
        with self._lock:
            return self._db.execute(sql, params).fetchall()

    def close(self) -> None:
        with self._lock:
            self._db.close()
