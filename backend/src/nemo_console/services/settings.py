"""Runtime settings view/overrides. Only URLs (loopback), the poll interval and credential env-var NAMES are mutable."""
from __future__ import annotations

import json
import os
import re
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

from nemo_console.config import Settings, __version__
from nemo_console.services.errors import ApiError
from nemo_console.services.store import Store

ENV_NAME = re.compile(r"[A-Z_][A-Z0-9_]{0,63}")


def key_env_allowed(name: str) -> bool:
    """Only credential-looking env names may be referenced (else an admin could read the length of, or send as a
    Bearer token, ANY variable such as AWS_SECRET_ACCESS_KEY): LITELLM_*, NEMO_CONSOLE_PLAYGROUND_*, or *_API_KEY."""
    return ENV_NAME.fullmatch(name) is not None and (
        name.startswith(("LITELLM_", "NEMO_CONSOLE_PLAYGROUND_")) or name.endswith("_API_KEY"))

URL_NAME = re.compile(r"[a-z][a-z0-9_]{0,31}")
OVERRIDES_KEY = "overrides"


def validate_url(url: str, allowed_hosts: list[str]) -> str:
    try:
        u = urlsplit(url)
        host = (u.hostname or "").lower()
        port = u.port
    except ValueError:
        raise ApiError(422, "invalid_url", "unparseable url") from None
    if u.scheme != "http" or u.username or u.password or u.query or u.fragment:
        raise ApiError(422, "invalid_url", "urls must be plain http://host:port[/path] with no credentials")
    if host not in {h.lower() for h in allowed_hosts}:
        raise ApiError(422, "invalid_url", f"host must be one of {sorted(allowed_hosts)} (loopback only)")
    if port is not None and not 1 <= port <= 65535:
        raise ApiError(422, "invalid_url", "bad port")
    return url.rstrip("/")


class SettingsService:
    def __init__(self, cfg: Settings, store: Store) -> None:
        self.cfg = cfg
        self.store = store
        self.restart_required = False

    def overrides(self) -> dict[str, Any]:
        rows = self.store.query("SELECT v FROM settings WHERE k=?", (OVERRIDES_KEY,))
        if not rows:
            return {}
        try:
            doc = json.loads(rows[0]["v"])
            return doc if isinstance(doc, dict) else {}
        except ValueError:
            return {}

    def effective_urls(self) -> dict[str, str]:
        urls = dict(self.cfg.urls)
        for k, v in self.overrides().get("urls", {}).items():
            try:
                urls[k] = validate_url(v, self.cfg.allowed_url_hosts)
            except ApiError:
                continue
        return urls

    def effective_key_env(self) -> dict[str, str]:
        env = dict(self.cfg.playground_key_env)
        for k, v in self.overrides().get("playground_key_env", {}).items():
            if isinstance(v, str) and key_env_allowed(v):
                env[k] = v
        return env

    def effective_poll_interval(self) -> float:
        v = self.overrides().get("poll_interval_s")
        return float(v) if isinstance(v, int | float) and 0.5 <= v <= 60 else self.cfg.poll_interval_s

    def token_status(self) -> dict[str, str]:
        out: dict[str, str] = {}
        for logical, name in sorted(self.effective_key_env().items()):
            val = os.environ.get(name)
            out[logical] = f"[set, {len(val)} chars]" if val else "[unset]"
        return out

    def _state_dir_display(self, admin: bool) -> str:
        sd = self.cfg.state_dir
        if admin:
            return str(sd)
        try:
            return "~/" + str(sd.resolve().relative_to(Path.home().resolve()))
        except ValueError:
            return "<state dir>"          # outside $HOME: do not reveal the absolute path to non-admins

    def view(self, admin: bool = True) -> dict[str, Any]:
        return {"version": __version__, "urls": self.effective_urls(), "state_dir": self._state_dir_display(admin),
                "poll_interval_s": self.effective_poll_interval(), "ring_seconds": self.cfg.ring_seconds,
                "playground_key_env": self.effective_key_env(), "tokens": self.token_status(),
                "trust_loopback": self.cfg.effective_loopback_trust()[0], "restart_required": self.restart_required}

    def update(self, urls: dict[str, str] | None, poll_interval_s: float | None,
               key_env: dict[str, str] | None) -> tuple[dict[str, Any], float | None]:
        ov = self.overrides()
        if urls is not None:
            merged = dict(ov.get("urls", {}))
            for k, v in urls.items():
                if k not in self.cfg.urls or not URL_NAME.fullmatch(k):
                    raise ApiError(422, "invalid_setting", f"unknown url name {k[:40]!r}")
                merged[k] = validate_url(v, self.cfg.allowed_url_hosts)
            ov["urls"] = merged
            self.restart_required = True
        if key_env is not None:
            merged_env = dict(ov.get("playground_key_env", {}))
            for k, v in key_env.items():
                if not URL_NAME.fullmatch(k) or not key_env_allowed(v):
                    raise ApiError(422, "invalid_setting", "credential settings take an ENV VAR NAME (e.g. LITELLM_API_KEY), never a value; allowed: LITELLM_*, NEMO_CONSOLE_PLAYGROUND_*, *_API_KEY")
                merged_env[k] = v
            ov["playground_key_env"] = merged_env
        if poll_interval_s is not None:
            if not 0.5 <= poll_interval_s <= 60:
                raise ApiError(422, "invalid_setting", "poll_interval_s must be between 0.5 and 60")
            ov["poll_interval_s"] = poll_interval_s
        self.store.execute("INSERT INTO settings(k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
                           (OVERRIDES_KEY, json.dumps(ov)))
        return self.view(), poll_interval_s

    def saved_params(self, system: str) -> dict[str, Any]:
        rows = self.store.query("SELECT v FROM settings WHERE k=?", (f"params:{system}",))
        try:
            doc = json.loads(rows[0]["v"]) if rows else {}
        except ValueError:
            doc = {}
        return doc if isinstance(doc, dict) else {}

    def save_params(self, system: str, values: dict[str, Any]) -> None:
        self.store.execute("INSERT INTO settings(k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
                           (f"params:{system}", json.dumps(values)))
