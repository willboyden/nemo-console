"""NeMo Relay adapter: persistent gateway (systemd user unit), plugin policy, ATOF/ATIF, routes.toml.

Facts this adapter is built on (source in brackets; see backend/tests/relay/fixtures for captured output):
  * The gateway is `nemo-relay --bind 127.0.0.1:4040 gateway start`, run by the user unit
    `nemo-relay-gateway.service` [systemctl show, live 2026-09-24]. There is NO `gateway status`
    subcommand [`nemo-relay gateway --help`]; liveness is an HTTP probe, unit state is systemd's.
  * `GET /health` is 404 and `GET /v1/models` is 401 without a bearer [live curl]. Any HTTP answer
    below 500 therefore means "the gateway is up"; a 404 or 401 is NOT a failure.
  * Liveness polls `/health`, never `/v1/models` (since 2026-10-04). The gateway answers `/health` itself, while it
    FORWARDS `/v1/models` to LiteLLM, which logged every keyless poll as an ERROR trace in Langfuse:
    ~43,000 a day at the 2 s poll, 98% of all traces [live, 2026-10-04]. "Is the upstream up" is asked
    of LiteLLM's unauthenticated `/health/liveliness` instead, which creates no Langfuse trace [live].
  * `nemo-relay plugins list --json` carries enabled / validation / policy / attestation_mode
    (schema_version 1) and `doctor --offline --json` exists (schema_version 2) [live].
  * `plugins validate` and `doctor` PASS while a gateway start can still fail (unsigned plugin
    under host policy, a routes.toml the plugin rejects) and `doctor` prints "Upstream openai=unset
    anthropic=unset" because it reads its own env, not the unit's wrapper [clients/nemo-relay/README.md,
    plugins.toml.example, routes.toml header]. Those caveats are attached to every validation result.
  * ATOF is JSONL, one event per line: {atof_version, kind: scope|mark, name, scope_category:
    start|end (scope only), category, timestamp (RFC 3339, ns), uuid, parent_uuid, metadata, data}
    [captured from ~/.local/share/nemo-relay/atof]. It rotates to `<file>.N`, older ones gzipped as `<file>.N.gz`
    [live 2026-10-17: relay-events.jsonl, .1, .2.gz]; a .gz is decompressed with caps, never followed. Lines contain
    prompts and completions, so events are built from an explicit allowlist and every string that
    is copied must look like an identifier; free text can never reach an event.

Security: no secret file is ever opened (the gateway .env is not referenced anywhere here). All file
reads are O_NOFOLLOW, regular-file only, size-capped and contained under paths handed in via ctx.paths.
"""
from __future__ import annotations

import asyncio
import contextlib
import errno
import gzip
import hashlib
import ipaddress
import json
import os
import re
import stat
import time
import tomllib
from collections import deque
from collections.abc import AsyncIterator
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any, ClassVar

from nemo_console.adapters.base import AdapterContext, CommandResult, SystemAdapter
from nemo_console.domain.models import (
    ActionResult,
    ActionSpec,
    ApplyResult,
    Component,
    ConfigFile,
    ConfigFileSpec,
    Diagnostic,
    Endpoint,
    MetricPoint,
    MetricsSnapshot,
    PlaygroundSpec,
    Problem,
    SystemEvent,
    SystemStatus,
    ValidationResult,
)

UNIT = "nemo-relay-gateway.service"
DEFAULT_URL = "http://127.0.0.1:4040"
DEFAULT_LITELLM_URL = "http://127.0.0.1:4000"   # the gateway's upstream; ctx.urls["litellm"] overrides it
# Fallback only (the playground reads route ids from clients/nemo-relay/routes.toml when it can); mirrors that file
# (2026-10-17). The default is the route LiteLLM's `switchyard` alias forwards to; auto-local escalates to an engine
# that is no longer deployed.
DEFAULT_ROUTE_IDS = ["super-lightning", "super-glm", "super-vl", "bolt", "direct-lightning", "auto-local", "direct-27b",
                     "direct-flashnext"]
PLAYGROUND_DEFAULT_ROUTE = "super-lightning"

MAX_CONFIG_BYTES = 256 * 1024
TAIL_WINDOW_BYTES = 4 * 1024 * 1024          # max bytes read from the end of an ATOF file per call
FOLLOW_CHUNK_BYTES = 1024 * 1024             # max bytes consumed per follow poll
MAX_LINE_BYTES = 1024 * 1024                 # longer ATOF lines are skipped, never buffered
MAX_SCAN_ENTRIES = 50_000                    # directory listing bound
ATOF_NAME = re.compile(r"^.+\.jsonl(\.\d+)?(\.gz)?$")
ATOF_ROTATION = re.compile(r"\.(\d+)(?:\.gz)?$")
MAX_GZ_FILE_BYTES = 64 * 1024 * 1024         # a larger compressed rotation is skipped, not read
MAX_GZ_BYTES = 64 * 1024 * 1024              # decompressed bytes read from one .gz rotation (bomb guard); over = skipped
IDENT = re.compile(r"^[A-Za-z0-9_.:/@+\-]{1,120}$")   # the only shape of string an event may copy
PLUGIN_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.:/@+\-]{0,119}$")   # never starts with '-'
ENV_NAME = re.compile(r"^[A-Z_][A-Z0-9_]*$")
SEMVER = re.compile(r"\b(\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.]+)?)\b")

SECRET_PATTERNS = [
    re.compile(r"sk-[A-Za-z0-9_\-]{16,}"),
    re.compile(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=\-]{8,}"),
    re.compile(r"""(?im)^(\s*(?:api_key|apikey|secret|password|token|access_key|private_key)\s*=\s*)(["'])(?!\[REDACTED)[^"'\n]+\2"""),
]

VALIDATION_CAVEATS = (
    "`nemo-relay plugins validate` and `doctor` can PASS while the gateway still fails to start "
    "(e.g. an unsigned plugin under host policy, or a routes.toml the plugin rejects). "
    "The authoritative check is a gateway restart."
)
DOCTOR_CAVEATS = [
    (
        "`doctor` line `Upstream openai=unset anthropic=unset` reflects the CLI's own environment, not the "
        "gateway unit: ops/nemo-relay-gateway.sh exports NEMO_RELAY_OPENAI_BASE_URL / _ANTHROPIC_BASE_URL."
    ),
    "`plugins list` runtime_state `stopped` is per CLI process; it does not describe the running gateway.",
    "'not enabled by config alone' in doctor refers to desired-state; see `plugins list` for enabled.",
    VALIDATION_CAVEATS,
]

ROUTES_NOTE = (
    "Validated by a TOML parse plus a semantic pass that MIRRORS the Switchyard schema_version-1 language. "
    "`switchyard-server --dry-run` is NOT a valid proxy for the Relay plugin schema (different builds; the "
    "server accepts fields such as capable_system_prompt that the plugin rejects), so it is not used. "
    "`nemo-relay plugins validate` checks the plugin manifest/policy, not this file. The authoritative check is "
    "a gateway restart: applying restarts nemo-relay-gateway.service and the console rolls back automatically "
    "if it does not come back healthy."
)


# ── small pure helpers ─────────────────────────────────────────────────────────────────────────
def scrub(text: str) -> str:
    """Redact secret-shaped substrings (defence in depth; the runner also redacts)."""
    out = text
    out = SECRET_PATTERNS[0].sub("[REDACTED]", out)
    out = SECRET_PATTERNS[1].sub("Bearer [REDACTED]", out)
    out = SECRET_PATTERNS[2].sub(lambda m: f'{m.group(1)}"[REDACTED]"', out)
    return out


def _ident(v: object) -> str | None:
    return v if isinstance(v, str) and IDENT.fullmatch(v) else None


def _d(v: object) -> dict[str, Any]:
    return v if isinstance(v, dict) else {}


def _num(v: object) -> float | int | None:
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        return None
    return v


