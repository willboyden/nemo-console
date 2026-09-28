"""Playground proxy: streamed, clamped, never persisted. The credential is resolved server-side from an env-var NAME."""
from __future__ import annotations

import os
from collections.abc import AsyncIterator
from dataclasses import dataclass
from typing import Any

import httpx

from nemo_console.adapters.base import SystemAdapter
from nemo_console.services.common import KNOWN, redact
from nemo_console.services.errors import ApiError

ROUTE_HEADERS = ("x-served-by-route", "x-switchyard-route", "x-route")
TIER_HEADERS = ("x-served-by-tier", "x-switchyard-tier", "x-tier")
MAX_NONSTREAM_BYTES = 4 * 1024 * 1024
MAX_ERROR_BYTES = 4096


@dataclass
class Prepared:
    url: str
    headers: dict[str, str]
    body: dict[str, Any]
    stream: bool
    secret: str | None


def resolve_secret(key_env: dict[str, str], system: str, environ: dict[str, str] | None = None) -> tuple[str | None, str | None]:
    """Returns (env NAME, value). Value is registered with the redactor so it can never leak out of any response/log."""
    name = key_env.get(system)
    if not name:
        return None, None
    value = (environ if environ is not None else os.environ).get(name)
    if value:
        KNOWN.add(value)
    return name, value or None


def prepare(a: SystemAdapter, body: dict[str, Any], key_env: dict[str, str], max_tokens_cap: int,
            environ: dict[str, str] | None = None) -> Prepared:
    spec = a.playground()
    url = a.playground_url()
    if spec is None or url is None:
        raise ApiError(404, "not_found", "this system has no playground")
    name, secret = resolve_secret(key_env, a.id, environ)
    if name and not secret:
        raise ApiError(503, "credential_missing", f"env var {name} is not set in the console process")
    if spec.wire == "openai_chat" and body.get("system"):
        body = {**body, "messages": [{"role": "system", "content": body["system"]}, *body["messages"]]}
    body = {k: v for k, v in body.items() if not (k == "system" and spec.wire == "openai_chat")}
    mt = body.get("max_tokens")
    body = {**body, "max_tokens": max(1, min(int(mt), max_tokens_cap)) if mt else min(1024, max_tokens_cap)}
    headers = {"content-type": "application/json", "accept-encoding": "identity", "accept": "text/event-stream" if body.get("stream") else "application/json"}
    if secret:
        headers["authorization"] = f"Bearer {secret}"
        if spec.wire == "anthropic_messages":
            headers["x-api-key"] = secret
    if spec.wire == "anthropic_messages":
        headers["anthropic-version"] = "2023-06-01"
    return Prepared(url, headers, body, bool(body.get("stream")), secret)


def served_by(headers: httpx.Headers) -> dict[str, str]:
    out: dict[str, str] = {}
    for src, dst in ((ROUTE_HEADERS, "X-Served-By-Route"), (TIER_HEADERS, "X-Served-By-Tier")):
        for h in src:
            if v := headers.get(h):
                out[dst] = redact(v)[:100].replace("\n", " ")
                break
    return out


async def open_upstream(http: httpx.AsyncClient, p: Prepared, timeout: float = 600.0) -> httpx.Response:
    """Send and return a response whose body is NOT yet read. Raises ApiError on transport/upstream errors."""
    req = http.build_request("POST", p.url, json=p.body, headers=p.headers, timeout=httpx.Timeout(timeout, connect=5.0))
    try:
        resp = await http.send(req, stream=True)
    except httpx.HTTPError as e:
        raise ApiError(502, "upstream_unreachable", redact(f"could not reach the system: {type(e).__name__}")) from None
    if resp.status_code >= 400:
        try:
            raw = b""
            async for chunk in resp.aiter_bytes():
                raw += chunk
                if len(raw) >= MAX_ERROR_BYTES:
                    break
        finally:
            await resp.aclose()
        text = redact(raw[:MAX_ERROR_BYTES].decode("utf-8", "replace"))
        raise ApiError(502, "upstream_error", f"system answered {resp.status_code}: {text}"[:1200])
    return resp


async def relay_stream(resp: httpx.Response, secret: str | None) -> AsyncIterator[bytes]:
    try:
        async for chunk in resp.aiter_raw():
            if secret and secret.encode() in chunk:
                chunk = chunk.replace(secret.encode(), b"[redacted]")
            yield chunk
    finally:
        await resp.aclose()


async def read_capped(resp: httpx.Response, secret: str | None) -> bytes:
    buf = bytearray()
    try:
        async for chunk in resp.aiter_bytes():
            buf += chunk
            if len(buf) > MAX_NONSTREAM_BYTES:
                raise ApiError(502, "upstream_too_large", "upstream response too large")
    finally:
        await resp.aclose()
    data = bytes(buf)
    return data.replace(secret.encode(), b"[redacted]") if secret else data
