"""Cross-implementation state, Python side (backend/PORTING.md section 5).

  write <dir>   build a state dir with the PYTHON services (keys, audit rows, config revisions, metric rollups, settings
                overrides, saved params) and write <dir>/console.db + <dir>/expected.json (what Python reads back).
                The committed fixture is backend/crates/nemo-console/tests/fixtures/xstate/ (Rust must read it the same).
  check <dir>   open a state dir the RUST services wrote (<dir>/console.db) with the PYTHON services and compare every
                read with <dir>/expected.json (what Rust read back). Exit 0 when equal.

    REF=$(node parity/ref.mjs) && cd backend && env -i HOME=$(mktemp -d) PATH=/usr/bin:/bin NEMO_CONSOLE_NO_USER_ENV=1 LANG=C.UTF-8 \\
        "$REF/.venv/bin/python" -I ../parity/gen/gen_xstate_fixture.py write ../backend/crates/nemo-console/tests/fixtures/xstate

Hermetic: no network, no subprocess, no real HOME; the keys are fixed, obviously fake values of this fixture only.
"""
from __future__ import annotations

import asyncio
import base64
import json
import secrets
import shutil
import sqlite3
import sys
import tempfile
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from nemo_console.adapters.base import AdapterContext, SystemAdapter
from nemo_console.config import Settings
from nemo_console.domain.models import (
    ApplyResult, ConfigFile, ConfigFileSpec, MetricPoint, ValidationResult,
)
from nemo_console.services.audit import AuditLog
from nemo_console.services import auth as auth_module
from nemo_console.services.auth import AuthService
from nemo_console.services.config_store import ConfigStore
from nemo_console.services.metrics import MetricsPoller
from nemo_console.services.settings import SettingsService
from nemo_console.services.status import StatusService
from nemo_console.services.store import Store

NOW = 1_793_000_000.0                     # a fixed "now" for the 1h/24h history reads (rollup buckets are absolute)
# hermetic: every upstream URL is a closed port (the same as the Rust test Env)
CLOSED = {k: "http://127.0.0.1:9" for k in ("switchyard", "relay", "dynamo", "dynamo_worker_metrics", "litellm", "dcgm",
                                            "prometheus", "guardrails")}
SERIES = [("sy.requests_total", {}), ("sy.tokens", {"kind": "prompt"}), ("sy.tokens", {"kind": "completion"})]


class FileAdapter(SystemAdapter):
    """Just enough adapter for the config store: one editable file `routes`, always valid and healthy."""
    id = "switchyard"
    label = "Switchyard"

    def __init__(self, path: Path) -> None:
        super().__init__(AdapterContext(runner=None, http=None, urls={}, paths={}))  # type: ignore[arg-type]
        self.path = path

    async def status(self): raise NotImplementedError          # noqa: ANN201
    async def metrics(self): raise NotImplementedError         # noqa: ANN201
    async def recent_events(self, limit: int = 100): return []  # noqa: ANN201
    async def follow_events(self, since_ts=None):  # noqa: ANN001, ANN201
        return
        yield

    def config_files(self) -> list[ConfigFileSpec]:
        return [ConfigFileSpec(id="routes", label="Routes", description="d", display_path="routes.toml", format="toml",
                               editable=True)]

    def config_path(self, file_id: str) -> Path:
        if file_id != "routes":
            raise KeyError(file_id)
        return self.path

    async def read_config(self, file_id: str) -> ConfigFile:
        return ConfigFile(spec=self.config_files()[0], content=self.path.read_text(), sha256="", mtime=0)

    async def validate_config(self, file_id: str, content: str) -> ValidationResult:
        return ValidationResult(ok=True, validator="fake", authoritative=True)

    async def post_apply(self, file_id: str, *, restart: bool) -> ApplyResult:
        return ApplyResult(ok=True, post_check="healthy", restarted=restart)

    def actions(self): return []                                # noqa: ANN201
    async def run_action(self, action_id, params): raise KeyError(action_id)  # noqa: ANN001, ANN201