def _parse_ts(v: object) -> float | None:
    if not isinstance(v, str):
        return None
    m = re.fullmatch(r"(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.(\d+))?(Z|[+-]\d\d:\d\d)", v)
    if not m:
        return None
    frac = (m.group(2) or "0")[:6].ljust(6, "0")
    tz = "+00:00" if m.group(3) == "Z" else m.group(3)
    try:
        return datetime.fromisoformat(f"{m.group(1)}.{frac}{tz}").timestamp()
    except ValueError:
        return None


def _open_regular(path: Path, cap: int | None = None) -> tuple[int, os.stat_result]:
    """Open a regular, non-symlink file read-only (O_NOFOLLOW). Caller closes the fd."""
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
    except OSError as e:
        if e.errno == errno.ELOOP:
            raise PermissionError(f"{path.name} is a symlink") from e
        raise
    st = os.fstat(fd)
    if not stat.S_ISREG(st.st_mode):
        os.close(fd)
        raise PermissionError(f"{path.name} is not a regular file")
    if cap is not None and st.st_size > cap:
        os.close(fd)
        raise PermissionError(f"{path.name} exceeds the {cap} byte read cap")
    return fd, st


def _contained(path: Path, root: Path) -> bool:
    try:
        path.resolve(strict=False).relative_to(root.resolve(strict=False))
    except ValueError:
        return False
    return True


def read_capped(path: Path, root: Path, cap: int = MAX_CONFIG_BYTES) -> tuple[bytes, os.stat_result]:
    if not _contained(path, root):
        raise PermissionError("path escapes its allowed root")
    fd, st = _open_regular(path, cap)
    try:
        with os.fdopen(fd, "rb", closefd=False) as fh:
            return fh.read(cap + 1)[:cap], st
    finally:
        os.close(fd)


def tail_bytes(path: Path, root: Path, window: int = TAIL_WINDOW_BYTES) -> tuple[bytes, bool]:
    """Last <= window bytes of a file; second value True when the window starts mid-file."""
    if not _contained(path, root):
        raise PermissionError("path escapes its allowed root")
    fd, st = _open_regular(path)
    try:
        start = max(0, st.st_size - window)
        os.lseek(fd, start, os.SEEK_SET)
        data = b""
        while len(data) < window:
            chunk = os.read(fd, min(1 << 20, window - len(data)))
            if not chunk:
                break
            data += chunk
        return data, start > 0
    finally:
        os.close(fd)


def gz_tail_bytes(path: Path, root: Path, window: int = TAIL_WINDOW_BYTES) -> tuple[bytes, bool]:
    """Last <= window DECOMPRESSED bytes of a gzipped rotation (a gzip stream cannot be read from the end, so it is
    decompressed in chunks, keeping only the window). Same no-follow / regular-file / containment rules as tail_bytes;
    a file over MAX_GZ_FILE_BYTES, or one that decompresses past MAX_GZ_BYTES, raises instead of being read."""
    if not _contained(path, root):
        raise PermissionError("path escapes its allowed root")
    fd, _st = _open_regular(path, MAX_GZ_FILE_BYTES)
    buf = bytearray()
    total = 0
    try:
        with os.fdopen(fd, "rb", closefd=False) as raw, gzip.GzipFile(fileobj=raw) as gz:
            while chunk := gz.read(1 << 20):
                total += len(chunk)
                if total > MAX_GZ_BYTES:
                    raise PermissionError(f"{path.name} decompresses past the {MAX_GZ_BYTES} byte cap")
                buf += chunk
                if len(buf) > window:
                    del buf[: len(buf) - window]
    except (EOFError, gzip.BadGzipFile) as e:
        raise OSError(f"{path.name}: not a readable gzip file") from e
    finally:
        os.close(fd)
    return bytes(buf), total > window


def _human_bytes(n: int) -> str:
    v = float(n)
    for unit in ("B", "KiB", "MiB", "GiB"):
        if v < 1024 or unit == "GiB":
            return f"{v:.0f} {unit}" if unit == "B" else f"{v:.1f} {unit}"
        v /= 1024
    return f"{n} B"


# ── ATOF line -> metadata-only event ───────────────────────────────────────────────────────────
def _usage_fields(usage: object, out: dict[str, Any]) -> None:
    if not isinstance(usage, dict):
        return
    for src, dst in (("input_tokens", "input_tokens"), ("prompt_tokens", "input_tokens"),
                     ("output_tokens", "output_tokens"), ("completion_tokens", "output_tokens"),
                     ("total_tokens", "total_tokens"), ("cache_read_tokens", "cache_read_tokens"),
                     ("cache_write_tokens", "cache_write_tokens")):
        n = _num(usage.get(src))
        if n is not None and dst not in out:
            out[dst] = n


def parse_atof_line(raw: bytes) -> SystemEvent | None:
    """One ATOF JSONL line -> SystemEvent carrying allowlisted METADATA only, or None if unusable.

    Never copies `data.prompt`, `data.content`, `messages`, headers (except the session id) or any string that
    is not identifier-shaped. `size_bytes` is the size of the original line.
    """
    if not raw.strip() or len(raw) > MAX_LINE_BYTES:
        return None
    try:
        o = json.loads(raw)
    except ValueError:
        return None
    if not isinstance(o, dict):
        return None
    ts = _parse_ts(o.get("timestamp"))
    name = _ident(o.get("name"))
    if ts is None or name is None:
        return None
    md_raw = o.get("metadata")
    md: dict[str, Any] = md_raw if isinstance(md_raw, dict) else {}
    d_raw = o.get("data")
    d: dict[str, Any] = d_raw if isinstance(d_raw, dict) else {}
    data: dict[str, Any] = {"atof_kind": _ident(o.get("kind")), "name": name, "size_bytes": len(raw)}

    for key, val in (("phase", o.get("scope_category")), ("category", o.get("category")),
                     ("uuid", o.get("uuid")), ("gateway_path", md.get("gateway_path")),
                     ("status_code", md.get("otel.status_code")), ("severity", md.get("nemo_relay.log.severity")),
                     ("session_id", md.get("session_id")), ("turn_index", md.get("turn_index"))):
        v = _ident(val)
        if v is not None:
            data[key] = v
    headers = d.get("headers")
    if "session_id" not in data and isinstance(headers, dict):
        v = _ident(headers.get("x-nemo-relay-session-id"))
        if v is not None:
            data["session_id"] = v

    content = d.get("content")
    profile = o.get("category_profile")
    model = (_ident(d.get("model")) or _ident(d.get("selected_model"))
             or (_ident(content.get("model")) if isinstance(content, dict) else None)
             or (_ident(profile.get("model_name")) if isinstance(profile, dict) else None))
    if model:
        data["model"] = model
    is_err = name == "switchyard.routing.error"
    for key in ("served_model", "target", "target_model", "algorithm", "outcome", "call_role", "category",
                "failure_kind", "phase", "stop_reason", "event_type", "provider"):
        v = _ident(d.get(key))
        if v is not None:
            data[f"error_{key}" if (is_err and key in ("category", "phase")) else key] = v
    for key in ("latency_ms", "call_index", "upstream_status", "chunk_index"):
        n = _num(d.get(key))
        if n is not None:
            data[key] = n
    fb = d.get("fallback_used")
    if isinstance(fb, bool):
        data["fallback_used"] = fb
    choices = d.get("choices")
    if isinstance(choices, list) and choices and isinstance(choices[0], dict):
        v = _ident(choices[0].get("finish_reason"))
        if v is not None:
            data["finish_reason"] = v
    _usage_fields(d.get("usage"), data)
    meas = d.get("measurements")
    if isinstance(meas, list):
        for m in meas[:8]:
            if not isinstance(m, dict):
                continue
            attrs = m.get("attributes")
            val = _num(m.get("value"))
            if val is None or not isinstance(attrs, dict):
                continue
            tt = _ident(attrs.get("token_type"))
            if tt in ("input", "output", "reasoning", "cached"):
                data[f"{tt}_tokens"] = val
            tm = _ident(attrs.get("target_model"))
            if tm and "model" not in data:
                data["model"] = tm

    bits = [name]
    if data.get("phase"):
        bits.append(str(data["phase"]))
    if data.get("model"):
        bits.append(f"model={data['model']}")
    if "input_tokens" in data or "output_tokens" in data:
        bits.append(f"tok={data.get('input_tokens', '?')}/{data.get('output_tokens', '?')}")
    if "latency_ms" in data:
        bits.append(f"{float(data['latency_ms']):.0f}ms")
    if "upstream_status" in data:
        bits.append(f"upstream={data['upstream_status']}")
    return SystemEvent(ts=ts, system="relay", kind="relay.atof", summary=" ".join(bits)[:200], data=data)


