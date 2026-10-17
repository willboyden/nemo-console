"""Topology: the three managed systems (health from their adapters) plus litellm / engine probes."""
from __future__ import annotations

import asyncio
import time
from collections.abc import Sequence

import httpx

from nemo_console.config import DEFAULT_TOPOLOGY_ENGINES, TopologyEngine
from nemo_console.domain.models import Health, Topology, TopologyEdge, TopologyNode
from nemo_console.services.engines import EngineProbe
from nemo_console.services.status import StatusService

# Engine nodes come from the setting `topology_engines` (addendum 9.6; defaults describe the author's deployment).
# OPT-IN engines stopped = "off", not red; a container that does not exist at all = "off" labelled "not deployed".
TTL = 5.0
NOT_DEPLOYED = " (not deployed)"
FIXED_IDS = ("clients", "relay", "litellm", "switchyard", "dynamo-frontend")


class TopologyService:
    def __init__(self, status: StatusService, http: httpx.AsyncClient, probe: EngineProbe, litellm_url: str,
                 engines: Sequence[TopologyEngine] | None = None) -> None:
        self.status = status
        self.http = http
        self.probe = probe
        self.litellm_url = litellm_url.rstrip("/")
        listed = list(engines) if engines is not None else [TopologyEngine(**e) for e in DEFAULT_TOPOLOGY_ENGINES]
        seen = set(FIXED_IDS)                   # a configured engine may not shadow a fixed node or repeat an id
        self.engines: list[TopologyEngine] = []
        for e in listed:
            if e.id not in seen:
                seen.add(e.id)
                self.engines.append(e)
        self._cache: tuple[float, Topology] | None = None

    async def _litellm(self) -> Health:
        try:
            r = await self.http.get(f"{self.litellm_url}/health/liveliness", timeout=2.0)
            return "up" if r.status_code == 200 else "degraded"
        except httpx.HTTPError:
            return "down"

    async def _engine(self, container: str, *, opt_in: bool = False) -> tuple[Health, bool]:
        """(health, deployed). Not deployed (no such container) is "off" for every engine, never red."""
        st = await self.probe.state(container)
        if st == "running":
            return "up", True
        if st == "stopped":
            return ("off" if opt_in else "down"), True
        if st == "absent":
            return "off", False
        return "unknown", True

    async def get(self) -> Topology:
        if self._cache and time.monotonic() - self._cache[0] < TTL:
            return self._cache[1]
        statuses, litellm, engines = await asyncio.gather(
            self.status.all(), self._litellm(),
            asyncio.gather(*(self._engine(e.container, opt_in=e.opt_in) for e in self.engines)))
        h = {s.system: s.health for s in statuses}
        dyn = h.get("dynamo", "unknown")
        nodes = [
            TopologyNode(id="clients", label="Clients (Open WebUI, opencode, agents)", kind="client", health="unknown"),
            TopologyNode(id="relay", label="NeMo Relay", kind="gateway", health=h.get("relay", "unknown"), system="relay"),
            TopologyNode(id="litellm", label="LiteLLM router", kind="router", health=litellm),
            TopologyNode(id="switchyard", label="NeMo Switchyard", kind="router", health=h.get("switchyard", "unknown"),
                         system="switchyard"),
            TopologyNode(id="dynamo-frontend", label="Dynamo frontend (opt-in)", kind="engine", health=dyn, system="dynamo"),
            *(TopologyNode(id=e.id, label=e.label + ("" if deployed else NOT_DEPLOYED), kind="engine", health=health)
              for e, (health, deployed) in zip(self.engines, engines, strict=True)),
        ]
        edges = [
            TopologyEdge(src="clients", dst="litellm", label="/v1"),
            TopologyEdge(src="clients", dst="relay", label="agent gateway"),
            TopologyEdge(src="relay", dst="litellm", label="scoped key"),
            # opencode's default model is a Switchyard route (:4010), so this is on the default path now. Labels stay
            # under ~10 characters: a longer chip is clipped by the nodes beside the column gap ("agent gateway").
            TopologyEdge(src="clients", dst="switchyard", label="default",
                         active=h.get("switchyard") in ("up", "degraded")),
            TopologyEdge(src="switchyard", dst="litellm", label="targets"),
            TopologyEdge(src="litellm", dst="dynamo-frontend", label="alias", active=dyn == "up"),
            *(TopologyEdge(src="litellm", dst=e.id, label="alias", active=health == "up")
              for e, (health, _deployed) in zip(self.engines, engines, strict=True)),
        ]
        topo = Topology(nodes=nodes, edges=edges, ts=time.time())
        self._cache = (time.monotonic(), topo)
        return topo