def reads(state: Path, cfg_file: Path, keys: list[dict[str, str]]) -> dict[str, Any]:
    """Every read the console makes of persisted state, as JSON."""
    cfg = Settings(state_dir=state, repo_root=state / "repo", background_tasks=False, urls=CLOSED)
    store = Store(state)
    auth = AuthService(store, state, False)
    audit = AuditLog(store)
    settings = SettingsService(cfg, store)
    adapter = FileAdapter(cfg_file)
    cs = ConfigStore(store, {"switchyard": adapter}, cfg.config_cap_bytes, cfg.revisions_keep)  # type: ignore[dict-item]
    poller = MetricsPoller(StatusService({}, 2.0, 8.0), store, 2.0, 900)
    out: dict[str, Any] = {}
    out["keys"] = []
    for k in keys:
        p = auth.authenticate(k["key"])
        out["keys"].append({"label": k["label"], "principal": None if p is None else [p.name, p.role]})
    out["keys"].append({"label": "wrong", "principal": None if auth.authenticate("nck_wrong") is None else "?"})
    out["audit_all"] = audit.list(500, None)
    out["audit_dynamo"] = audit.list(500, "dynamo")
    out["history"] = [r.model_dump() for r in cs.history(adapter, "routes")]
    rows = store.query("SELECT id, content FROM revisions ORDER BY rowid")
    out["revisions"] = {r["id"]: base64.b64encode(bytes(r["content"])).decode() for r in rows}
    out["metrics"] = {f"{w}:{k}": poller.history("switchyard", w, k, now=NOW)
                      for w in ("1h", "24h") for k in ("sy.requests_total", "sy.tokens", "sy.tokens{kind=prompt}")}
    out["overrides"] = settings.overrides()
    out["effective_urls"] = settings.effective_urls()
    out["effective_key_env"] = settings.effective_key_env()
    out["effective_poll_interval"] = settings.effective_poll_interval()
    out["saved_params"] = {s: settings.saved_params(s) for s in ("dynamo", "relay")}
    out["schema_version"] = [r[0] for r in store.query("SELECT version FROM schema_version ORDER BY version")]
    store.close()
    return out