# ── routes.toml semantic validation (mirrors the Switchyard schema_version-1 language) ─────────
_CLIENT_FIELDS = {"format", "base_url", "api_key_env", "forward_auth", "max_retries"}
_TARGET_FIELDS = {"id", "llm_client", "system_prompt"}
_ROUTE_FIELDS = {"id", "type", "capable_target", "efficient_target", "picker", "confidence_threshold",
                 "recent_turn_window", "context_window", "tool_calling", "reasoning", "handoff_notes",
                 "classifier", "target"}
_CLASSIFIER_FIELDS = {"target", "base_threshold", "max_output_tokens"}
_PLUGIN_REJECTED = {"capable_system_prompt", "efficient_system_prompt"}
_FORMATS = {"openai_chat", "anthropic_messages"}
_ROUTE_TYPES = {"stage_router", "passthrough"}


_URL_RE = re.compile(r"^https?://([^/?#]*)(?:[/?#][^\s\\]*)?$")
_HOST_PORT_RE = re.compile(r"^([^:@\[\]]+)(?::(\d{1,5}))?$")
_V6_RE = re.compile(r"^\[([0-9A-Fa-f:.]+)\](?::(\d{1,5}))?$")
_SERVICE_LABEL = re.compile(r"^[a-z][a-z0-9_-]*$")


def base_url_problem(url: str) -> str | None:
    """None when the URL may only reach this box (loopback or a bare single-label docker service name).

    Rejects userinfo entirely (`http://127.0.0.1@evil`), whitespace/backslashes, non-loopback IPs and any dotted
    hostname, so a routes.toml cannot send prompts or the LiteLLM credential off-box. One trailing dot is tolerated.
    """
    if any(ord(ch) < 33 or ord(ch) > 126 for ch in url) or "\\" in url:
        return "contains whitespace, control or non-ASCII characters"
    m = _URL_RE.match(url)
    if not m:
        return "must be an http(s) URL"
    auth = m.group(1)
    if "@" in auth:
        return "must not contain userinfo (user@host)"
    v6 = _V6_RE.match(auth)
    if v6:
        try:
            ok = ipaddress.IPv6Address(v6.group(1)).is_loopback
        except ValueError:
            return "has an invalid IPv6 address"
        port = v6.group(2)
        host_ok = ok
    else:
        hp = _HOST_PORT_RE.match(auth)
        if not hp:
            return "has an unparseable host"
        host = hp.group(1).lower()
        port = hp.group(2)
        host = host.removesuffix(".")
        try:
            host_ok = ipaddress.IPv4Address(host).is_loopback
        except ValueError:
            host_ok = host == "localhost" or bool(_SERVICE_LABEL.fullmatch(host))
    if port is not None and int(port) > 65535:
        return "has an out-of-range port"
    if not host_ok:
        return ("points off-box: only loopback (127.0.0.0/8, ::1, localhost) or a bare single-label service name "
                "(e.g. litellm) is allowed, otherwise prompts and the LiteLLM credential would leave this machine")
    return None


def _line_of(lines: list[str], header: str) -> int | None:
    pat = re.compile(rf"^\s*\[{re.escape(header)}\]\s*(#.*)?$")
    for i, ln in enumerate(lines, 1):
        if pat.match(ln):
            return i
    return None


def _table(d: dict[str, Any], key: str, diags: list[Diagnostic]) -> dict[str, dict[str, Any]]:
    v = d.get(key)
    if v is None:
        return {}
    if not isinstance(v, dict) or not all(isinstance(x, dict) for x in v.values()):
        diags.append(Diagnostic(severity="error", message=f"[{key}] must be a table of tables"))
        return {}
    return v


def validate_routes_semantics(text: str) -> list[Diagnostic]:
    diags: list[Diagnostic] = []
    lines = text.splitlines()
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError as e:
        m = re.search(r"line (\d+)", str(e))
        return [Diagnostic(severity="error", message=f"TOML syntax error: {e}", line=int(m.group(1)) if m else None)]

    def err(msg: str, header: str | None = None) -> None:
        diags.append(Diagnostic(severity="error", message=msg, line=_line_of(lines, header) if header else None))

    def warn(msg: str, header: str | None = None) -> None:
        diags.append(Diagnostic(severity="warn", message=msg, line=_line_of(lines, header) if header else None))

    if data.get("schema_version") != 1:
        err(f"schema_version must be 1 (got {data.get('schema_version')!r})")
    for k in data:
        if k not in {"schema_version", "llm_clients", "targets", "routes"}:
            warn(f"unknown top-level key {k!r}; the plugin rejects unknown fields")

    clients = _table(data, "llm_clients", diags)
    targets = _table(data, "targets", diags)
    routes = _table(data, "routes", diags)
    if not routes:
        err("no [routes.*] defined; the gateway would have nothing to route")

    for cname, c in clients.items():
        h = f"llm_clients.{cname}"
        if c.get("format") not in _FORMATS:
            err(f"{h}: format must be one of {sorted(_FORMATS)}", h)
        bu = c.get("base_url")
        if not isinstance(bu, str):
            err(f"{h}: base_url must be an http(s) URL", h)
        else:
            bad = base_url_problem(bu)
            if bad:
                err(f"{h}: base_url {bad}", h)
        if "api_key" in c or "apikey" in c:
            err(f"{h}: literal API keys are not allowed; use api_key_env = \"ENV_NAME\"", h)
        ake = c.get("api_key_env")
        if ake is not None and not (isinstance(ake, str) and ENV_NAME.fullmatch(ake)):
            err(f"{h}: api_key_env must be an environment variable NAME (e.g. LITELLM_MASTER_KEY), never a value", h)
        mr = c.get("max_retries")
        if mr is not None and (isinstance(mr, bool) or not isinstance(mr, int) or mr < 0):
            err(f"{h}: max_retries must be a non-negative integer", h)
        for k in c:
            if k not in _CLIENT_FIELDS and k not in {"api_key", "apikey"}:
                warn(f"{h}: unknown field {k!r}; the plugin rejects unknown fields", h)

    target_identity: dict[tuple[str, str], str] = {}
    for tname, t in targets.items():
        h = f"targets.{tname}"
        tid, cl = t.get("id"), t.get("llm_client")
        if not isinstance(tid, str) or not tid:
            err(f"{h}: id must be a non-empty string", h)
        if not isinstance(cl, str) or cl not in clients:
            err(f"{h}: llm_client {cl!r} is not defined under [llm_clients]", h)
        if isinstance(tid, str) and isinstance(cl, str):
            prev = target_identity.get((tid, cl))
            if prev is not None:
                warn(f"{h}: same (id, llm_client) as targets.{prev}; the plugin silently DROPS one with only a WARN", h)
            target_identity[(tid, cl)] = tname
        for k in t:
            if k not in _TARGET_FIELDS:
                warn(f"{h}: unknown field {k!r}; the plugin rejects unknown fields", h)

    def fmt_of(tname: str) -> str | None:
        t = targets.get(tname)
        c = clients.get(t.get("llm_client", "")) if t else None
        f = c.get("format") if c else None
        return f if isinstance(f, str) else None

    route_ids: dict[str, str] = {}
    for rname, r in routes.items():
        h = f"routes.{rname}"
        rid, rtype = r.get("id"), r.get("type")
        if not isinstance(rid, str) or not rid:
            err(f"{h}: id must be a non-empty string", h)
        elif rid in route_ids:
            err(f"{h}: route id {rid!r} duplicates routes.{route_ids[rid]}", h)
        else:
            route_ids[rid] = rname
        if rtype not in _ROUTE_TYPES:
            err(f"{h}: type must be one of {sorted(_ROUTE_TYPES)}", h)
        for bad in _PLUGIN_REJECTED & set(r):
            err(f"{h}: {bad} is rejected by the Relay plugin ('unknown field') although switchyard-server 0.2.0 accepts it", h)
        for k in r:
            if k not in _ROUTE_FIELDS and k not in _PLUGIN_REJECTED:
                warn(f"{h}: unknown field {k!r}; the plugin rejects unknown fields", h)

        def need(field: str, h: str = h, r: dict[str, Any] = r) -> str | None:
            v = r.get(field)
            if not isinstance(v, str) or v not in targets:
                err(f"{h}: {field} {v!r} is not defined under [targets]", h)
                return None
            return v

        used: list[str] = []
        if rtype == "passthrough":
            tv = need("target")
            if tv:
                used.append(tv)
        elif rtype == "stage_router":
            for f in ("capable_target", "efficient_target"):
                tv = need(f)
                if tv:
                    used.append(tv)
            thr = r.get("confidence_threshold")
            if thr is not None and (isinstance(thr, bool) or not isinstance(thr, (int, float)) or not 0 <= thr <= 1):
                err(f"{h}: confidence_threshold must be a number between 0 and 1", h)
            for f in ("recent_turn_window", "context_window"):
                v = r.get(f)
                if v is not None and (isinstance(v, bool) or not isinstance(v, int) or v <= 0):
                    err(f"{h}: {f} must be a positive integer", h)
            for f in ("tool_calling", "reasoning"):
                if f in r and not isinstance(r[f], bool):
                    err(f"{h}: {f} must be a boolean", h)
        cl = r.get("classifier")
        if cl is not None:
            ch = f"routes.{rname}.classifier"
            if not isinstance(cl, dict):
                err(f"{ch} must be a table", ch)
            else:
                tv = cl.get("target")
                if not isinstance(tv, str) or tv not in targets:
                    err(f"{ch}: target {tv!r} is not defined under [targets]", ch)
                else:
                    used.append(tv)
                for k in cl:
                    if k not in _CLASSIFIER_FIELDS:
                        warn(f"{ch}: unknown field {k!r}; the plugin rejects unknown fields", ch)
        # Switchyard 0.3.0 plugin refuses a route that holds one model id on two wire formats.
        by_model: dict[str, set[str]] = {}
        for tn in used:
            fm = fmt_of(tn)
            tid = targets[tn].get("id")
            if fm is not None and isinstance(tid, str):
                by_model.setdefault(tid, set()).add(fm)
        for tid, fs in by_model.items():
            if len(fs) > 1:
                err(f"{h}: model id {tid!r} is used on two wire formats ({', '.join(sorted(fs))}); the 0.3.0 plugin "
                    "refuses this. Give the second use its own alias (see the nemotron-lightning-judge alias)", h)
    return diags


