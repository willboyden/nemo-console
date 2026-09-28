"""Adapter registry. LOCKED: owned by the orchestrator. Lazy so a missing adapter never breaks import."""
from __future__ import annotations

from importlib import import_module

from nemo_console.adapters.base import AdapterContext, SystemAdapter
from nemo_console.domain.models import SystemId

_REGISTRY: dict[SystemId, tuple[str, str]] = {
    "switchyard": ("nemo_console.adapters.switchyard", "SwitchyardAdapter"),
    "relay": ("nemo_console.adapters.relay", "RelayAdapter"),
    "dynamo": ("nemo_console.adapters.dynamo", "DynamoAdapter"),
}
SYSTEM_IDS: tuple[SystemId, ...] = ("switchyard", "relay", "dynamo")


def build_adapter(system: SystemId, ctx: AdapterContext) -> SystemAdapter:
    module, cls = _REGISTRY[system]
    adapter_cls: type[SystemAdapter] = getattr(import_module(module), cls)
    return adapter_cls(ctx)