async def write_state(state: Path, cfg_file: Path) -> list[dict[str, str]]:
    cfg = Settings(state_dir=state, repo_root=state / "repo", background_tasks=False, urls=CLOSED)
    store = Store(state)
    auth = AuthService(store, state, False)
    keys = []
    for role, label in (("admin", "admin"), ("viewer", "viewer")):
        # obviously fake, fixed keys: the committed fixture must not look like a leaked credential
        auth_module.secrets = SimpleNamespace(token_urlsafe=lambda _n, _l=label: f"TEST-ONLY-not-a-secret-{_l}")
        kid, key = auth.mint(role, f"xstate {label}")      # type: ignore[arg-type]
        keys.append({"label": label, "key": key, "kid": kid})
    auth_module.secrets = secrets
    audit = AuditLog(store)
    audit.record(actor="key:abc", role="admin", method="PUT", path="/api/v1/settings", status=200,
                 params={"poll_interval_s": 7.5})
    audit.record(actor="loopback", role="admin", method="POST", path="/api/v1/systems/dynamo/actions/up", status=200,
                 params={"confirm": True, "params": {"note": "héllo ✓", "api_key": "x"}})
    audit.record(actor="key:abc", role="viewer", method="POST", path="/api/v1/systems/dynamo/actions/x", status=403,
                 params="[12 bytes, non-json]")
    audit.record(actor="key:abc", role="admin", method="POST", path="/api/v1/systems/relay/actions/x", status=422,
                 params={f"k{i}": "v" * 100 for i in range(100)})           # over 8000 chars -> truncation marker
    audit.record(actor="key:abc", role="admin", method="DELETE", path="/api/v1/systems", status=405, params=None)
    store.execute("INSERT INTO audit(ts, actor, role, method, path, system, status, params) VALUES (?,?,?,?,?,?,?,?)",
                  (1.5, "a", "admin", "POST", "/api/v1/x", None, 200, '{"token": [redacted]'))    # unparseable row
    settings = SettingsService(cfg, store)
    settings.update({"dynamo": "http://localhost:8021/"}, 7.5, {"switchyard": "MY_API_KEY"})
    settings.save_params("dynamo", {"gpu_memory_utilization": 0.6, "max_num_seqs": 16, "router_mode": "kv",
                                     "enable_prefix_caching": False})
    adapter = FileAdapter(cfg_file)
    cs = ConfigStore(store, {"switchyard": adapter}, cfg.config_cap_bytes, cfg.revisions_keep)  # type: ignore[dict-item]
    sha = __import__("hashlib").sha256(cfg_file.read_bytes()).hexdigest()
    r1 = await cs.apply(adapter, "routes", content="a = 2\n# é\n", base_sha256=sha, restart=True, confirm=True,
                        actor="key:abc")
    await cs.rollback(adapter, "routes", revision_id=r1.revision_id or "", confirm=True, actor="loopback")
    poller = MetricsPoller(StatusService({}, 2.0, 8.0), store, 2.0, 900)
    base = (NOW // 60) * 60 - 600
    for i in range(30):                                   # three 1-minute buckets, two series with labels
        ts = base + i * 6.0
        poller.record("switchyard", ts, [MetricPoint(key=k, value=float(i % 7) + (0.5 if lab else 0.0), labels=lab)
                                         for k, lab in SERIES])
    poller.flush()
    store.close()
    return keys


def checkpoint(db: Path) -> None:
    con = sqlite3.connect(db)
    con.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    con.close()


def diff(a: Any, b: Any, path: str = "") -> list[str]:
    """Strict JSON-value comparison: an int never equals a float, list order matters, dict order does not."""
    if type(a) is not type(b):
        return [f"{path}: {a!r} != {b!r}"]
    if isinstance(a, dict):
        out = []
        for k in sorted(set(a) | set(b)):
            out += diff(a.get(k, "<absent>"), b.get(k, "<absent>"), f"{path}/{k}")
        return out
    if isinstance(a, list):
        if len(a) != len(b):
            return [f"{path}: length {len(a)} != {len(b)}"]
        return [x for i, (u, v) in enumerate(zip(a, b, strict=True)) for x in diff(u, v, f"{path}/{i}")]
    return [] if a == b else [f"{path}: {a!r} != {b!r}"]


def main() -> int:
    mode, target = sys.argv[1], Path(sys.argv[2])
    if mode == "write":
        tmp = Path(tempfile.mkdtemp(prefix="nc-xstate-"))
        state, cfg_file = tmp / "state", tmp / "cfg" / "routes.toml"
        cfg_file.parent.mkdir()
        cfg_file.write_text("a = 1\n")
        keys = asyncio.run(write_state(state, cfg_file))
        checkpoint(state / "console.db")
        expected = reads(state, cfg_file, keys)
        target.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(state / "console.db", target / "console.db")
        (target / "keys.json").write_text(json.dumps(keys, indent=1) + "\n")
        (target / "expected.json").write_text(json.dumps(expected, indent=1) + "\n")
        (target / "routes.toml").write_text(cfg_file.read_text())
        shutil.rmtree(tmp)
        print(f"wrote {target}: {len(expected['audit_all'])} audit rows, {len(expected['history'])} revisions")
        return 0
    if mode == "check":
        tmp = Path(tempfile.mkdtemp(prefix="nc-xstate-check-"))
        state = tmp / "state"
        state.mkdir(mode=0o700)
        shutil.copyfile(target / "console.db", state / "console.db")
        cfg_file = tmp / "routes.toml"
        cfg_file.write_text((target / "routes.toml").read_text())
        keys = json.loads((target / "keys.json").read_text())
        expected = json.loads((target / "expected.json").read_text())
        got = reads(state, cfg_file, keys)
        shutil.rmtree(tmp)
        problems = diff(expected, got)
        for p in problems[:50]:
            print(p)
        print(f"python read of the rust-written state: {'EQUAL' if not problems else f'{len(problems)} DIFFS'}")
        return 0 if not problems else 1
    print("usage: gen_xstate_fixture.py write|check <dir>", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