# ── plugins.toml summary (no raw content; public keys are only counted) ────────────────────────
@dataclass(frozen=True)
class PluginsSummary:
    parsed: bool
    dynamic_manifests: int = 0
    attestation: str | None = None
    trusted_keys: int = 0
    atof: bool = False
    atif: bool = False
    otel: bool = False
    pii_redaction: bool = False
    observability_component: bool = False
    atof_dir: str | None = None
    error: str | None = None

    @property
    def exporters_enabled(self) -> int:
        return int(self.atof) + int(self.atif) + int(self.otel)


def summarize_plugins_toml(text: str) -> PluginsSummary:
    try:
        d = tomllib.loads(text)
    except tomllib.TOMLDecodeError as e:
        return PluginsSummary(parsed=False, error=f"plugins.toml does not parse: {str(e)[:160]}")
    plugins = _d(d.get("plugins"))
    dyn = plugins.get("dynamic")
    policy = _d(plugins.get("policy"))
    defaults = _d(policy.get("defaults"))
    att = defaults.get("attestation")
    keys = defaults.get("trusted_public_keys")
    atof = atif = otel = pii = obs = False
    atof_dir: str | None = None
    comps = d.get("components")
    for c in comps if isinstance(comps, list) else []:
        if not isinstance(c, dict) or c.get("enabled") is False:
            continue
        cfg = _d(c.get("config"))
        if c.get("kind") == "pii_redaction":
            pii = True
        elif c.get("kind") == "observability":
            obs = True

            def on(key: str, cfg: dict[str, Any] = cfg) -> bool:
                sub = cfg.get(key)
                return isinstance(sub, dict) and sub.get("enabled") is True

            atof, atif, otel = on("atof"), on("atif"), on("opentelemetry")
            sinks = _d(cfg.get("atof")).get("sinks")
            if isinstance(sinks, list) and sinks and isinstance(sinks[0], dict):
                od = sinks[0].get("output_directory")
                atof_dir = od if isinstance(od, str) else None
    return PluginsSummary(
        parsed=True, dynamic_manifests=len(dyn) if isinstance(dyn, list) else 0,
        attestation=att if isinstance(att, str) else None,
        trusted_keys=len(keys) if isinstance(keys, list) else 0,
        atof=atof, atif=atif, otel=otel, pii_redaction=pii, observability_component=obs, atof_dir=atof_dir)


@dataclass(frozen=True)
class Probe:
    reachable: bool
    status: int | None = None
    detail: str = ""


@dataclass(frozen=True)
class UnitState:
    known: bool
    load: str = ""
    active: str = ""
    sub: str = ""
    pid: int = 0
    restarts: int = 0
    result: str = ""
    enabled: str = ""
    active_since: float | None = None


def parse_systemctl_show(text: str) -> UnitState:
    kv: dict[str, str] = {}
    for ln in text.splitlines():
        if "=" in ln:
            k, _, v = ln.partition("=")
            kv[k.strip()] = v.strip()
    if "ActiveState" not in kv:
        return UnitState(known=False)
    since = None
    m = re.fullmatch(r"@(\d+)", kv.get("ActiveEnterTimestamp", ""))
    if m:
        since = float(m.group(1))

    def i(k: str) -> int:
        return int(kv[k]) if kv.get(k, "").isdigit() else 0

    return UnitState(known=True, load=kv.get("LoadState", ""), active=kv["ActiveState"], sub=kv.get("SubState", ""),
                     pid=i("MainPID"), restarts=i("NRestarts"), result=kv.get("Result", ""),
                     enabled=kv.get("UnitFileState", ""), active_since=since)


SHOW_PROPS = "ActiveEnterTimestamp,ActiveState,SubState,MainPID,NRestarts,Result,LoadState,UnitFileState"
SHOW_ARGV = ["systemctl", "--user", "--timestamp=unix", "show", UNIT, "-p", SHOW_PROPS]   # with the default unit


