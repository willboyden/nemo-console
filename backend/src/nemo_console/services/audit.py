"""Append-only audit of every mutating call. Params are redacted; chat text and file bodies are never stored."""
from __future__ import annotations

import json
import logging
import re
import time
from collections import Counter, deque
from typing import Any

from nemo_console.services.common import redact, redact_obj
from nemo_console.services.store import Store

log = logging.getLogger(__name__)
_SYSTEM_IN_PATH = re.compile(r"/api/v1/systems/([a-z]+)/")
CONTENT_KEYS = {"content": "chars", "messages": "messages", "system": "chars", "rendered": "chars"}


def summarize_body(doc: Any) -> Any:
    """Redact a JSON request body; replace bulk/private fields with a size marker."""
    if not isinstance(doc, dict):
        return "[non-object body]"
    out: dict[str, Any] = {}
    for k, v in doc.items():
        if k in CONTENT_KEYS:
            n = len(v) if isinstance(v, str | list) else 0
            out[k] = f"[{n} {CONTENT_KEYS[k]}]"
        else:
            out[k] = v
    return redact_obj(out)


def params_blob(params: Any) -> str:
    """Valid JSON by construction: values are redacted per string via redact_obj; oversize becomes a marker object."""
    safe = redact_obj(params)
    blob = json.dumps(safe, default=str)
    if len(blob) <= 8000:
        return blob
    keys = sorted(safe)[:20] if isinstance(safe, dict) else []
    return json.dumps({"_truncated": True, "bytes": len(blob), "keys": keys})


def load_params(raw: str) -> Any:
    try:
        return json.loads(raw)
    except ValueError:
        return {"_unparseable": True}


class AuditLog:
    def __init__(self, store: Store) -> None:
        self.store = store

    def record(self, *, actor: str, role: str, method: str, path: str, status: int, params: Any) -> None:
        m = _SYSTEM_IN_PATH.match(path)
        blob = params_blob(params)
        self.store.execute(
            "INSERT INTO audit(ts, actor, role, method, path, system, status, params) VALUES (?,?,?,?,?,?,?,?)",
            (time.time(), actor, role, method, redact(path)[:300], m.group(1) if m else None, status, blob))

    def list(self, limit: int = 100, system: str | None = None) -> list[dict[str, Any]]:
        limit = max(1, min(limit, 500))
        if system:
            rows = self.store.query("SELECT * FROM audit WHERE system=? ORDER BY id DESC LIMIT ?", (system, limit))
        else:
            rows = self.store.query("SELECT * FROM audit ORDER BY id DESC LIMIT ?", (limit,))
        return [{"id": r["id"], "ts": r["ts"], "actor": r["actor"], "role": r["role"], "method": r["method"],
                 "path": r["path"], "system": r["system"], "status": r["status"], "params": load_params(r["params"])}
                for r in rows]


class DenialLog:
    """Unauthenticated/denied requests are attacker-controlled volume: never rows in the audit table.
    They are counted (bounded label set), kept in a small ring, and logged as ONE aggregated line per minute."""

    STATUSES = (401, 403, 421, 429)

    def __init__(self, interval: float = 60.0, ring: int = 100) -> None:
        self.interval = interval
        self.ring: deque[tuple[float, int, str]] = deque(maxlen=ring)
        self._pending: Counter[int] = Counter()
        self._last_log = time.monotonic()
        self.total: Counter[int] = Counter()

    def note(self, status: int, method: str) -> None:
        status = status if status in self.STATUSES else 403
        self.total[status] += 1
        self._pending[status] += 1
        self.ring.append((time.time(), status, method[:7]))
        now = time.monotonic()
        if now - self._last_log >= self.interval and self._pending:
            log.warning("denied requests in the last %ds: %s", int(now - self._last_log),
                        ", ".join(f"{k}={v}" for k, v in sorted(self._pending.items())))
            self._pending.clear()
            self._last_log = now
