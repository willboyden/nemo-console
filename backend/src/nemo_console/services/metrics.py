"""Metrics poller: 2 s ring buffer (15 min) in memory + 1-minute rollups in SQLite for 1h/24h history."""
from __future__ import annotations

import asyncio
import logging
import time
from collections import deque

from nemo_console.domain.models import MetricPoint, SystemId
from nemo_console.services.status import StatusService
from nemo_console.services.store import Store

log = logging.getLogger(__name__)
WINDOWS = {"15m": 900, "1h": 3600, "24h": 86400}
MAX_SERIES_PER_SYSTEM = 256          # bounded label cardinality
ROLLUP_KEEP_S = 8 * 86400


def series_name(p: MetricPoint) -> str:
    if not p.labels:
        return p.key
    return p.key + "{" + ",".join(f"{k}={v}" for k, v in sorted(p.labels.items())) + "}"


class MetricsPoller:
    def __init__(self, status: StatusService, store: Store, interval: float, ring_seconds: int) -> None:
        self.status = status
        self.store = store
        self.interval = interval
        self.ring_seconds = ring_seconds
        self.ring: dict[tuple[SystemId, str], deque[tuple[float, float]]] = {}
        self._pending: dict[tuple[SystemId, str], list[float]] = {}   # bucket, sum, max, n
        self._series_count: dict[SystemId, int] = {}
        self._last_prune = 0.0

    @property
    def maxlen(self) -> int:
        return max(2, int(self.ring_seconds / max(self.interval, 0.1)) + 1)

    async def tick(self) -> None:
        for system in self.status.adapters:
            snap = await self.status.metrics(system)
            self.record(system, snap.ts, snap.points if snap.available else [])

    def record(self, system: SystemId, ts: float, points: list[MetricPoint]) -> None:
        bucket = int(ts // 60)
        for p in points:
            name = series_name(p)
            k = (system, name)
            if k not in self.ring:
                if self._series_count.get(system, 0) >= MAX_SERIES_PER_SYSTEM:
                    continue
                self._series_count[system] = self._series_count.get(system, 0) + 1
                self.ring[k] = deque(maxlen=self.maxlen)
            self.ring[k].append((ts, p.value))
            cur = self._pending.get(k)
            if cur is not None and int(cur[0]) != bucket:
                self._flush_one(k, cur)
                cur = None
            if cur is None:
                self._pending[k] = [float(bucket), p.value, p.value, 1.0]
            else:
                cur[1] += p.value
                cur[2] = max(cur[2], p.value)
                cur[3] += 1
        if ts - self._last_prune > 3600:
            self._last_prune = ts
            self.store.execute("DELETE FROM rollup WHERE bucket < ?", (int((ts - ROLLUP_KEEP_S) // 60),))

    def _flush_one(self, k: tuple[SystemId, str], cur: list[float]) -> None:
        self.store.execute(
            "INSERT INTO rollup(system, series, bucket, avg, max, n) VALUES (?,?,?,?,?,?) "
            "ON CONFLICT(system, series, bucket) DO UPDATE SET avg=excluded.avg, max=excluded.max, n=excluded.n",
            (k[0], k[1], int(cur[0]), cur[1] / cur[3], cur[2], int(cur[3])))

    def flush(self) -> None:
        for k, cur in list(self._pending.items()):
            self._flush_one(k, cur)

    @staticmethod
    def _match(series: str, key: str) -> bool:
        if "{" in key:
            return series == key
        return series == key or series.startswith(key + "{")

    def history(self, system: SystemId, window: str, key: str, now: float | None = None) -> list[list[float]]:
        now = now if now is not None else time.time()
        span = WINDOWS[window]
        acc: dict[float, float] = {}
        if window == "15m":
            for (sys_id, name), dq in self.ring.items():
                if sys_id == system and self._match(name, key):
                    for ts, v in dq:
                        if ts >= now - span:
                            acc[ts] = acc.get(ts, 0.0) + v
        else:
            self.flush()
            since = int((now - span) // 60)
            for r in self.store.query("SELECT series, bucket, avg FROM rollup WHERE system=? AND bucket>=? ORDER BY bucket",
                                      (system, since)):
                if self._match(r["series"], key):
                    ts = float(r["bucket"] * 60)
                    acc[ts] = acc.get(ts, 0.0) + r["avg"]
        return [[ts, acc[ts]] for ts in sorted(acc)]

    async def run(self) -> None:
        while True:
            try:
                await self.tick()
            except Exception:
                log.exception("metrics tick failed")
            await asyncio.sleep(self.interval)