class RelayAdapter(SystemAdapter):
    id = "relay"
    label = "NeMo Relay"

    # ctx.paths keys this adapter reads (all absolute, fixed by the core at startup):
    #   relay_routes  clients/nemo-relay/routes.toml     relay_plugins ~/.config/nemo-relay/plugins.toml
    #   relay_config  ~/.config/nemo-relay/config.toml   relay_atof_dir ~/.local/share/nemo-relay/atof
    #   relay_atif_dir ~/.local/share/nemo-relay/atif
    # ctx.urls: "relay" (default http://127.0.0.1:4040), "litellm" (the gateway's upstream, default :4000).
    unit = UNIT                         # systemd --user unit (addendum 9.11: the container sets `relay_unit`)

    def __init__(self, ctx: AdapterContext) -> None:
        super().__init__(ctx)
        self.poll_interval = 1.0        # health poll cadence after a restart (tests set 0)
        self.settle_checks = 3          # consecutive good polls required (config errors surface AFTER the banner)
        self.restart_timeout = 30.0
        self.follow_interval = 1.0
        self._cache: dict[str, tuple[float, Any]] = {}

    # ── paths / urls ────────────────────────────────────────────────────────────────────────
    @property
    def _base(self) -> str:
        return self.ctx.urls.get("relay", DEFAULT_URL).rstrip("/")

    @property
    def _litellm(self) -> str:
        return self.ctx.urls.get("litellm", DEFAULT_LITELLM_URL).rstrip("/")

    def _path(self, key: str) -> Path:
        p = self.ctx.paths.get(key)
        if p is None:
            raise KeyError(f"ctx.paths is missing {key!r}")
        return p

    _FILES: ClassVar[dict[str, str]] = {"routes": "relay_routes", "plugins": "relay_plugins", "config": "relay_config"}

    def config_path(self, file_id: str) -> Path:
        return self._path(self._FILES[file_id])       # KeyError for an unknown id

    def _read_file(self, file_id: str) -> tuple[bytes, os.stat_result]:
        p = self.config_path(file_id)
        return read_capped(p, p.parent)

    # ── runner helper (failure is data) ─────────────────────────────────────────────────────
    async def _run(self, argv: list[str], timeout: float = 15.0, max_bytes: int = 256_000) -> CommandResult:
        try:
            return await self.ctx.runner.run(argv, timeout=timeout, max_bytes=max_bytes)
        except Exception as e:  # noqa: BLE001 - failure is data
            return CommandResult(returncode=127, stdout="", stderr=f"{type(e).__name__}: {str(e)[:200]}")

    async def _cached(self, key: str, ttl: float, fn: Any) -> Any:
        now = time.monotonic()
        hit = self._cache.get(key)
        if hit and now - hit[0] < ttl:
            return hit[1]
        val = await fn()
        self._cache[key] = (now, val)
        return val

    async def _unit(self) -> UnitState:
        r = await self._run(["systemctl", "--user", "--timestamp=unix", "show", self.unit, "-p", SHOW_PROPS],
                                timeout=8.0)
        return parse_systemctl_show(r.stdout) if r.returncode == 0 else UnitState(known=False)

    async def _get_probe(self, url: str) -> Probe:
        try:
            resp = await self.ctx.http.get(url, timeout=3.0, follow_redirects=False)
        except Exception as e:  # noqa: BLE001
            return Probe(False, None, f"{type(e).__name__}")
        return Probe(True, resp.status_code, f"HTTP {resp.status_code}")

    async def _probe(self) -> Probe:
        # The gateway answers /health itself (404): any answer below 500 means it is up. Never /v1/models,
        # which the gateway forwards to LiteLLM (see the module docstring).
        return await self._get_probe(f"{self._base}/health")

    async def _upstream(self) -> Probe:
        # LiteLLM's unauthenticated liveness route: not a model route, so it leaves no Langfuse trace.
        return await self._get_probe(f"{self._litellm}/health/liveliness")

    async def _version(self) -> str | None:
        async def go() -> str | None:
            r = await self._run(["nemo-relay", "--version"], timeout=8.0, max_bytes=4096)
            m = SEMVER.search(r.stdout) if r.returncode == 0 else None
            return m.group(1) if m else None
        res: str | None = await self._cached("version", 300.0, go)
        return res

    async def _plugins_list(self) -> list[dict[str, Any]] | None:
        async def go() -> list[dict[str, Any]] | None:
            r = await self._run(["nemo-relay", "plugins", "list", "--json"], timeout=15.0)
            if r.returncode != 0:
                return None
            try:
                d = json.loads(r.stdout)
            except ValueError:
                return None
            rows = d.get("data") if isinstance(d, dict) else None
            return [x for x in rows if isinstance(x, dict)] if isinstance(rows, list) else None
        rows: list[dict[str, Any]] | None = await self._cached("plugins_list", 20.0, go)
        return rows

    def _plugins_summary(self) -> tuple[PluginsSummary, float | None]:
        try:
            raw, st = self._read_file("plugins")
        except (OSError, KeyError) as e:
            return PluginsSummary(parsed=False, error=f"cannot read plugins.toml: {type(e).__name__}"), None
        return summarize_plugins_toml(raw.decode("utf-8", "replace")), st.st_mtime

    def _scan_dir(self, key: str, pattern: re.Pattern[str] | None) -> tuple[int, int] | None:
        """(count, bytes) of regular non-symlink files in a fixed dir, or None when unreadable."""
        try:
            root = self._path(key)
            n = size = 0
            with os.scandir(root) as it:
                for i, e in enumerate(it):
                    if i >= MAX_SCAN_ENTRIES:
                        break
                    if pattern is not None and not pattern.match(e.name):
                        continue
                    if e.is_symlink() or not e.is_file(follow_symlinks=False):
                        continue
                    n += 1
                    size += e.stat(follow_symlinks=False).st_size
            return n, size
        except (OSError, KeyError):
            return None

    # ── observe ─────────────────────────────────────────────────────────────────────────────
    async def status(self) -> SystemStatus:
        now = time.time()
        probe, upstream, unit, version, plist = await asyncio.gather(
            self._probe(), self._upstream(), self._unit(), self._version(), self._plugins_list())
        summary, plugins_mtime = await asyncio.to_thread(self._plugins_summary)
        atof_scan = await asyncio.to_thread(self._scan_dir, "relay_atof_dir", None)
        problems: list[Problem] = []
        restart_fix = f"systemctl --user restart {self.unit}"

        unit_running = unit.known and unit.active == "active"
        if not unit.known:
            ustate = "unknown"
        elif unit.load == "not-found":
            ustate = "absent"
        elif unit.active == "failed":
            ustate = "failed"
        elif unit_running:
            ustate = "running"
        else:
            ustate = "stopped"
        components = [
            Component(kind="systemd", name=self.unit, state=ustate,
                      detail=(f"{unit.active}/{unit.sub} pid={unit.pid} restarts={unit.restarts} enabled={unit.enabled}"
                              if unit.known else "systemctl --user show failed")),
            Component(kind="http", name="gateway " + self._base, state="running" if probe.reachable else "stopped",
                      detail=(f"{probe.detail} on GET /health (the gateway answers it itself; 404 is expected and means UP)"
                              if probe.reachable else f"unreachable ({probe.detail})")),
            Component(kind="http", name="upstream " + self._litellm,
                      state="running" if upstream.reachable and (upstream.status or 0) < 500 else "stopped",
                      detail=f"{upstream.detail} on GET /health/liveliness" if upstream.reachable
                      else f"unreachable ({upstream.detail})"),
        ]

        health: str
        if probe.reachable:
            health = "up"
            if (probe.status or 0) >= 500:
                health = "degraded"
                problems.append(Problem(severity="warn", code="relay.gateway.error",
                                        message=f"Gateway answered /health with HTTP {probe.status}.",
                                        fix=restart_fix))
            if not upstream.reachable or (upstream.status or 0) >= 500:
                health = "degraded"
                problems.append(Problem(severity="warn", code="relay.gateway.upstream_error",
                                        message=(f"Gateway is up but its upstream LiteLLM ({self._litellm}) is not answering "
                                                 f"/health/liveliness ({upstream.detail}); requests through the gateway will fail."),
                                        fix=f"curl -s -o /dev/null -w '%{{http_code}}\\n' {self._litellm}/health/liveliness"))
            if unit.known and not unit_running:
                problems.append(Problem(severity="info", code="relay.gateway.not_under_systemd",
                                        message=f"The gateway answers but {self.unit} is {unit.active}; it is being run by hand.",
                                        fix=None))
        else:
            health = "degraded" if unit_running else "down"
            problems.append(Problem(
                severity="error", code="relay.gateway.down",
                message=(f"{self.unit} is active but {self._base} does not answer." if unit_running
                         else f"NeMo Relay gateway is not reachable at {self._base} ({self.unit} is {unit.active or 'unknown'})."),
                fix=restart_fix))

        if not summary.parsed:
            problems.append(Problem(severity="warn", code="relay.plugins_toml.unreadable",
                                    message=summary.error or "plugins.toml could not be read.",
                                    fix="nemo-relay doctor --offline"))
        else:
            if summary.attestation != "signature_required":
                problems.append(Problem(
                    severity="error" if summary.dynamic_manifests else "warn", code="relay.plugin.attestation_weak",
                    message=f"Plugin attestation is {summary.attestation or 'unset'!r}, not 'signature_required': anyone who can write the "
                            "plugin directory can swap the .so and its digest together.",
                    fix='Local security policy: signature enforcement settings are a human decision. In ~/.config/nemo-relay/plugins.toml set [plugins.policy.defaults] attestation = "signature_required" '
                        "with your trusted_public_keys; the console shows a diff only."))
            if summary.dynamic_manifests == 0:
                problems.append(Problem(severity="warn", code="relay.plugin.none_configured",
                                        message="No [[plugins.dynamic]] entry: Switchyard routing is not configured in Relay.",
                                        fix="nemo-relay plugins add --user <path-to>/relay-plugin.toml"))
            if summary.exporters_enabled == 0:
                problems.append(Problem(severity="warn", code="relay.exporters.off",
                                        message="No exporter is enabled (ATOF, ATIF and OpenTelemetry are all off): Relay records nothing.",
                                        fix="Set enabled = true under [components.config.atof] / .atif / .opentelemetry in ~/.config/nemo-relay/plugins.toml (human decision)."))
            elif not summary.pii_redaction and (summary.atof or summary.atif):
                problems.append(Problem(severity="warn", code="relay.pii_redaction.off",
                                        message="ATOF/ATIF are on but no enabled pii_redaction component precedes them; prompts land on disk unredacted.",
                                        fix="Add the pii_redaction component from clients/nemo-relay/plugins.toml.example to ~/.config/nemo-relay/plugins.toml (human decision)."))

        plugin_row = self._plugin_row(plist)
        plugin_state = "not configured"
        if plist is None:
            problems.append(Problem(severity="info", code="relay.plugins.list_unavailable",
                                    message="`nemo-relay plugins list --json` failed; plugin enablement is unknown.",
                                    fix="nemo-relay plugins list"))
        elif plugin_row is not None:
            plugin_state = (f"{plugin_row.get('id')} enabled={plugin_row.get('enabled')} validation={plugin_row.get('validation_state')} "
                            f"policy={plugin_row.get('policy_state')} attestation={plugin_row.get('attestation_mode')}")
            if plugin_row.get("enabled") is not True:
                health = "degraded" if health == "up" else health
                problems.append(Problem(severity="warn", code="relay.plugin.disabled", message=f"Plugin {plugin_row.get('id')} is registered but not enabled.",
                                        fix=f"nemo-relay plugins enable {plugin_row.get('id')}"))
            elif plugin_row.get("validation_state") != "valid" or plugin_row.get("policy_state") != "valid":
                health = "degraded" if health == "up" else health
                problems.append(Problem(severity="error", code="relay.plugin.invalid",
                                        message=f"Plugin {plugin_row.get('id')} validation={plugin_row.get('validation_state')} policy={plugin_row.get('policy_state')}.",
                                        fix=f"nemo-relay plugins validate {plugin_row.get('id')}"))
            if plugin_row.get("last_error"):
                problems.append(Problem(severity="warn", code="relay.plugin.last_error",
                                        message=scrub(str(plugin_row["last_error"]))[:300], fix=None))

        if unit.active_since and plugins_mtime and plugins_mtime > unit.active_since + 1 and unit_running:
            problems.append(Problem(severity="warn", code="relay.config.newer_than_gateway",
                                    message="plugins.toml changed after the gateway started; the running gateway has not loaded it.",
                                    fix=restart_fix))
        if atof_scan and atof_scan[1] > 1024 ** 3:
            problems.append(Problem(severity="warn", code="relay.atof.large",
                                    message=f"ATOF directory holds {_human_bytes(atof_scan[1])} in {atof_scan[0]} files; Relay has no retention policy.",
                                    fix="Prune old rotated files under ~/.local/share/nemo-relay/atof/ (relay-events.jsonl.N)."))

        endpoints = [Endpoint(name="gateway", url=self._base, role="api", auth="bearer")]
        summary_line = {"up": "gateway up", "degraded": "gateway degraded", "down": "gateway down", "unknown": "unknown"}[health]
        summary_line += f" · {plugin_state}" if plist is not None else ""
        return SystemStatus(system="relay", health=health, version=version,
                            summary=summary_line[:240], endpoints=endpoints, components=components,
                            problems=problems, checked_at=now)

    @staticmethod
    def _plugin_row(plist: list[dict[str, Any]] | None) -> dict[str, Any] | None:
        if not plist:
            return None
        for r in plist:
            if r.get("enabled") is True and not r.get("tombstoned"):
                return r
        return plist[0]

    async def metrics(self) -> MetricsSnapshot:
        now = time.time()
        probe, unit, plist = await asyncio.gather(self._probe(), self._unit(), self._plugins_list())
        if not probe.reachable:
            return MetricsSnapshot(system="relay", ts=now, available=False, points=[],
                                   error=f"gateway not reachable at {self._base} ({probe.detail})")
        pts = [MetricPoint(key="relay.gateway_up", value=1.0, unit="1")]
        summary, plugins_mtime = await asyncio.to_thread(self._plugins_summary)
        row = self._plugin_row(plist)
        if plist is not None:
            # "loaded" = enabled + valid + policy ok + a gateway that answers and started after the config last changed.
            fresh = not (unit.active_since and plugins_mtime and plugins_mtime > unit.active_since + 1)
            loaded = bool(row and row.get("enabled") is True and row.get("validation_state") == "valid"
                          and row.get("policy_state") == "valid" and fresh)
            pts.append(MetricPoint(key="relay.plugin_loaded", value=1.0 if loaded else 0.0, unit="1"))
        atof = await asyncio.to_thread(self._scan_dir, "relay_atof_dir", None)
        if atof is not None:
            pts.append(MetricPoint(key="relay.atof_files", value=float(atof[0]), unit="1"))
            pts.append(MetricPoint(key="relay.atof_bytes", value=float(atof[1]), unit="B"))
        atif = await asyncio.to_thread(self._scan_dir, "relay_atif_dir", re.compile(r".+\.json$"))
        if atif is not None:
            pts.append(MetricPoint(key="relay.atif_trajectories", value=float(atif[0]), unit="1"))
        if summary.parsed:
            pts.append(MetricPoint(key="relay.exporters_enabled", value=float(summary.exporters_enabled), unit="1"))
        return MetricsSnapshot(system="relay", ts=now, available=True, points=pts)

    # ── events (ATOF) ───────────────────────────────────────────────────────────────────────
    def _atof_files_newest_first(self) -> list[Path]:
        root = self._path("relay_atof_dir")
        found: list[tuple[int, Path]] = []
        with os.scandir(root) as it:
            for i, e in enumerate(it):
                if i >= MAX_SCAN_ENTRIES:
                    break
                if not ATOF_NAME.match(e.name) or e.is_symlink() or not e.is_file(follow_symlinks=False):
                    continue
                m = ATOF_ROTATION.search(e.name)
                found.append((int(m.group(1)) if m else 0, Path(e.path)))
        found.sort(key=lambda t: (t[0], t[1].name))
        return [p for _, p in found]

    def _read_recent(self, limit: int) -> list[SystemEvent]:
        root = self._path("relay_atof_dir")
        out: deque[SystemEvent] = deque(maxlen=limit)
        need = limit
        collected: list[list[SystemEvent]] = []
        for p in self._atof_files_newest_first()[:4]:
            try:
                buf, partial = gz_tail_bytes(p, root) if p.name.endswith(".gz") else tail_bytes(p, root)
            except OSError:
                continue
            lines = buf.split(b"\n")
            if partial:
                lines = lines[1:]           # first line may be cut mid-record
            evs = [e for e in (parse_atof_line(ln) for ln in lines) if e is not None and e.data.get("name") != "llm.chunk"]
            if evs:
                collected.append(evs[-need:])
                need -= len(collected[-1])
            if need <= 0:
                break
        for chunk in reversed(collected):    # oldest file's events first
            out.extend(chunk)
        return list(out)

    async def recent_events(self, limit: int = 100) -> list[SystemEvent]:
        """Chronological (oldest first), at most `limit`, newest ATOF file(s) only. llm.chunk marks are skipped (noise)."""
        limit = max(1, min(limit, 1000))
        try:
            return await asyncio.to_thread(self._read_recent, limit)
        except (OSError, KeyError):
            return []

    def _follow_step(self, state: dict[str, Any]) -> list[SystemEvent]:
        """Read whatever was appended since the last poll. `state` holds ino/offset/remainder for THIS iterator."""
        root = self._path("relay_atof_dir")
        files = [f for f in self._atof_files_newest_first() if not f.name.endswith(".gz")]   # only plain files grow
        active = files[0] if files else None
        if active is None:
            return []
        if not _contained(active, root):
            return []
        fd, st = _open_regular(active)
        try:
            if state.get("ino") != st.st_ino:
                state.update(ino=st.st_ino, offset=0 if state.get("ino") is not None else st.st_size, rest=b"")
            elif st.st_size < state["offset"]:
                state.update(offset=0, rest=b"")           # truncated in place
            if st.st_size <= state["offset"]:
                return []
            os.lseek(fd, state["offset"], os.SEEK_SET)
            chunk = os.read(fd, min(FOLLOW_CHUNK_BYTES, st.st_size - state["offset"]))
        finally:
            os.close(fd)
        state["offset"] += len(chunk)
        buf = state["rest"] + chunk
        parts = buf.split(b"\n")
        rest = parts.pop()
        state["rest"] = rest if len(rest) < MAX_LINE_BYTES else b""
        return [e for e in (parse_atof_line(ln) for ln in parts) if e is not None and e.data.get("name") != "llm.chunk"]

    async def follow_events(self, since_ts: float | None = None) -> AsyncIterator[SystemEvent]:
        state: dict[str, Any] = {}
        if since_ts is not None:
            with contextlib.suppress(OSError, KeyError):
                for ev in await asyncio.to_thread(self._read_recent, 500):
                    if ev.ts > since_ts:
                        yield ev
        while True:
            try:
                events = await asyncio.to_thread(self._follow_step, state)
            except (OSError, KeyError):
                events = []
            for ev in events:
                yield ev
            await asyncio.sleep(self.follow_interval)

    # ── configure: files ────────────────────────────────────────────────────────────────────
    def config_files(self) -> list[ConfigFileSpec]:
        policy_why = ("Carries the plugin signature-enforcement policy (attestation mode and trusted_public_keys). "
                      "Local security policy: signature enforcement settings are a human decision, so in v1 the console shows a proposed diff only "
                      "and never writes this file.")
        return [
            ConfigFileSpec(
                id="routes", label="Switchyard routes (Relay plugin)", format="toml", editable=True,
                description="schema_version-1 Switchyard deployment loaded by the Relay native plugin (host-side copy; "
                            "clients here speak anthropic_messages, the judge speaks openai_chat).",
                display_path="clients/nemo-relay/routes.toml",
                validators=["TOML parse", "Switchyard schema mirror (semantic)"], authoritative=False,
                apply_effect=f"restarts {self.unit}; ~5-10 s of dropped requests on :4040; auto-rolled back if the gateway is not healthy"),
            ConfigFileSpec(
                id="plugins", label="Relay plugins & observability policy", format="toml", editable=False,
                description="Plugin registration, attestation policy, PII redaction and the ATOF/ATIF/OTLP exporters.",
                display_path="~/.config/nemo-relay/plugins.toml", why_readonly=policy_why,
                validators=["nemo-relay plugins validate"]),
            ConfigFileSpec(
                id="config", label="Relay user config", format="toml", editable=False,
                description="Relay user config layer (agents and CLI defaults).",
                display_path="~/.config/nemo-relay/config.toml", why_readonly=policy_why,
                validators=["nemo-relay doctor --offline"]),
        ]

    async def read_config(self, file_id: str) -> ConfigFile:
        self.config_path(file_id)                                          # KeyError for an unknown id
        spec = next(s for s in self.config_files() if s.id == file_id)
        raw, st = await asyncio.to_thread(self._read_file, file_id)
        return ConfigFile(spec=spec, content=scrub(raw.decode("utf-8", "replace")),
                          sha256=hashlib.sha256(raw).hexdigest(), mtime=st.st_mtime)

    async def validate_config(self, file_id: str, content: str) -> ValidationResult:
        if file_id != "routes":
            if file_id not in self._FILES:
                raise KeyError(file_id)
            diags = []
            try:
                tomllib.loads(content)
            except tomllib.TOMLDecodeError as e:
                diags.append(Diagnostic(severity="error", message=f"TOML syntax error: {e}"))
            return ValidationResult(ok=not diags, validator="TOML parse", authoritative=False, diagnostics=diags,
                                    note="Read-only in the console; this parse is only for reviewing a proposed diff. " + VALIDATION_CAVEATS)
        if len(content.encode()) > MAX_CONFIG_BYTES:
            return ValidationResult(ok=False, validator="toml+switchyard-schema-mirror", authoritative=False,
                                    diagnostics=[Diagnostic(severity="error", message="content exceeds 256 KiB")], note=ROUTES_NOTE)
        diags = validate_routes_semantics(content)
        if any(p.search(content) for p in (SECRET_PATTERNS[0], SECRET_PATTERNS[1])):
            diags.append(Diagnostic(severity="error", message="content looks like it contains a literal secret; use api_key_env = \"ENV_NAME\""))
        ok = not any(d.severity == "error" for d in diags)
        return ValidationResult(ok=ok, validator="toml+switchyard-schema-mirror", authoritative=False,
                                diagnostics=diags, note=ROUTES_NOTE)

    async def _wait_healthy(self) -> tuple[bool, str]:
        deadline = time.monotonic() + self.restart_timeout
        good = 0
        baseline: tuple[int, int] | None = None
        last = "no health check completed"
        while True:
            unit, probe = await asyncio.gather(self._unit(), self._probe())
            running = unit.known and unit.active == "active" and unit.sub == "running"
            if running and probe.reachable and (probe.status or 0) < 500:
                ident = (unit.pid, unit.restarts)
                if baseline is None or ident == baseline:
                    baseline = ident
                    good += 1
                else:                               # pid or restart count moved: the unit is flapping
                    baseline, good = ident, 1
                    last = "unit restarted again while settling (crash loop?)"
                if good >= self.settle_checks:
                    return True, f"gateway healthy: {probe.detail}, pid {unit.pid}, {good} consecutive checks"
            else:
                good = 0
                last = f"unit={unit.active}/{unit.sub} probe={probe.detail}"
            if time.monotonic() >= deadline:
                return False, f"gateway not healthy within {self.restart_timeout:.0f}s: {last}"
            await asyncio.sleep(self.poll_interval)

    async def _restart(self) -> tuple[bool, str]:
        r = await self._run(["systemctl", "--user", "restart", self.unit], timeout=60.0, max_bytes=16_000)
        if r.returncode != 0 or r.timed_out:
            return False, scrub(f"systemctl restart failed (rc={r.returncode}, timed_out={r.timed_out}): {(r.stderr or r.stdout)[:400]}")
        ok, detail = await self._wait_healthy()
        if not ok:
            st = await self._run(["systemctl", "--user", "status", "--no-pager", "-n", "15", self.unit], timeout=10.0, max_bytes=8_000)
            detail += "\n" + scrub(st.stdout or st.stderr)[-1800:]
        return ok, detail

    async def post_apply(self, file_id: str, *, restart: bool) -> ApplyResult:
        if file_id != "routes":
            return ApplyResult(ok=False, detail=f"{file_id} is read-only in the console", post_check="skipped")
        if not restart:
            return ApplyResult(ok=True, restarted=False, post_check="skipped",
                               detail="written without restart: the gateway keeps its old routes until it is restarted")
        ok, detail = await self._restart()
        return ApplyResult(ok=ok, restarted=True, post_check="healthy" if ok else "unhealthy", detail=detail[:2500])

    # ── act ─────────────────────────────────────────────────────────────────────────────────
    def actions(self) -> list[ActionSpec]:
        return [
            ActionSpec(id="gateway_restart", label="Restart gateway", danger="disruptive",
                       description=f"systemctl --user restart {self.unit}, then health-check for up to 30 s. Drops in-flight requests on :4040 "
                                   "(opencode and other host clients)."),
            ActionSpec(id="gateway_status", label="Gateway status", danger="safe",
                       description=f"systemctl --user status {self.unit} (Relay has no `gateway status` subcommand)."),
            ActionSpec(id="doctor_offline", label="Doctor (offline)", danger="safe",
                       description="nemo-relay doctor --offline --json, summarised, with notes on its known misleading lines."),
            ActionSpec(id="agents_list", label="List agents", danger="safe",
                       description="nemo-relay agents --json: supported coding agents and whether each is detected."),
            ActionSpec(id="plugins_validate", label="Validate plugins", danger="safe",
                       description="nemo-relay plugins validate <id> --json for every registered plugin. Passing does NOT prove the gateway will start."),
        ]

    async def run_action(self, action_id: str, params: dict[str, object]) -> ActionResult:
        t0 = time.monotonic()

        def done(ok: bool, out: str, problems: list[Problem] | None = None) -> ActionResult:
            return ActionResult(ok=ok, output=scrub(out)[:20_000], duration_ms=int((time.monotonic() - t0) * 1000),
                                problems=problems or [])

        if action_id == "gateway_restart":
            ok, detail = await self._restart()
            return done(ok, detail, [] if ok else [Problem(
                severity="error", code="relay.gateway.restart_unhealthy", message="Gateway did not become healthy after restart.",
                fix=f"systemctl --user status {self.unit}")])
        if action_id == "gateway_status":
            r = await self._run(["systemctl", "--user", "status", "--no-pager", "-n", "20", self.unit], timeout=10.0)
            return done(r.returncode in (0, 3), r.stdout or r.stderr)      # 3 = inactive, a valid answer
        if action_id == "agents_list":
            r = await self._run(["nemo-relay", "agents", "--json"], timeout=20.0)
            if r.returncode != 0:
                return done(False, r.stderr or r.stdout)
            try:
                rows = json.loads(r.stdout)
            except ValueError:
                return done(False, "agents --json did not return JSON")
            lines = [f"{x.get('name')}: {x.get('status')} configured={x.get('configured')} version={x.get('version')}"
                     for x in rows if isinstance(x, dict)]
            return done(True, "\n".join(lines))
        if action_id == "doctor_offline":
            r = await self._run(["nemo-relay", "doctor", "--offline", "--json"], timeout=30.0, max_bytes=1_000_000)
            if r.returncode != 0:
                return done(False, (r.stderr or r.stdout)[:2000])
            try:
                d = json.loads(r.stdout)
            except ValueError:
                return done(False, "doctor --json did not return JSON")
            return done(True, self._doctor_summary(d))
        if action_id == "plugins_validate":
            plist = await self._plugins_list()
            ids = [str(r["id"]) for r in (plist or []) if isinstance(r.get("id"), str) and PLUGIN_ID.fullmatch(str(r["id"]))]
            if not ids:
                return done(False, "no registered plugins found (`nemo-relay plugins list --json` returned none)")
            out: list[str] = []
            ok = True
            for pid in ids:
                r = await self._run(["nemo-relay", "plugins", "validate", "--json", "--", pid], timeout=30.0)
                try:
                    dd = json.loads(r.stdout).get("data", {})
                except (ValueError, AttributeError):
                    dd = {}
                valid = bool(dd.get("valid")) and r.returncode == 0
                ok = ok and valid
                out.append(f"{pid}: valid={dd.get('valid')} authenticity={dd.get('authenticity_state')} "
                           f"integrity={dd.get('integrity_state')} policy={dd.get('policy_state')} attestation={dd.get('attestation_mode')}")
                for e in (dd.get("errors") or [])[:10]:
                    out.append(f"  error: {str(e)[:300]}")
            out.append("NOTE: " + VALIDATION_CAVEATS)
            return done(ok, "\n".join(out))
        raise KeyError(action_id)

    @staticmethod
    def _doctor_summary(d: dict[str, Any]) -> str:
        lines = [f"nemo-relay {d.get('binary_version')} (doctor schema {d.get('schema_version')})"]
        cfg = _d(d.get("configuration"))
        ph = _d(cfg.get("plugin_host_validation"))
        lines.append(f"plugin host validation: {ph.get('status')} - {ph.get('details')}")
        rep = _d(ph.get("report"))
        for p in rep.get("dynamic_plugins", []) if isinstance(rep.get("dynamic_plugins"), list) else []:
            if isinstance(p, dict):
                s = _d(p.get("status"))
                lines.append(f"  plugin {p.get('plugin_id')}: manifest={s.get('manifest')} integrity={s.get('integrity')} "
                             f"authenticity={s.get('authenticity')} policy_satisfied={s.get('policy_satisfied')}")
        for o in d.get("observability", []) if isinstance(d.get("observability"), list) else []:
            if isinstance(o, dict):
                lines.append(f"observability: {o.get('name')}: {o.get('status')} - {str(o.get('details'))[:160]}")
        ua = cfg.get("upstream_auth")
        if isinstance(ua, dict):
            lines.append(f"upstream_auth (CLI env only): {json.dumps(ua)}")
        lines.append("")
        lines.extend("NOTE: " + c for c in DOCTOR_CAVEATS)
        return "\n".join(lines)

    # ── interact ────────────────────────────────────────────────────────────────────────────
    def playground(self) -> PlaygroundSpec | None:
        """OpenAI wire on POST /v1/chat/completions. Grounded in clients/nemo-relay/README.md ("routed completions on
        both the OpenAI wire (opencode) and the Anthropic wire") and opencode.json (baseURL :4040/v1, bearer from env);
        `OPTIONS /v1/messages` answering `allow: POST` confirms the Anthropic route exists too. No prompt was sent."""
        ids = list(DEFAULT_ROUTE_IDS)
        with contextlib.suppress(OSError, KeyError, PermissionError, tomllib.TOMLDecodeError):
            raw, _ = self._read_file("routes")
            parsed = tomllib.loads(raw.decode("utf-8", "replace")).get("routes", {})
            found = [r["id"] for r in parsed.values() if isinstance(r, dict) and isinstance(r.get("id"), str) and IDENT.fullmatch(r["id"])]
            if found:
                ids = found
        default = PLAYGROUND_DEFAULT_ROUTE if PLAYGROUND_DEFAULT_ROUTE in ids else ids[0]
        return PlaygroundSpec(
            wire="openai_chat", models=ids, default_model=default,
            note="Chat goes through the Relay gateway on :4040 (Switchyard plugin routes) and is recorded to ATOF/ATIF (PII-redacted, "
                 "if enabled). /v1/* answers 401 without a bearer, so the core attaches a LiteLLM virtual key by env NAME: "
                 "playground_key_env['relay'] (default LITELLM_API_KEY, what opencode sends). Route ids come from "
                 "routes.toml; engines behind them may be down.")

    def playground_url(self) -> str | None:
        return f"{self._base}/v1/chat/completions"
