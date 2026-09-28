"""Config files (ADR 4/5): CAS + backup + atomic write + adapter post_apply + AUTO-ROLLBACK. The adapter never writes."""
from __future__ import annotations

import asyncio
import logging
import os
import tempfile
import time
import uuid
from pathlib import Path

from nemo_console.adapters.base import SystemAdapter
from nemo_console.domain.models import (
    ApplyResult,
    ConfigFile,
    ConfigFileSpec,
    ConfigRevision,
    SystemId,
    ValidationResult,
)
from nemo_console.services.common import cap, find_secret, redact, sha256_hex
from nemo_console.services.errors import ApiError
from nemo_console.services.store import Store

log = logging.getLogger(__name__)


def atomic_write(path: Path, data: bytes, mode: int) -> None:
    fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.", suffix=".tmp")
    try:
        try:
            view = memoryview(data)
            while view:
                view = view[os.write(fd, view):]
            os.fchmod(fd, mode)
            os.fsync(fd)
        finally:
            os.close(fd)
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except FileNotFoundError:
            pass
        raise
    dfd = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(dfd)
    finally:
        os.close(dfd)


class ConfigStore:
    def __init__(self, store: Store, adapters: dict[SystemId, SystemAdapter], cap_bytes: int, keep: int,
                 validate_sem: asyncio.Semaphore | None = None) -> None:
        self.validate_sem = validate_sem or asyncio.Semaphore(2)     # bounds parallel dry-runs (each may `docker run`)
        self.store = store
        self.adapters = adapters
        self.cap_bytes = cap_bytes
        self.keep = keep
        self._locks: dict[tuple[str, str], asyncio.Lock] = {}
        # allowlist fixed at startup: (system, file_id) -> resolved absolute path
        self.allow: dict[tuple[SystemId, str], Path] = {}
        for sid, a in adapters.items():
            try:
                for spec in a.config_files():
                    p = a.config_path(spec.id)
                    if p.is_absolute():
                        self.allow[(sid, spec.id)] = p.resolve()
            except Exception:  # noqa: BLE001
                log.warning("adapter %s config allowlist could not be built", sid)

    # ── lookups ─────────────────────────────────────────────────────────────────────────────
    def spec(self, a: SystemAdapter, file_id: str) -> ConfigFileSpec:
        for s in a.config_files():
            if s.id == file_id:
                return s
        raise ApiError(404, "not_found", "no such config file")

    def path(self, a: SystemAdapter, file_id: str) -> Path:
        allowed = self.allow.get((a.id, file_id))
        if allowed is None:
            raise ApiError(404, "not_found", "no such config file")
        try:
            p = a.config_path(file_id)
        except KeyError:
            raise ApiError(404, "not_found", "no such config file") from None
        if not p.is_absolute() or p.is_symlink():
            raise ApiError(403, "symlink_refused", "config path is a symlink or not absolute; refusing")
        if p.resolve() != allowed:
            raise ApiError(403, "path_changed", "config path no longer matches the startup allowlist")
        return p

    def _read(self, path: Path) -> bytes:
        try:
            if path.stat().st_size > self.cap_bytes:
                raise ApiError(413, "config_too_large", f"file exceeds {self.cap_bytes} bytes")
            return path.read_bytes()
        except FileNotFoundError:
            return b""          # not created yet: an editable file starts as empty content

    async def read(self, a: SystemAdapter, file_id: str) -> ConfigFile:
        self.spec(a, file_id)
        path = self.path(a, file_id)
        try:
            cf = await a.read_config(file_id)
        except FileNotFoundError:
            raise ApiError(404, "not_found", "config file does not exist") from None
        data = self._read(path)     # the core's sha (of raw bytes) is the one CAS uses
        mtime = path.stat().st_mtime if path.exists() else 0.0
        return cf.model_copy(update={"sha256": sha256_hex(data), "mtime": mtime})

    async def validate(self, a: SystemAdapter, file_id: str, content: str) -> ValidationResult:
        self.spec(a, file_id)
        self._check_content(content, secrets=True)
        return await self._validate(a, file_id, content)

    async def _validate(self, a: SystemAdapter, file_id: str, content: str) -> ValidationResult:
        async with self.validate_sem:
            return await a.validate_config(file_id, content)

    # ── apply ───────────────────────────────────────────────────────────────────────────────
    def _check_content(self, content: str, *, secrets: bool) -> bytes:
        data = content.encode("utf-8")
        if len(data) > self.cap_bytes:
            raise ApiError(413, "config_too_large", f"content exceeds {self.cap_bytes} bytes")
        if "\x00" in content:
            raise ApiError(422, "invalid_content", "content contains a NUL byte")
        if secrets and (why := find_secret(content)):
            raise ApiError(422, "secret_in_content", f"refusing content that contains {why}")
        return data

    def _lock(self, system: str, file_id: str) -> asyncio.Lock:
        return self._locks.setdefault((system, file_id), asyncio.Lock())

    def _backup(self, a: SystemAdapter, file_id: str, data: bytes, actor: str, note: str) -> str:
        rid = uuid.uuid4().hex[:16]
        self.store.execute(
            "INSERT INTO revisions(id, system, file_id, ts, actor, sha256, note, content) VALUES (?,?,?,?,?,?,?,?)",
            (rid, a.id, file_id, time.time(), actor, sha256_hex(data), note, data))
        self.store.execute(
            "DELETE FROM revisions WHERE system=? AND file_id=? AND id NOT IN "
            "(SELECT id FROM revisions WHERE system=? AND file_id=? ORDER BY ts DESC, rowid DESC LIMIT ?)",
            (a.id, file_id, a.id, file_id, self.keep))
        return rid

    async def apply(self, a: SystemAdapter, file_id: str, *, content: str, base_sha256: str, restart: bool,
                    confirm: bool, actor: str) -> ApplyResult:
        spec = self.spec(a, file_id)
        if not spec.editable:
            raise ApiError(403, "read_only", spec.why_readonly or "this file is read-only in v1")
        if not confirm:
            raise ApiError(400, "confirm_required", "confirm:true is required to apply")
        data = self._check_content(content, secrets=True)
        async with self._lock(a.id, file_id):
            path = self.path(a, file_id)
            vr = await self._validate(a, file_id, content)
            if not vr.ok:
                msgs = "; ".join(d.message for d in vr.diagnostics[:3])
                raise ApiError(422, "validation_failed", redact(f"{vr.validator}: {msgs}")[:500])
            return await self._write_and_check(a, file_id, path, data, base_sha256, restart, actor, "pre-apply backup")

    async def rollback(self, a: SystemAdapter, file_id: str, *, revision_id: str, confirm: bool, actor: str) -> ApplyResult:
        spec = self.spec(a, file_id)
        if not spec.editable:
            raise ApiError(403, "read_only", spec.why_readonly or "this file is read-only in v1")
        if not confirm:
            raise ApiError(400, "confirm_required", "confirm:true is required to roll back")
        rows = self.store.query("SELECT content FROM revisions WHERE id=? AND system=? AND file_id=?",
                                (revision_id, a.id, file_id))
        if not rows:
            raise ApiError(404, "not_found", "no such revision")
        target = bytes(rows[0]["content"])
        try:
            target_text = target.decode("utf-8")
        except UnicodeDecodeError:
            raise ApiError(422, "invalid_revision", "the stored revision is not valid UTF-8; refusing to restore it") from None
        if why := find_secret(target_text):
            raise ApiError(422, "secret_in_content", f"refusing to restore a revision that contains {why}")
        vr = await self._validate(a, file_id, target_text)
        if not vr.ok:
            msgs = "; ".join(d.message for d in vr.diagnostics[:3])
            raise ApiError(422, "validation_failed", redact(f"revision no longer validates ({vr.validator}): {msgs}")[:500])
        async with self._lock(a.id, file_id):
            path = self.path(a, file_id)
            current = sha256_hex(self._read(path))
            return await self._write_and_check(a, file_id, path, target, current, True, actor,
                                               f"pre-rollback backup (to {revision_id})")

    async def _write_and_check(self, a: SystemAdapter, file_id: str, path: Path, data: bytes, base_sha: str,
                               restart: bool, actor: str, note: str) -> ApplyResult:
        old = self._read(path)
        old_sha = sha256_hex(old)
        if old_sha != base_sha:
            raise ApiError(409, "stale_base", "the file changed since you loaded it; reload and re-apply")
        existed = path.exists()
        mode = (path.stat().st_mode & 0o777) if existed else 0o600
        if not existed:
            path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        rid = self._backup(a, file_id, old, actor, note)
        atomic_write(path, data, mode)
        new_sha = sha256_hex(data)
        result = await self._post(a, file_id, restart)
        if result.post_check == "unhealthy":
            if existed:
                atomic_write(path, old, mode)
            else:
                path.unlink(missing_ok=True)      # it did not exist before: roll back to "absent"
            again = await self._post(a, file_id, restart)
            detail = f"post-apply health check failed; restored the previous file. {result.detail}"
            if again.post_check == "unhealthy":
                detail += " NOTE: still unhealthy after restoring the backup - investigate manually."
            return result.model_copy(update={"ok": False, "revision_id": rid, "new_sha256": old_sha,
                                             "rolled_back": True, "detail": cap(redact(detail), 2000)})
        return result.model_copy(update={"revision_id": rid, "new_sha256": new_sha, "detail": cap(redact(result.detail), 2000)})

    @staticmethod
    async def _post(a: SystemAdapter, file_id: str, restart: bool) -> ApplyResult:
        try:
            return await a.post_apply(file_id, restart=restart)
        except Exception as e:  # noqa: BLE001 - a crashing post-apply is an unhealthy apply
            return ApplyResult(ok=False, restarted=restart, post_check="unhealthy",
                               detail=redact(f"post_apply raised {type(e).__name__}: {e}")[:300])

    def history(self, a: SystemAdapter, file_id: str) -> list[ConfigRevision]:
        self.spec(a, file_id)
        rows = self.store.query("SELECT id, ts, actor, sha256, note FROM revisions WHERE system=? AND file_id=? "
                                "ORDER BY ts DESC, rowid DESC", (a.id, file_id))
        return [ConfigRevision(id=r["id"], ts=r["ts"], actor=r["actor"], sha256=r["sha256"], note=r["note"]) for r in rows]
