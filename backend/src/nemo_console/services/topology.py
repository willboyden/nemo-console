"""Topology: the three managed systems (health from their adapters) plus litellm / engine probes."""
from __future__ import annotations

import asyncio
import time

import httpx

from nemo_console.domain.models import Health, Topology, TopologyEdge, TopologyNode
from nemo_console.services.engines import EngineProbe
from nemo_console.services.status import StatusService

SHARED_ENGINE = "ai-lab-vllm-qwen38-vllm-1"     # fixed literal: the engine that shares the Workstation card with Dynamo
# fixed literal: the lab's default engine since 2026-09-26 (SGLang TP2, one rank on EACH card) and the capable tier
FLASHNEXT_ENGINE = "ai-lab-sglang-flashnext-flashnext-1"
TTL = 5.0


class TopologyService:
    def __init__(self, status: StatusService, http: httpx.AsyncClient, probe: EngineProbe, litellm_url: str) -> None:
        self.status = status
        self.http = http
        self.probe = probe
        self.litellm_url = litellm_url.rstrip("/")
        self._cache: tuple[float, Topology] | None = None

    async def _litellm(self) -> Health:
        try:
            r = await self.http.get(f"{self.litellm_url}/health/liveliness", timeout=2.0)
            return "up" if r.status_code == 200 else "degraded"
        except httpx.HTTPError:
            return "down"

    async def _engine(self, container: str) -> Health:
        st = await self.probe.state(container)
        if st == "running":
            return "up"
        return "down" if st == "stopped" else "unknown"

    async def get(self) -> Topology:
        if self._cache and time.monotonic() - self._cache[0] < TTL:
            return self._cache[1]
        statuses, litellm, engine, flashnext = await asyncio.gather(
            self.status.all(), self._litellm(), self._engine(SHARED_ENGINE), self._engine(FLASHNEXT_ENGINE))
        h = {s.system: s.health for s in statuses}
        dyn = h.get("dynamo", "unknown")
        nodes = [
            TopologyNode(id="clients", label="Clients (Open WebUI, opencode, agents)", kind="client", health="unknown"),
            TopologyNode(id="relay", label="NeMo Relay", kind="gateway", health=h.get("relay", "unknown"), system="relay"),
            TopologyNode(id="litellm", label="LiteLLM router", kind="router", health=litellm),
            TopologyNode(id="switchyard", label="NeMo Switchyard", kind="router", health=h.get("switchyard", "unknown"),
                         system="switchyard"),
            TopologyNode(id="dynamo-frontend", label="Dynamo frontend", kind="engine", health=dyn, system="dynamo"),
            TopologyNode(id="sglang-flashnext", label="SGLang FlashNext (TP2, both cards)", kind="engine", health=flashnext),
            TopologyNode(id="vllm-qwen38", label="vLLM Qwen3.8 (shares the Workstation card)", kind="engine", health=engine),
        ]
        edges = [
            TopologyEdge(src="clients", dst="litellm", label="/v1"),
            TopologyEdge(src="clients", dst="relay", label="agent gateway"),
            TopologyEdge(src="relay", dst="litellm", label="scoped key"),
            TopologyEdge(src="clients", dst="switchyard", label="opt-in :4010", active=h.get("switchyard") == "up"),
            TopologyEdge(src="switchyard", dst="litellm", label="targets"),
            TopologyEdge(src="litellm", dst="dynamo-frontend", label="alias", active=dyn == "up"),
            TopologyEdge(src="litellm", dst="sglang-flashnext", label="alias", active=flashnext == "up"),
            TopologyEdge(src="litellm", dst="vllm-qwen38", label="alias", active=engine == "up"),
        ]
        topo = Topology(nodes=nodes, edges=edges, ts=time.time())
        self._cache = (time.monotonic(), topo)
        return topo
