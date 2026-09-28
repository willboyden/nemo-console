"""/routing/summary: aggregate 'routing.decision' SystemEvents from the Switchyard adapter (metadata only)."""
from __future__ import annotations

import time
from collections import Counter
from typing import Any

from nemo_console.adapters.base import SystemAdapter
from nemo_console.domain.models import SystemEvent

WINDOW_SECONDS = {"15m": 900, "1h": 3600, "24h": 86400, "7d": 604800}
MAX_LABELS = 200


def _num(v: Any) -> int:
    return int(v) if isinstance(v, int | float) and not isinstance(v, bool) and v >= 0 else 0


def _tokens(d: dict[str, Any]) -> tuple[int, int]:
    t = d.get("tokens")
    if isinstance(t, dict):
        p, c = _num(t.get("prompt")), _num(t.get("completion"))
        if p or c:
            return p, c
        return _num(t.get("input")), _num(t.get("output"))
    p, c = _num(d.get("prompt_tokens")), _num(d.get("completion_tokens"))
    if p or c:
        return p, c
    return _num(d.get("input_tokens")), _num(d.get("output_tokens"))


def _is_error(d: dict[str, Any]) -> bool:
    if d.get("error"):
        return True
    if d.get("ok") is False:
        return True
    st = d.get("status")
    return isinstance(st, int) and not isinstance(st, bool) and st >= 400


def summarize(events: list[SystemEvent], window: str, now: float | None = None) -> dict[str, Any]:
    now = now if now is not None else time.time()
    since = now - WINDOW_SECONDS[window]
    by: dict[str, Counter[str]] = {"route": Counter(), "model": Counter(), "tier": Counter()}
    prompt = completion = requests = errors = 0
    for e in events:
        if e.kind != "routing.decision" or e.ts < since:
            continue
        requests += 1
        errors += _is_error(e.data)
        p, c = _tokens(e.data)
        prompt += p
        completion += c
        for dim, counter in by.items():
            v = e.data.get(dim)
            label = str(v)[:80] if isinstance(v, str | int) and str(v) else "unknown"
            if label in counter or len(counter) < MAX_LABELS:
                counter[label] += 1
            else:
                counter["other"] += 1
    return {"window": window, "by_route": dict(by["route"]), "by_model": dict(by["model"]), "by_tier": dict(by["tier"]),
            "tokens": {"prompt": prompt, "completion": completion, "total": prompt + completion},
            "requests": requests, "errors": errors}


async def routing_summary(a: SystemAdapter, window: str) -> dict[str, Any]:
    return summarize(await a.recent_events(limit=5000), window)
