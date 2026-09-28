"""NVIDIA Dynamo adapter (frontend + vLLM worker, image nvcr.io/nvidia/ai-dynamo/vllm-runtime:1.5.0).

Provenance of every fact used here (see also tests/dynamo/fixtures/, captured 2026-09-24):
  * flags, choices, defaults: `python3 -m dynamo.frontend --help` / `-m dynamo.vllm --help` in the pinned image
  * metric names: live scrape of the frontend `:8020/metrics` and the worker `:8081/metrics`
  * worker `:8081` is NOT reachable from the host in rootless Docker (container IP times out), so the worker is
    read with `docker exec <worker> python3 -c <urllib one-liner>` through the allowlisted runner
  * the running worker's argv is read from `docker inspect` (`Config.Cmd`), not guessed

Contract notes (ctx wiring; every key is optional, defaults in parentheses):
  * ctx.urls["dynamo"]            frontend base URL (http://127.0.0.1:8020)
  * ctx.paths["dynamo_compose"]   ABSOLUTE path of inference/dynamo/docker-compose.yml (required for config/actions)
  * ctx.state_dir/dynamo/override.yml   the generated compose override (ADR 8), mode 0600

Threat notes for the hand-editable `override` file (validate_config is a CLOSED allowlist, fail-closed):
  * only `services.dynamo-worker|dynamo-frontend` with `command` / `environment`; command must start with the
    fixed `python3 -m dynamo.{vllm,frontend}` and every token starting `-` (single- or double-dash) must be an
    allowlisted flag
  * `$` is refused in every command token and env value, except a command token byte-identical to a
    `${VAR:-default}` token already in the base compose (a NEW variable would make compose pull host env/.env values
    into the container argv/env, i.e. a secret-exfil path)
  * `--model` / `--served-model-name` must equal the base compose's values; environment keys must already exist in the
    base compose for that service, and their values may not hold `$` or look like a secret
  * then `docker compose config -q` as the authoritative parse. The generated (render_override) path satisfies all of it.

Lifecycle is `docker compose` on the repo compose plus the override; `scripts/use-model.sh` is never called.
"""
from __future__ import annotations

import asyncio
import contextlib
import hashlib
import json
import math
import os
import re
import shlex
import time
from collections.abc import AsyncIterator, Callable
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import httpx
import yaml

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
    ParamSpec,
    PlaygroundSpec,
    Problem,
    SystemEvent,
    SystemStatus,
    ValidationResult,
)

# ── fixed identities (container names come from the compose project name `ai-lab-dynamo`) ────────
PROJECT = "ai-lab-dynamo"
FRONTEND = "ai-lab-dynamo-dynamo-frontend-1"
WORKER = "ai-lab-dynamo-dynamo-worker-1"
MOCKER = "ai-lab-dynamo-dynamo-mocker-1"
SVC_FRONTEND = "dynamo-frontend"
SVC_WORKER = "dynamo-worker"
SVC_MOCKER = "dynamo-mocker"
DYNAMO_NAMES = (FRONTEND, WORKER, MOCKER)

SHARING_ENGINE = "ai-lab-vllm-qwen38-vllm-1"          # the engine the operator swaps against, same GPU UUID
SHARING_PROJECT = "ai-lab-vllm-qwen38"
SHARING_SERVICE = "vllm"
ROUTER_ALIAS = "qwen3.8-27b-1m"

DEFAULT_URL = "http://127.0.0.1:8020"
DEFAULT_MODEL = "unsloth/Qwen3.8-27B-NVFP4"          # the worker's --served-model-name in the compose
WORKER_METRICS_PORT = 8081                             # DYN_SYSTEM_PORT in the compose

_NAME_RE = re.compile(r"[a-zA-Z0-9][a-zA-Z0-9_.-]{0,127}")
_UUID_RE = re.compile(r"GPU-[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}")
_COMPOSE_NAMES = {"docker-compose.yml", "docker-compose.yaml", "compose.yml", "compose.yaml"}
_MAX_CONFIG_BYTES = 256 * 1024
_STRUCTURAL = "--dyn-enable-structural-tag"

# Choices copied from `--help` of the pinned image (dynamo.vllm / dynamo.frontend).
TOOL_PARSERS = [
    "qwen3_coder", "qwen25", "hermes", "llama3_json", "mistral", "pythonic", "phi4", "jamba", "harmony",
    "gemma4", "glm47", "kimi_k2", "kimi_k3", "nemotron_deci", "nemotron_nano", "deepseek_v3", "deepseek_v3_1",
    "deepseek_v3_2", "deepseek_v4", "minimax_m2", "minimax_m3", "inkling", "muse", "default",
]
REASONING_PARSERS = [
    "none", "qwen3", "deepseek_r1", "gpt_oss", "glm45", "granite", "mistral", "step3", "nemotron_v3",
    "nemotron_nano", "nemotron3", "nemotron_deci", "minimax_m2", "minimax_m3", "kimi", "kimi_k25", "kimi_k3",
    "gemma4", "deepseek_v3", "deepseek_v3_1", "deepseek_v3_2", "deepseek_v4", "inkling", "basic", "muse",
]
ROUTER_MODES = ["round-robin", "random", "power-of-two", "kv", "least-loaded", "device-aware-weighted"]

# Every flag an override command may carry (catalog flags + the fixed plumbing flags of the base compose).
_FIXED_FLAGS_WORKER = {"--model", "--served-model-name", "--discovery-backend", "--request-plane"}
_FIXED_FLAGS_FRONTEND = {"--http-host", "--http-port", "--discovery-backend", "--request-plane"}


class ParamError(ValueError):
    """A parameter value that is unknown, mistyped or out of range."""


# ── redaction / log parsing ─────────────────────────────────────────────────────────────────────
_ANSI = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]")
_LEAD_TS = re.compile(r"^\d{4}-\d{2}-\d{2}T[\d:.]+Z?\s+")
_DOCKER_TS = re.compile(r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(\.\d+)?Z\s?(.*)$", re.DOTALL)
_LEVEL = re.compile(r"\b(TRACE|DEBUG|INFO|WARNING|WARN|ERROR|CRITICAL|FATAL)\b")
_SECRETS = [
    (re.compile(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{6,}"), "Bearer [redacted]"),
    (re.compile(r"\bsk-[A-Za-z0-9_-]{8,}"), "[redacted]"),
    (re.compile(r"\bhf_[A-Za-z0-9]{10,}"), "[redacted]"),
    (re.compile(r"(?i)\b(api[_-]?key|token|secret|password|authorization)\b(\s*[=:]\s*)\S+"), r"\1\2[redacted]"),
]
_ABS_PATH = re.compile(r"(?<![\w:/.~-])(?:/[\w.@+=,-]+){2,}/?")
_NOISE = re.compile(r"http response sent.*status\W*200")


def redact(text: str) -> str:
    """Strip ANSI, mask secrets and collapse absolute paths to `<path>/leaf` (no host layout leaks)."""
    out = _ANSI.sub("", text)
    for pat, rep in _SECRETS:
        out = pat.sub(rep, out)

    def _path(m: re.Match[str]) -> str:
        leaf = m.group(0).rstrip("/").rsplit("/", 1)[-1]
        return f"<path>/{leaf}"

    return _ABS_PATH.sub(_path, out)


def parse_log_line(raw: str, component: str) -> SystemEvent | None:
    """One `docker logs --timestamps` line -> event (None for blanks and the health-poll noise)."""
    m = _DOCKER_TS.match(raw.rstrip("\n"))
    if not m:
        return None
    base, frac, rest = m.group(1), m.group(2) or "", m.group(3)
    try:
        ts = datetime.fromisoformat(base + (frac[:7] if frac else "")).replace(tzinfo=UTC).timestamp()
    except ValueError:
        return None
    plain = _ANSI.sub("", rest).strip()
    if not plain or _NOISE.search(plain):
        return None
    plain = _LEAD_TS.sub("", plain)
    lv = _LEVEL.search(plain)
    level = lv.group(1) if lv else "INFO"
    level = {"WARNING": "WARN", "FATAL": "ERROR", "CRITICAL": "ERROR"}.get(level, level)
    summary = redact(plain)
    if len(summary) > 400:
        summary = summary[:397] + "..."
    return SystemEvent(ts=ts, system="dynamo", kind="dynamo.log", summary=summary,
                       data={"component": component, "level": level})


# ── Prometheus text parsing (no client lib: the exposition is plain text) ───────────────────────
_SAMPLE = re.compile(r"^([A-Za-z_:][A-Za-z0-9_:]*)(?:\{(.*)\})?\s+(\S+)(?:\s+\S+)?$")
_LABEL = re.compile(r'([A-Za-z_][A-Za-z0-9_]*)="((?:[^"\\]|\\.)*)"')

Sample = tuple[str, dict[str, str], float]


def parse_prom(text: str) -> list[Sample]:
    out: list[Sample] = []
    for line in text.splitlines():
        if not line or line.startswith("#"):
            continue
        m = _SAMPLE.match(line.strip())
        if not m:
            continue
        try:
            val = float(m.group(3))
        except ValueError:
            continue
        out.append((m.group(1), dict(_LABEL.findall(m.group(2) or "")), val))
    return out


def _sum(samples: list[Sample], name: str, where: Callable[[dict[str, str]], bool] | None = None) -> float | None:
    vals = [v for n, lb, v in samples if n == name and (where is None or where(lb))]
    return sum(vals) if vals else None


def histogram_quantile(samples: list[Sample], base: str, q: float) -> float | None:
    """Lifetime quantile from cumulative `_bucket` samples, summed across label sets, linear interpolation."""
    cum: dict[float, float] = {}
    for n, lb, v in samples:
        if n == base + "_bucket" and "le" in lb:
            try:
                le = float(lb["le"])
            except ValueError:
                continue
            cum[le] = cum.get(le, 0.0) + v
    if not cum:
        return None
    bounds = sorted(cum)
    total = cum[bounds[-1]]
    if total <= 0:
        return None
    rank = q * total
    prev_b, prev_c = 0.0, 0.0
    for b in bounds:
        c = cum[b]
        if c >= rank:
            if b == float("inf"):
                finite = [x for x in bounds if x != float("inf")]
                return finite[-1] if finite else None
            if c == prev_c:
                return b
            return prev_b + (b - prev_b) * (rank - prev_c) / (c - prev_c)
        prev_b, prev_c = b, c
    return None


# ── override rendering (pure) ───────────────────────────────────────────────────────────────────
Item = tuple[str, list[str]]     # (flag, args); the pre-flag prefix is handled separately


def split_command(tokens: list[str]) -> tuple[list[str], list[Item]]:
    """`python3 -m dynamo.vllm --a 1 --b` -> (prefix, [(--a,[1]), (--b,[])]). Value tokens never start `--`."""
    prefix: list[str] = []
    items: list[Item] = []
    for tok in tokens:
        if tok.startswith("--"):
            items.append((tok, []))
        elif items:
            items[-1][1].append(tok)
        else:
            prefix.append(tok)
    return prefix, items


def join_command(prefix: list[str], items: list[Item]) -> list[str]:
    out = list(prefix)
    for flag, args in items:
        out.append(flag)
        out.extend(args)
    return out


def _set_flag(items: list[Item], flag: str, args: list[str] | None, *, also: tuple[str, ...] = ()) -> None:
    """Replace `flag` (and any `also` aliases) at its original position; None removes it; new flags append."""
    names = (flag, *also)
    pos = next((i for i, (f, _) in enumerate(items) if f in names), None)
    items[:] = [it for it in items if it[0] not in names]
    if args is None:
        return
    items.insert(len(items) if pos is None else min(pos, len(items)), (flag, list(args)))


def _norm_flag(f: str) -> str:
    return "--" + f[2:].replace("_", "-") if f.startswith("--") else f


def _dup_key(flag: str) -> str:
    """`--no-x` and `--x` are the same switch for duplicate detection."""
    return "--" + flag[2:].removeprefix("no-") if flag.startswith("--") else flag


def _parse_flags(tokens: list[str]) -> tuple[list[tuple[str, list[str], str]], list[str]]:
    """Tokens -> ([(normalised flag, values, raw flag)], stray positional tokens). Handles `--flag=value`."""
    out: list[tuple[str, list[str], str]] = []
    stray: list[str] = []
    for tok in tokens:
        if tok.startswith("-") and len(tok) > 1:
            raw, eq, val = tok.partition("=")
            out.append((_norm_flag(raw), [val] if eq else [], raw))
        elif out:
            out[-1][1].append(tok)
        else:
            stray.append(tok)
    return out, stray


def _spec_ok(vals: list[str]) -> bool:
    if len(vals) != 1:
        return False
    try:
        cfg = json.loads(vals[0])
    except ValueError:
        return False
    n = cfg.get("num_speculative_tokens") if isinstance(cfg, dict) else None
    return (isinstance(cfg, dict) and set(cfg) == {"method", "num_speculative_tokens"} and cfg["method"] == "mtp"
            and isinstance(n, int) and not isinstance(n, bool) and 1 <= n <= 8)


def _find(items: list[Item], flag: str) -> list[str] | None:
    return next((a for f, a in items if f == flag), None)


def _yq(s: str) -> str:
    """A YAML double-quoted scalar (a JSON string is one). `$` is NOT escaped: base-carried `${VAR:-x}` tokens must
    stay live so compose interpolates them exactly as it does for the base file (escaping would freeze them
    into literal text; measured with `docker compose config`). Generated values are validated and never hold `$`."""
    return json.dumps(s, ensure_ascii=True)


def _ints(v: object, key: str, lo: int, hi: int) -> int:
    if isinstance(v, bool):
        raise ParamError(f"{key}: expected an integer, got a boolean")
    if isinstance(v, str) and re.fullmatch(r"-?\d{1,18}", v.strip()):
        v = int(v)
    if isinstance(v, float) and v.is_integer():
        v = int(v)
    if not isinstance(v, int):
        raise ParamError(f"{key}: expected an integer")
    if not lo <= v <= hi:
        raise ParamError(f"{key}: {v} outside [{lo}, {hi}]")
    return v


def _floats(v: object, key: str, lo: float, hi: float) -> float:
    if isinstance(v, bool) or not isinstance(v, (int, float, str)):
        raise ParamError(f"{key}: expected a number")
    try:
        f = float(v)
    except ValueError:
        raise ParamError(f"{key}: expected a number") from None
    if math.isnan(f) or not lo <= f <= hi:
        raise ParamError(f"{key}: {v} outside [{lo}, {hi}]")
    return f


def _bools(v: object, key: str) -> bool:
    if not isinstance(v, bool):
        raise ParamError(f"{key}: expected true/false")
    return v


def _enum(v: object, key: str, choices: list[str]) -> str:
    if not isinstance(v, str) or v not in choices:
        raise ParamError(f"{key}: {v!r} is not one of {choices}")
    return v


def _catalog() -> list[ParamSpec]:
    w = "worker (dynamo.vllm)"
    return [
        ParamSpec(key="max_model_len", flag="--max-model-len", label="Context length (tokens)", type="int",
                  group=w, default=262144, min=1024, max=1048576,
                  help="Model context length (prompt and output). Bigger contexts need more KV cache; the KV "
                       "budget is set separately by kv_cache_memory_bytes."),
        ParamSpec(key="gpu_memory_utilization", flag="--gpu-memory-utilization", label="GPU memory utilization",
                  type="float", group=w, default=0.5, min=0.05, max=1.0,
                  help="Fraction of GPU memory for this vLLM instance (0..1), per instance. IGNORED by vLLM when "
                       "kv_cache_memory_bytes is set."),
        ParamSpec(key="kv_cache_memory_bytes", flag="--kv-cache-memory-bytes", label="KV cache size (bytes)",
                  type="int", group=w, default=20000000000, min=268435456, max=100000000000,
                  help="KV cache bytes per GPU. Overrides gpu_memory_utilization when set. (vLLM 0.28 name; "
                       "0.27's --kv-cache-memory does not exist here.)"),
        ParamSpec(key="max_num_seqs", flag="--max-num-seqs", label="Max concurrent sequences", type="int",
                  group=w, default=64, min=1, max=1024,
                  help="Maximum number of sequences processed in a single iteration."),
        ParamSpec(key="max_num_batched_tokens", flag="--max-num-batched-tokens", label="Max batched tokens",
                  type="int", group=w, default=8192, min=256, max=131072,
                  help="Maximum tokens processed in a single iteration (chunked-prefill budget)."),
        ParamSpec(key="speculative_enabled", flag="--speculative-config", label="Speculative decoding (MTP)",
                  type="bool", group=w, default=True,
                  help="Multi-token-prediction speculative decoding; the pinned checkpoint ships an MTP draft. "
                       "Off removes --speculative-config."),
        ParamSpec(key="num_speculative_tokens", flag="--speculative-config", label="Speculative tokens", type="int",
                  group=w, default=2, min=1, max=8,
                  help="num_speculative_tokens inside the speculative config (method stays mtp)."),
        ParamSpec(key="tool_call_parser", flag="--dyn-tool-call-parser", label="Tool-call parser", type="enum",
                  group=w, default="qwen3_coder", choices=TOOL_PARSERS,
                  help="Dynamo's own dialect (NOT vLLM's --tool-call-parser, which does nothing here). Qwen3.8 "
                       "uses qwen3_coder (the XML format vLLM calls qwen3_xml)."),
        ParamSpec(key="reasoning_parser", flag="--dyn-reasoning-parser", label="Reasoning parser", type="enum",
                  group=w, default="qwen3", choices=REASONING_PARSERS,
                  help="Splits <think> output into reasoning_content. `none` = no reasoning parsing."),
        ParamSpec(key="default_thinking_mode", flag="--dyn-default-thinking-mode", label="Default thinking mode",
                  type="enum", group=w, default="unset", choices=["unset", "enabled", "disabled"], advanced=True,
                  help="Deployment-level default for the chat template; a request's chat_template_kwargs win."),
        ParamSpec(key="enable_prefix_caching", flag="--enable-prefix-caching", label="Prefix caching", type="bool",
                  group=w, default=True, help="Reuse KV blocks across requests that share a prefix."),
        ParamSpec(key="enable_chunked_prefill", flag="--enable-chunked-prefill", label="Chunked prefill",
                  type="bool", group=w, default=True,
                  help="Prefill requests are chunked to the remaining max_num_batched_tokens."),
        ParamSpec(key="enable_structural_tag", flag=_STRUCTURAL, label="Structural-tag tool decoding", type="bool",
                  group=w, default=True, advanced=True,
                  help="Guided decoding for tool calls. MUST stay ON: without it tool_choice=\"required\" "
                       "degenerates into whitespace in reasoning_content and returns no tool call "
                       "(measured 2026-09-23)."),
        ParamSpec(key="router_mode", flag="--router-mode", label="Router mode", type="enum",
                  group="frontend (dynamo.frontend)", default="round-robin", choices=ROUTER_MODES,
                  help="How the frontend picks a worker. `kv` only helps with more than one worker and needs "
                       "NATS/ZMQ KV events published by the workers; with one worker it adds a hop for nothing."),
    ]


_CATALOG_BY_KEY = {p.key: p for p in _catalog()}
_WORKER_FLAGS = {p.flag for p in _catalog() if p.flag and p.group.startswith("worker")} | {
    "--enable-prefix-caching", "--no-enable-prefix-caching", "--enable-chunked-prefill",
    "--no-enable-chunked-prefill", "--no-dyn-enable-structural-tag"} | _FIXED_FLAGS_WORKER
_FRONTEND_FLAGS = {"--router-mode"} | _FIXED_FLAGS_FRONTEND


def validate_values(values: dict[str, object]) -> dict[str, Any]:
    """Allowlist + type + range check. Unknown keys are refused (never silently dropped)."""
    clean: dict[str, Any] = {}
    for key, raw in values.items():
        spec = _CATALOG_BY_KEY.get(key)
        if spec is None:
            raise ParamError(f"unknown parameter {key!r}")
        if spec.type == "int":
            clean[key] = _ints(raw, key, int(spec.min or 0), int(spec.max or 0))
        elif spec.type == "float":
            clean[key] = _floats(raw, key, float(spec.min or 0), float(spec.max or 1))
        elif spec.type == "bool":
            clean[key] = _bools(raw, key)
        else:
            clean[key] = _enum(raw, key, spec.choices or [])
    return clean


def param_warnings(values: dict[str, Any]) -> list[str]:
    w: list[str] = []
    if values.get("enable_structural_tag") is False:
        w.append("enable_structural_tag=false: tool_choice=\"required\" will return no tool call on this model "
                 "(guided decoding degenerates into whitespace). Keep it ON.")
    if values.get("router_mode") == "kv":
        w.append("router_mode=kv needs more than one worker and NATS/ZMQ KV events from the workers; with the "
                 "single worker (file discovery, no NATS) it adds a hop for nothing and may not route.")
    if "kv_cache_memory_bytes" in values and "gpu_memory_utilization" in values:
        w.append("kv_cache_memory_bytes is set, so vLLM ignores gpu_memory_utilization (per --help).")
    if float(values.get("gpu_memory_utilization", 0)) > 0.95:
        w.append("gpu_memory_utilization above 0.95 leaves little headroom on a card shared with other engines.")
    if values.get("enable_chunked_prefill") is False and int(values.get("max_num_batched_tokens", 1 << 30)) < int(
            values.get("max_model_len", 0)):
        w.append("chunked prefill is off and max_num_batched_tokens < max_model_len; vLLM may refuse to start "
                 "(recalled from vLLM behaviour, not verified against this image).")
    if values.get("reasoning_parser") == "none":
        w.append("reasoning_parser=none: <think> text stays inline in content.")
    return w


def _spec_json(items: list[Item], enabled: bool | None, n: int | None) -> list[str] | None:
    cur = _find(items, "--speculative-config")
    cfg: dict[str, Any] = {"method": "mtp"}
    if cur:
        with contextlib.suppress(ValueError, TypeError):
            parsed = json.loads(cur[0])
            if isinstance(parsed, dict):
                cfg = parsed
    if enabled is False:
        return None
    if enabled is None and n is None:
        return cur
    if n is not None:
        cfg["num_speculative_tokens"] = n
    return [json.dumps(cfg, separators=(",", ":"))]


def render_override(values: dict[str, object], base_worker: list[str], base_frontend: list[str] | None = None,
                    ) -> tuple[str, list[str]]:
    """PURE. Base commands + validated values -> (compose override YAML, warnings). Only supplied keys change."""
    v = validate_values(values)
    wp, wi = split_command(list(base_worker))
    fp, fi = split_command(list(base_frontend or []))
    win = [(f, list(a)) for f, a in wi]
    fin = [(f, list(a)) for f, a in fi]

    def num(key: str, flag: str, fmt: Callable[[Any], str] = str) -> None:
        if key in v:
            _set_flag(win, flag, [fmt(v[key])])

    num("max_model_len", "--max-model-len")
    num("gpu_memory_utilization", "--gpu-memory-utilization", lambda x: f"{x:.2f}")
    num("kv_cache_memory_bytes", "--kv-cache-memory-bytes")
    num("max_num_seqs", "--max-num-seqs")
    num("max_num_batched_tokens", "--max-num-batched-tokens")
    if "speculative_enabled" in v or "num_speculative_tokens" in v:
        _set_flag(win, "--speculative-config",
                  _spec_json(win, v.get("speculative_enabled"), v.get("num_speculative_tokens")))
    if "tool_call_parser" in v:
        _set_flag(win, "--dyn-tool-call-parser", [v["tool_call_parser"]])
    if "reasoning_parser" in v:
        _set_flag(win, "--dyn-reasoning-parser", None if v["reasoning_parser"] == "none" else [v["reasoning_parser"]])
    if "default_thinking_mode" in v:
        _set_flag(win, "--dyn-default-thinking-mode",
                  None if v["default_thinking_mode"] == "unset" else [v["default_thinking_mode"]])
    for key, flag in (("enable_prefix_caching", "--enable-prefix-caching"),
                      ("enable_chunked_prefill", "--enable-chunked-prefill")):
        if key in v:
            _set_flag(win, flag, [] if v[key] else None, also=("--no" + flag[1:],))
            if not v[key]:
                win.append(("--no" + flag[1:], []))
    if "enable_structural_tag" in v:
        _set_flag(win, _STRUCTURAL, [] if v["enable_structural_tag"] else None, also=("--no-dyn-enable-structural-tag",))
    if "router_mode" in v:
        _set_flag(fin, "--router-mode", None if v["router_mode"] == "round-robin" else [v["router_mode"]])

    lines = [
        "# Generated by Nemo Console (ADR 8). The repo compose is never edited: delete this file to revert.",
        "# Compose REPLACES `command` (it does not merge it), so the full argv is restated below; flags the",
        "# console does not manage are carried over verbatim from the base compose.",
        "# nemo-console-values: " + json.dumps(v, sort_keys=True, separators=(",", ":")),
    ]
    warns = param_warnings(v)
    lines += [f"# WARNING: {w}" for w in warns]
    lines += ["services:", f"  {SVC_WORKER}:", "    command:"]
    lines += [f"      - {_yq(t)}" for t in join_command(wp, win)]
    if fin != fi and base_frontend is not None:
        lines += [f"  {SVC_FRONTEND}:", "    command:"]
        lines += [f"      - {_yq(t)}" for t in join_command(fp, fin)]
    return "\n".join(lines) + "\n", warns


def parse_values_header(content: str) -> dict[str, Any]:
    for line in content.splitlines()[:12]:
        if line.startswith("# nemo-console-values:"):
            with contextlib.suppress(ValueError):
                data = json.loads(line.split(":", 1)[1])
                if isinstance(data, dict):
                    return data
    return {}


def _load_base_commands(text: str) -> tuple[list[str], list[str]]:
    """Worker and frontend argv from the raw base compose (folded `>` string or list)."""
    doc = yaml.safe_load(text)
    out: list[list[str]] = []
    for svc in (SVC_WORKER, SVC_FRONTEND):
        cmd = ((doc or {}).get("services") or {}).get(svc, {}).get("command")
        if isinstance(cmd, str):
            out.append(shlex.split(cmd))
        elif isinstance(cmd, list) and all(isinstance(t, str) for t in cmd):
            out.append(list(cmd))
        else:
            raise ParamError(f"base compose has no usable command for {svc}")
    return out[0], out[1]


def _time_ms(seconds: float | None) -> float | None:
    return None if seconds is None else seconds * 1000.0


_EXEC_SCRIPT = (
    "import urllib.request as u,sys\n"
    "def g(p):\n"
    f"  try: return u.urlopen('http://127.0.0.1:{WORKER_METRICS_PORT}'+p,timeout=5).read().decode()\n"
    "  except Exception as e: return ''\n"
    "sys.stdout.write(g('/health')+'\\n---METRICS---\\n'+g('/metrics'))\n"
)


class DynamoAdapter(SystemAdapter):
    id = "dynamo"
    label = "NVIDIA Dynamo"

    def __init__(self, ctx: AdapterContext, *, health_timeout: float = 600.0, poll_interval: float = 5.0,
                 follow_interval: float = 2.0) -> None:
        super().__init__(ctx)
        self.health_timeout = health_timeout
        self.poll_interval = poll_interval
        self.follow_interval = follow_interval
        self._models: list[str] = []
        self._card_uuid: str | None = None

    # ── plumbing ────────────────────────────────────────────────────────────────────────────
    @property
    def base_url(self) -> str:
        return self.ctx.urls.get("dynamo", DEFAULT_URL).rstrip("/")

    def _compose_path(self) -> Path | None:
        return self.ctx.paths.get("dynamo_compose")

    def _override_path(self) -> Path:
        return self.ctx.state_dir / "dynamo" / "override.yml"

    def _override_active(self) -> bool:
        p = self._override_path()
        try:
            return p.is_file() and not p.is_symlink() and bool(p.read_text().strip())
        except OSError:
            return False

    def _compose_argv(self, *tail: str, profile_mock: bool = False) -> list[str]:
        base = self._compose_path()
        if base is None:
            raise FileNotFoundError("ctx.paths['dynamo_compose'] is not configured")
        argv = ["docker", "compose", "--project-directory", str(base.parent), "-f", str(base)]
        if self._override_active():
            argv += ["-f", str(self._override_path())]
        if profile_mock:
            argv += ["--profile", "mock"]
        return [*argv, *tail]

    async def _inspect(self, names: list[str]) -> dict[str, dict[str, Any]]:
        """`docker inspect` (rc!=0 when some are absent, stdout still holds the rest). Verified by name."""
        safe = [n for n in names if _NAME_RE.fullmatch(n)]
        if not safe:
            return {}
        res = await self.ctx.runner.run(["docker", "inspect", *safe], timeout=15.0, max_bytes=2_000_000)
        try:
            data = json.loads(res.stdout or "[]")
        except ValueError:
            return {}
        out: dict[str, dict[str, Any]] = {}
        for d in data if isinstance(data, list) else []:
            name = str(d.get("Name", "")).lstrip("/")
            if name in safe:
                out[name] = d
        return out

    @staticmethod
    def _project_ok(d: dict[str, Any], service: str) -> bool:
        lb = (d.get("Config") or {}).get("Labels") or {}
        return lb.get("com.docker.compose.project") == PROJECT and lb.get("com.docker.compose.service") == service

    @staticmethod
    def _state(d: dict[str, Any] | None) -> str:
        if d is None:
            return "absent"
        st = d.get("State") or {}
        if st.get("Running"):
            return "running"
        if st.get("Restarting") or (st.get("Status") == "exited" and st.get("ExitCode") not in (0, None)):
            return "failed"
        return "stopped"

    async def _get_json(self, path: str) -> Any | None:
        try:
            r = await self.ctx.http.get(self.base_url + path, timeout=5.0)
            if r.status_code != 200:
                return None
            return r.json()
        except (httpx.HTTPError, ValueError):
            return None

    async def _worker_scrape(self) -> tuple[dict[str, Any] | None, list[Sample]]:
        """Worker /health + /metrics from INSIDE the container (host cannot reach the bus IP in rootless)."""
        res = await self.ctx.runner.run(["docker", "exec", WORKER, "python3", "-c", _EXEC_SCRIPT],
                                        timeout=20.0, max_bytes=2_000_000)
        if res.returncode != 0 or "---METRICS---" not in res.stdout:
            return None, []
        head, _, body = res.stdout.partition("---METRICS---")
        health: dict[str, Any] | None = None
        with contextlib.suppress(ValueError):
            parsed = json.loads(head.strip() or "null")
            health = parsed if isinstance(parsed, dict) else None
        return health, parse_prom(body)

    # ── observe ─────────────────────────────────────────────────────────────────────────────
    async def status(self) -> SystemStatus:
        now = time.time()
        try:
            return await self._status(now)
        except Exception as e:  # noqa: BLE001 - failure is data (base.py contract)
            return SystemStatus(system="dynamo", health="unknown", summary=f"status probe failed: {type(e).__name__}",
                                checked_at=now,
                                problems=[Problem(severity="error", code="dynamo.probe.failed",
                                                  message=redact(str(e))[:300], fix="check the docker runner/socket")])

    async def _status(self, now: float) -> SystemStatus:
        insp = await self._inspect([FRONTEND, WORKER, MOCKER, SHARING_ENGINE])
        fe, wk, mk, eng = insp.get(FRONTEND), insp.get(WORKER), insp.get(MOCKER), insp.get(SHARING_ENGINE)
        fe_state, wk_state, mk_state = self._state(fe), self._state(wk), self._state(mk)
        problems: list[Problem] = []
        comps: list[Component] = []

        models_doc = health_doc = None
        if fe_state == "running":
            health_doc = await self._get_json("/health")
            models_doc = await self._get_json("/v1/models")
        models = [str(m.get("id")) for m in (models_doc or {}).get("data", []) if isinstance(m, dict) and m.get("id")]
        if models:
            self._models = models
        fe_http = health_doc is not None

        fe_health = ((fe or {}).get("State") or {}).get("Health", {}).get("Status")
        comps.append(Component(kind="container", name=FRONTEND, state=fe_state,
                               image=(fe or {}).get("Config", {}).get("Image"),
                               detail=(f"docker health: {fe_health}; " if fe_health else "")
                               + ("HTTP ok" if fe_http else "HTTP unreachable" if fe_state == "running" else "")))
        loading = wk_state == "running" and fe_http and not models
        wk_detail = None
        if wk_state == "running":
            wk_detail = "loading: model not registered with the frontend yet (cold start ~4 min)" if loading else (
                f"serving {', '.join(models)}" if models else None)
        comps.append(Component(kind="container", name=WORKER, state=wk_state,
                               image=(wk or {}).get("Config", {}).get("Image"), detail=wk_detail))
        if mk is not None:
            comps.append(Component(kind="container", name=MOCKER, state=mk_state,
                                   image=mk.get("Config", {}).get("Image"), detail="CPU mocker (gibberish by design)"))

        # problems --------------------------------------------------------------------------
        up_cmd = "cd inference/dynamo && docker compose up -d"
        if fe_state != "running":
            problems.append(Problem(severity="error", code="dynamo.frontend.down",
                                    message=f"frontend container {FRONTEND} is {fe_state}", fix=up_cmd))
        elif not fe_http:
            problems.append(Problem(severity="error", code="dynamo.frontend.unreachable",
                                    message=f"{FRONTEND} runs but {self.base_url}/health did not answer",
                                    fix=f"docker logs --tail 50 {FRONTEND}"))
        if wk_state == "absent" and mk_state != "running":
            problems.append(Problem(severity="warn" if fe_state == "running" else "info", code="dynamo.worker.absent",
                                    message="no Dynamo worker container: the frontend has nothing to route to",
                                    fix="run the console action `up` (or: cd inference/dynamo && docker compose up -d)"))
        elif wk_state in ("stopped", "failed"):
            problems.append(Problem(severity="error", code="dynamo.worker.stopped",
                                    message=f"worker {WORKER} is {wk_state}",
                                    fix=f"docker logs --tail 80 {WORKER}"))
        if wk_state == "running" and eng is not None and self._state(eng) == "running":
            problems.append(Problem(severity="error", code="dynamo.card.contended",
                                    message=f"{SHARING_ENGINE} and the Dynamo worker are both running on the same "
                                            "Workstation card; one will fail to allocate VRAM",
                                    fix="cd inference/vllm-qwen38 && docker compose down"))
        elif wk_state == "running" and self._state(eng) != "running":
            problems.append(Problem(severity="warn", code="dynamo.holds_card",
                                    message=f"vllm-qwen38 is {self._state(eng)} -> router alias {ROUTER_ALIAS} and "
                                            "the Switchyard capable tier are dark while Dynamo holds the card",
                                    # 2026-09-26: never a bare compose up here. The 27B needs Workstation-card VRAM
                                    # that the default engine (flashnext TP2, both cards) holds; use-model.sh stops it.
                                    fix="run the console action `down`, then: scripts/use-model.sh qwen3.8-27b-1m"))
        if mk_state == "running":
            problems.append(Problem(severity="info", code="dynamo.mocker.running",
                                    message="the CPU mocker is registered as the model; its text is gibberish by design",
                                    fix="run the console action `down` to remove it"))
        if wk_state == "running" and wk is not None:
            cmd = [str(c) for c in ((wk.get("Config") or {}).get("Cmd") or [])]
            if _STRUCTURAL not in cmd:
                problems.append(Problem(severity="warn", code="dynamo.structural_tag.off",
                                        message="worker runs without --dyn-enable-structural-tag: tool_choice=\"required\" "
                                                "returns no tool call on this model",
                                        fix="Params: set enable_structural_tag=true and apply"))
            if "--tool-call-parser" in cmd or "--reasoning-parser" in cmd:
                problems.append(Problem(severity="warn", code="dynamo.parser.dialect",
                                        message="worker was started with vLLM's --tool-call-parser/--reasoning-parser; "
                                                "Dynamo parses in the Rust frontend via --dyn-* flags, so these do nothing useful",
                                        fix="Params: set tool_call_parser / reasoning_parser (renders --dyn-*)"))
        if fe_state == "running" and fe is not None:
            fcmd = [str(c) for c in ((fe.get("Config") or {}).get("Cmd") or [])]
            if "--router-mode" in fcmd and fcmd[fcmd.index("--router-mode") + 1:][:1] == ["kv"]:
                problems.append(Problem(severity="warn", code="dynamo.router.kv",
                                        message="frontend runs --router-mode kv with a single worker and no NATS",
                                        fix="Params: router_mode=round-robin and apply"))

        # verdict ---------------------------------------------------------------------------
        if fe_state != "running" or not fe_http:
            health = "down" if fe_state != "running" else "degraded"
            summary = "down: frontend not running" if fe_state != "running" else "frontend unreachable"
        elif wk_state != "running" and mk_state != "running":
            health, summary = "degraded", "frontend up, no worker (nothing to serve)"
        elif loading or not models:
            health, summary = "degraded", "loading: worker up, model not yet registered (cold start ~4 min)"
        else:
            health, summary = "up", f"serving {models[0]}" + (f" (+{len(models) - 1})" if len(models) > 1 else "")

        image = (fe or wk or {}).get("Config", {}).get("Image", "") if (fe or wk) else ""
        version = image.rsplit(":", 1)[-1] if ":" in image.rsplit("/", 1)[-1] else None
        endpoints = [
            Endpoint(name="frontend api", url=f"{self.base_url}/v1", role="api", auth="none"),
            Endpoint(name="frontend health", url=f"{self.base_url}/health", role="health", auth="none"),
            Endpoint(name="frontend metrics", url=f"{self.base_url}/metrics", role="metrics", auth="none"),
            Endpoint(name="worker metrics (ai-lab bus, read via docker exec)",
                     url=f"http://dynamo-worker:{WORKER_METRICS_PORT}/metrics", role="metrics", auth="none"),
        ]
        return SystemStatus(system="dynamo", health=health, version=version, summary=summary,
                            endpoints=endpoints, components=comps, problems=problems, checked_at=now)

    async def metrics(self) -> MetricsSnapshot:
        ts = time.time()
        try:
            r = await self.ctx.http.get(self.base_url + "/metrics", timeout=5.0)
            r.raise_for_status()
            fe = parse_prom(r.text)
        except (httpx.HTTPError, ValueError) as e:
            return MetricsSnapshot(system="dynamo", ts=ts, available=False,
                                   error=f"frontend /metrics unreachable: {type(e).__name__}")
        pts: list[MetricPoint] = [MetricPoint(key="dyn.up", value=1.0, unit="1")]

        def add(key: str, val: float | None, unit: str = "1", **labels: str) -> None:
            if val is not None:
                pts.append(MetricPoint(key=key, value=float(val), unit=unit, labels=dict(labels)))

        ready = [v for n, _, v in fe if n == "dynamo_frontend_model_ready"]
        add("dyn.models", sum(1 for v in ready if v >= 1) if ready else None)
        by_status: dict[str, float] = {}
        for n, lb, v in fe:
            if n == "dynamo_frontend_requests_total":
                by_status[lb.get("status", "unknown")] = by_status.get(lb.get("status", "unknown"), 0.0) + v
        for st, v in sorted(by_status.items()):
            add("dyn.requests_total", v, "1", status=st)
        add("dyn.in_flight", _sum(fe, "dynamo_frontend_inflight_requests"))
        for qn, q in (("p50", 0.5), ("p95", 0.95)):
            add("dyn.ttft_ms", _time_ms(histogram_quantile(fe, "dynamo_frontend_time_to_first_token_seconds", q)), "ms", q=qn)
            add("dyn.itl_ms", _time_ms(histogram_quantile(fe, "dynamo_frontend_inter_token_latency_seconds", q)), "ms", q=qn)
        add("dyn.tokens", _sum(fe, "dynamo_frontend_input_sequence_tokens_sum"), "tok", kind="prompt")
        add("dyn.tokens", _sum(fe, "dynamo_frontend_output_tokens_total"), "tok", kind="completion")
        add("dyn.tokens", _sum(fe, "dynamo_frontend_cached_tokens_sum"), "tok", kind="cached")

        insp = await self._inspect([WORKER])
        wk = insp.get(WORKER)
        worker_up = 0.0
        if wk is not None and self._state(wk) == "running" and self._project_ok(wk, SVC_WORKER):
            health, wm = await self._worker_scrape()
            if health is not None and str(health.get("status")) == "ready":
                worker_up = 1.0
            add("dyn.kv_cache_usage", _sum(wm, "vllm:kv_cache_usage_perc"), "ratio")
            add("dyn.requests_running", _sum(wm, "vllm:num_requests_running"))
            add("dyn.requests_waiting", _sum(wm, "vllm:num_requests_waiting"))
            drafted, accepted = _sum(wm, "vllm:spec_decode_num_draft_tokens_total"), _sum(wm, "vllm:spec_decode_num_accepted_tokens_total")
            if drafted and accepted is not None:
                add("dyn.spec_accept_ratio", accepted / drafted, "ratio")
            pq, ph = _sum(wm, "vllm:prefix_cache_queries_total"), _sum(wm, "vllm:prefix_cache_hits_total")
            if pq and ph is not None:
                add("dyn.prefix_cache_hit_ratio", ph / pq, "ratio")
        add("dyn.worker_up", worker_up)
        return MetricsSnapshot(system="dynamo", ts=ts, available=True, points=pts)

    async def _logs(self, name: str, component: str, *, tail: int | None = None, since: float | None = None) -> list[SystemEvent]:
        d = (await self._inspect([name])).get(name)
        if d is None or not (self._project_ok(d, SVC_FRONTEND) or self._project_ok(d, SVC_WORKER)
                             or self._project_ok(d, SVC_MOCKER)):
            return []
        argv = ["docker", "logs", "--timestamps"]
        argv += ["--tail", str(max(1, min(tail, 1000)))] if tail is not None else []
        argv += ["--since", f"{since:.3f}"] if since is not None else []
        res = await self.ctx.runner.run([*argv, name], timeout=20.0, max_bytes=512_000)
        evs = [e for line in (res.stdout + "\n" + res.stderr).splitlines() if (e := parse_log_line(line, component))]
        return evs

    async def recent_events(self, limit: int = 100) -> list[SystemEvent]:
        limit = max(1, min(limit, 500))
        evs: list[SystemEvent] = []
        for name, comp in ((FRONTEND, "frontend"), (WORKER, "worker"), (MOCKER, "mocker")):
            evs += await self._logs(name, comp, tail=min(1000, limit * 3))
        evs.sort(key=lambda e: e.ts)
        return evs[-limit:]

    async def follow_events(self, since_ts: float | None = None) -> AsyncIterator[SystemEvent]:
        cursor = since_ts if since_ts is not None else time.time()
        seen: dict[tuple[float, str], None] = {}
        while True:
            batch: list[SystemEvent] = []
            for name, comp in ((FRONTEND, "frontend"), (WORKER, "worker"), (MOCKER, "mocker")):
                batch += await self._logs(name, comp, tail=200, since=cursor)
            batch.sort(key=lambda e: e.ts)
            for ev in batch:
                key = (ev.ts, ev.summary)
                if key in seen or ev.ts < cursor:
                    continue
                seen[key] = None
                while len(seen) > 2000:
                    seen.pop(next(iter(seen)))
                cursor = max(cursor, ev.ts)
                yield ev
            await asyncio.sleep(self.follow_interval)

    # ── configure: files ────────────────────────────────────────────────────────────────────
    def _display(self, p: Path | None) -> str:
        if p is None:
            return "inference/dynamo/docker-compose.yml"
        s = str(p)
        home = str(Path.home())
        return "~" + s[len(home):] if s.startswith(home + "/") else s

    def config_files(self) -> list[ConfigFileSpec]:
        return [
            ConfigFileSpec(
                id="compose", label="Dynamo compose (repo)", format="yaml", editable=False,
                description="Frontend + worker (+ CPU mocker profile) for NVIDIA Dynamo 1.5.0. Source of truth in git.",
                display_path="inference/dynamo/docker-compose.yml",
                why_readonly="The console changes Dynamo tunables through a generated compose override in its own "
                             "state dir (ADR 8) and never edits the reviewed repo compose; edit it via git.",
                validators=["docker compose config"], apply_effect="n/a (read-only)"),
            ConfigFileSpec(
                id="override", label="Dynamo compose override (console-generated)", format="yaml", editable=True,
                description="Restates the worker (and optionally frontend) `command` with the console-managed "
                            "tunables. Delete it to fall back to the repo compose.",
                display_path=self._display(self._override_path()),
                validators=["allowlist (services/flags/env)", "docker compose config -q"], authoritative=True,
                apply_effect="recreates dynamo-worker (~4 min cold start); router alias qwen3.8-27b-dynamo and the "
                             "Dynamo playground are unavailable meanwhile"),
        ]

    def config_path(self, file_id: str) -> Path:
        if file_id == "compose":
            p = self._compose_path()
            if p is None:
                raise KeyError("compose (ctx.paths['dynamo_compose'] not configured)")
            return p
        if file_id == "override":
            return self._override_path()
        raise KeyError(file_id)

    async def read_config(self, file_id: str) -> ConfigFile:
        spec = next(s for s in self.config_files() if s.id == file_id)
        p = self.config_path(file_id)
        if p.is_symlink():
            raise PermissionError(f"{file_id}: refusing to read through a symlink")
        if not p.exists():
            if file_id == "override":
                return ConfigFile(spec=spec, content="", sha256=hashlib.sha256(b"").hexdigest(), mtime=0.0)
            raise FileNotFoundError(str(p))
        raw = p.read_bytes()[: _MAX_CONFIG_BYTES + 1]
        if len(raw) > _MAX_CONFIG_BYTES:
            raise ValueError(f"{file_id}: larger than {_MAX_CONFIG_BYTES} bytes")
        return ConfigFile(spec=spec, content=raw.decode("utf-8", "replace"), sha256=hashlib.sha256(raw).hexdigest(),
                          mtime=p.stat().st_mtime)

    def _base_facts(self) -> dict[str, tuple[list[str], set[str]]]:
        """Per service: (base command tokens, base environment keys). Empty when the base is unreadable, which
        makes every `$`, `--model` and env key in a candidate fail closed."""
        base = self._compose_path()
        facts: dict[str, tuple[list[str], set[str]]] = {}
        try:
            doc: Any = yaml.safe_load(base.read_text()) if base is not None else None
            for svc in (SVC_WORKER, SVC_FRONTEND):
                body = doc["services"][svc]
                cmd = body.get("command")
                toks = shlex.split(cmd) if isinstance(cmd, str) else [str(t) for t in cmd or []]
                env = body.get("environment") or {}
                keys = set(env) if isinstance(env, dict) else {str(e).split("=", 1)[0] for e in env}
                facts[svc] = (toks, {str(k) for k in keys})
        except (OSError, yaml.YAMLError, KeyError, TypeError, AttributeError, ValueError):
            return {}
        return facts

    def _check_override_doc(self, content: str) -> list[Diagnostic]:
        d: list[Diagnostic] = []

        def err(msg: str) -> None:
            d.append(Diagnostic(severity="error", message=msg))

        if not content.strip():
            return [Diagnostic(severity="info", message="empty override: the repo compose applies unchanged")]
        try:
            doc = yaml.safe_load(content)
        except yaml.YAMLError as e:
            line = getattr(getattr(e, "problem_mark", None), "line", None)
            return [Diagnostic(severity="error", message=f"YAML: {getattr(e, 'problem', e)}",
                               line=None if line is None else int(line) + 1)]
        if not isinstance(doc, dict) or set(doc) - {"services"} or not isinstance(doc.get("services"), dict):
            return [Diagnostic(severity="error", message="only a top-level `services:` mapping is allowed")]
        base = self._base_facts()
        secret = re.compile(r"(?i)(\bsk-[A-Za-z0-9_-]{8,}|bearer\s+\S{6,}|\bhf_[A-Za-z0-9]{10,})")
        for svc, body in doc["services"].items():
            if svc not in (SVC_WORKER, SVC_FRONTEND):
                err(f"service {svc!r} is not allowed (only {SVC_WORKER}, {SVC_FRONTEND})")
                continue
            if not isinstance(body, dict) or set(body) - {"command", "environment"}:
                err(f"{svc}: only `command` and `environment` may be overridden")
                continue
            cmd = body.get("command")
            if cmd is not None:
                if not (isinstance(cmd, list) and all(isinstance(t, str) for t in cmd)):
                    err(f"{svc}.command must be a list of strings (a folded string is re-split by compose)")
                    continue
                want = ["python3", "-m", "dynamo.vllm" if svc == SVC_WORKER else "dynamo.frontend"]
                if cmd[:3] != want:
                    err(f"{svc}.command must start with {' '.join(want)}")
                    continue
                allowed = _WORKER_FLAGS if svc == SVC_WORKER else _FRONTEND_FLAGS
                b_tokens, _b_env = base.get(svc, ([], set()))
                b_dollar = {t for t in b_tokens if "$" in t}
                b_flags, _ = _parse_flags(b_tokens[3:])
                c_flags, stray = _parse_flags(cmd[3:])
                for tok in stray:
                    err(f"{svc}: positional token {tok!r} is not allowed (every value must follow a flag)")
                seen: set[str] = set()
                for flag, vals, raw in c_flags:
                    if raw not in allowed and flag not in {_norm_flag(a) for a in allowed}:
                        err(f"{svc}: flag {raw} is not on the console allowlist")
                    key = _dup_key(flag)
                    if key in seen:
                        err(f"{svc}: flag {flag} appears more than once (argparse takes the LAST occurrence, "
                            "so a duplicate can override a pinned value); repeat nothing")
                    seen.add(key)
                    base_vals = next((bv for bf, bv, _r in b_flags if bf == flag), None)
                    if flag == "--speculative-config":
                        if not _spec_ok(vals):
                            err(f"{svc}: --speculative-config must be exactly "
                                '{"method":"mtp","num_speculative_tokens":<int 1..8>}')
                        continue
                    if flag in ("--model", "--served-model-name"):
                        if base_vals is None or vals != base_vals:
                            err(f"{svc}: {flag} must equal the base compose's value (it is not tunable)")
                        continue
                    for v in vals:
                        if (v.startswith(("{", "[")) or "/" in v or "\\" in v) and "$" not in v and vals != base_vals:
                            err(f"{svc}: {flag} value {v!r} is a path/JSON and must equal the base compose's value")
                for tok in cmd[3:]:
                    if "$" in tok and tok not in b_dollar:
                        err(f"{svc}: `$` in {tok!r} is refused (compose would interpolate host env/.env into argv); "
                            "only `${VAR:-x}` tokens byte-identical to the base compose may stay")
                    if secret.search(tok):
                        err(f"{svc}: a value looks like a secret; secrets never go in argv")
                if svc == SVC_WORKER and _STRUCTURAL not in cmd:
                    d.append(Diagnostic(severity="warn", message="--dyn-enable-structural-tag is absent: "
                                        "tool_choice=\"required\" will not produce tool calls"))
                if svc == SVC_FRONTEND and "--router-mode" in cmd and cmd[cmd.index("--router-mode") + 1:][:1] == ["kv"]:
                    d.append(Diagnostic(severity="warn", message="router-mode kv needs >1 worker and NATS"))
            env = body.get("environment")
            if env is not None:
                if not isinstance(env, dict):
                    err(f"{svc}.environment must be a mapping")
                else:
                    b_env = base.get(svc, ([], set()))[1]
                    for k, val in env.items():
                        if k not in b_env:
                            err(f"{svc}: environment {k} is not defined in the base compose for this service")
                        elif "$" in str(val):
                            err(f"{svc}: environment {k} contains `$` (interpolation of host env/.env is refused)")
                        elif secret.search(str(val)):
                            err(f"{svc}: environment {k} looks like a secret")
        return d

    async def validate_config(self, file_id: str, content: str) -> ValidationResult:
        if file_id != "override":
            raise KeyError(file_id)
        diags = self._check_override_doc(content)
        ok = not any(x.severity == "error" for x in diags)
        note: str | None = None
        authoritative = False
        base = self._compose_path()
        if ok and content.strip() and base is not None:
            tmpdir = self.ctx.state_dir / "dynamo" / "tmp"
            tmpdir.mkdir(parents=True, exist_ok=True, mode=0o700)
            tmp = tmpdir / f"candidate-{os.getpid()}-{time.time_ns()}.yml"
            fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            try:
                with os.fdopen(fd, "w") as fh:
                    fh.write(content)
                res = await self.ctx.runner.run(
                    ["docker", "compose", "--project-directory", str(base.parent), "-f", str(base), "-f", str(tmp),
                     "config", "-q"], timeout=30.0)
            finally:
                with contextlib.suppress(OSError):
                    tmp.unlink()
            if res.returncode == 0:
                authoritative = True
            else:
                ok = False
                diags.append(Diagnostic(severity="error",
                                        message="docker compose config: " + redact((res.stderr or res.stdout).strip())[:400]))
        elif ok and content.strip():
            note = "docker compose config was not run (repo compose path not configured); allowlist checks only"
        return ValidationResult(ok=ok, validator="allowlist + docker compose config -q", authoritative=authoritative,
                                diagnostics=diags, note=note)

    async def post_apply(self, file_id: str, *, restart: bool) -> ApplyResult:
        if file_id != "override":
            return ApplyResult(ok=False, detail=f"{file_id} is read-only")
        if not restart:
            return ApplyResult(ok=True, restarted=False, post_check="skipped",
                               detail="override saved; takes effect on the next up/restart")
        return await self._recreate_worker_and_check("override written")

    # ── configure: params ───────────────────────────────────────────────────────────────────
    def param_catalog(self) -> list[ParamSpec]:
        return _catalog()

    def current_values(self) -> dict[str, Any]:
        """Values saved in the override header (empty when no override). Extra to the port; UIs may use it."""
        try:
            return parse_values_header(self._override_path().read_text())
        except OSError:
            return {}

    def param_warnings(self, values: dict[str, object]) -> list[str]:
        return param_warnings(validate_values(values))

    def _base_commands(self) -> tuple[list[str], list[str]]:
        base = self._compose_path()
        if base is None:
            raise ParamError("repo compose path is not configured (ctx.paths['dynamo_compose'])")
        try:
            return _load_base_commands(base.read_text())
        except (OSError, yaml.YAMLError) as e:
            raise ParamError(f"cannot read the repo compose: {type(e).__name__}") from e

    async def preview_params(self, values: dict[str, object]) -> str:
        wcmd, fcmd = self._base_commands()
        text, _ = render_override(values, wcmd, fcmd)
        return text

    def _write_override(self, text: str) -> None:
        d = self._override_path().parent
        d.mkdir(parents=True, exist_ok=True, mode=0o700)
        tmp = d / f".override.{os.getpid()}.tmp"
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w") as fh:
            fh.write(text)
        os.chmod(tmp, 0o600)
        os.replace(tmp, self._override_path())

    def _restore_override(self, previous: str | None) -> None:
        if previous is None:
            with contextlib.suppress(OSError):
                self._override_path().unlink()
        else:
            self._write_override(previous)

    async def apply_params(self, values: dict[str, object]) -> ApplyResult:
        try:
            text = await self.preview_params(values)
        except ParamError as e:
            return ApplyResult(ok=False, detail=str(e))
        try:
            previous: str | None = self._override_path().read_text()
        except OSError:
            previous = None
        self._write_override(text)
        try:
            chk = await self.ctx.runner.run(self._compose_argv("config", "-q"), timeout=30.0)
        except FileNotFoundError as e:
            self._restore_override(previous)
            return ApplyResult(ok=False, detail=str(e))
        if chk.returncode != 0:
            self._restore_override(previous)
            return ApplyResult(ok=False, detail="docker compose config rejected the override: "
                               + redact((chk.stderr or chk.stdout).strip())[:400])
        sha = hashlib.sha256(text.encode()).hexdigest()
        insp = await self._inspect([WORKER])
        if WORKER not in insp:
            return ApplyResult(ok=True, new_sha256=sha, restarted=False, post_check="skipped",
                               detail="override written (0600); no worker container exists, so nothing was recreated. "
                                      "It applies on the next `up`.")
        res = await self._recreate_worker_and_check("params applied")
        if res.post_check == "unhealthy":
            self._restore_override(previous)
            back = await self._recreate_worker_and_check("rollback")
            return ApplyResult(ok=False, new_sha256=sha, restarted=True, post_check="unhealthy", rolled_back=True,
                               detail=f"new override unhealthy ({res.detail}); previous override restored -> "
                                      f"{back.post_check}")
        return res.model_copy(update={"new_sha256": sha})

    # ── lifecycle helpers ───────────────────────────────────────────────────────────────────
    async def _wait_healthy(self, prev_started: str | None) -> tuple[bool, str]:
        deadline = time.monotonic() + self.health_timeout
        last = "no probe yet"
        while True:
            insp = await self._inspect([WORKER])
            wk = insp.get(WORKER)
            if wk is None or self._state(wk) != "running":
                last = f"worker {self._state(wk)}"
            elif prev_started is not None and (wk.get("State") or {}).get("StartedAt") == prev_started:
                last = "worker not recreated yet"
            else:
                models_doc = await self._get_json("/v1/models")
                models = (models_doc or {}).get("data") or []
                if models:
                    return True, f"serving {models[0].get('id')}"
                last = "worker running, /v1/models empty (loading)"
            if time.monotonic() >= deadline:
                return False, f"timed out after {self.health_timeout:.0f}s: {last}"
            await asyncio.sleep(self.poll_interval)

    async def _recreate_worker_and_check(self, why: str) -> ApplyResult:
        prev = (await self._inspect([WORKER])).get(WORKER)
        prev_started = ((prev or {}).get("State") or {}).get("StartedAt")
        try:
            argv = self._compose_argv("up", "-d", SVC_FRONTEND, SVC_WORKER)
        except FileNotFoundError as e:
            return ApplyResult(ok=False, detail=str(e))
        res = await self.ctx.runner.run(argv, timeout=600.0)
        if res.returncode != 0:
            return ApplyResult(ok=False, restarted=False, post_check="unhealthy",
                               detail=f"{why}; compose up failed: " + redact((res.stderr or res.stdout).strip())[-400:])
        ok, detail = await self._wait_healthy(prev_started)
        return ApplyResult(ok=ok, restarted=True, post_check="healthy" if ok else "unhealthy",
                           detail=f"{why}; {detail}")

    # ── act ─────────────────────────────────────────────────────────────────────────────────
    def actions(self) -> list[ActionSpec]:
        return [
            ActionSpec(id="preflight", label="Preflight (what shares the card)", danger="safe",
                       description="Lists containers pinned to the same GPU UUID as the Dynamo worker. Dynamo up/down "
                                   "touches no other card and never runs scripts/use-model.sh."),
            ActionSpec(id="up", label="Up (frontend + worker)", danger="disruptive",
                       description=f"docker compose up -d for the frontend and the vLLM worker (cold start ~4 min). "
                                   f"Refused while another engine holds the same card unless stop_sharing_engine=true, "
                                   f"which stops exactly {SHARING_ENGINE}. While Dynamo holds the card the router alias "
                                   f"{ROUTER_ALIAS} and the Switchyard capable tier are dark.",
                       params=[ParamSpec(key="stop_sharing_engine", label="Stop the sharing engine first", type="bool",
                                         default=False,
                                         help=f"Stops exactly {SHARING_ENGINE} (docker compose down in its own project, no -v).")]),
            ActionSpec(id="down", label="Down (Dynamo only)", danger="disruptive",
                       description="docker compose down for the Dynamo project (frontend, worker, mocker); volumes are kept. "
                                   "Frees the Workstation card; touches no other card. Does not restart vllm-qwen38."),
            ActionSpec(id="restart", label="Restart (recreate)", danger="disruptive",
                       description="up -d --force-recreate of frontend and worker, picking up the override."),
            ActionSpec(id="up_mock", label="Up mock (CPU, no VRAM)", danger="safe",
                       description="Starts the frontend and the CPU mocker (--profile mock) to test plumbing. Refused "
                                   "while the real worker runs (same model name)."),
        ]

    async def _sharers(self) -> tuple[list[dict[str, Any]], str | None]:
        """Running containers (other than Dynamo's) pinned to the worker's GPU UUID. -> (sharers, error)."""
        uuid: str | None = None
        base = self._compose_path()
        if base is not None:
            with contextlib.suppress(OSError, yaml.YAMLError, AttributeError, IndexError, TypeError):
                doc = yaml.safe_load(base.read_text())
                ids = doc["services"][SVC_WORKER]["deploy"]["resources"]["reservations"]["devices"][0]["device_ids"]
                uuid = str(ids[0])
        if uuid is None:
            wk = (await self._inspect([WORKER])).get(WORKER)
            for req in ((wk or {}).get("HostConfig") or {}).get("DeviceRequests") or []:
                if req.get("DeviceIDs"):
                    uuid = str(req["DeviceIDs"][0])
        self._card_uuid = uuid
        if uuid is None or not _UUID_RE.fullmatch(uuid):
            return [], "cannot determine the worker's GPU UUID from the compose or the container"
        ps = await self.ctx.runner.run(["docker", "ps", "--format", "{{.Names}}"], timeout=15.0)
        names = [n for n in ps.stdout.split() if _NAME_RE.fullmatch(n) and n not in DYNAMO_NAMES]
        if not names:
            return [], None
        found = await self._inspect(names)
        out: list[dict[str, Any]] = []
        for n, d in found.items():
            if self._state(d) != "running":
                continue
            for req in (d.get("HostConfig") or {}).get("DeviceRequests") or []:
                if uuid in (req.get("DeviceIDs") or []) or req.get("Count") == -1:
                    lb = (d.get("Config") or {}).get("Labels") or {}
                    out.append({"name": n, "image": (d.get("Config") or {}).get("Image", ""),
                                "project": lb.get("com.docker.compose.project", ""),
                                "service": lb.get("com.docker.compose.service", ""),
                                "workdir": lb.get("com.docker.compose.project.working_dir", ""),
                                "config": lb.get("com.docker.compose.project.config_files", ""),
                                "all_gpus": req.get("Count") == -1})
                    break
        return sorted(out, key=lambda s: s["name"]), None

    @staticmethod
    def _describe(s: dict[str, Any]) -> str:
        return f"{s['name']} ({s['image']}{', requests ALL gpus' if s['all_gpus'] else ''})"

    async def run_action(self, action_id: str, params: dict[str, object]) -> ActionResult:
        if action_id not in {a.id for a in self.actions()}:
            raise KeyError(action_id)
        t0 = time.monotonic()
        res = await self._dispatch(action_id, params)
        return res.model_copy(update={"duration_ms": int((time.monotonic() - t0) * 1000)})

    async def _dispatch(self, action_id: str, params: dict[str, object]) -> ActionResult:
        if self._compose_path() is None:
            return ActionResult(ok=False, problems=[Problem(severity="error", code="dynamo.compose.unset",
                                message="repo compose path is not configured", fix="set ctx.paths['dynamo_compose']")])
        if action_id == "preflight":
            sharers, err = await self._sharers()
            if err:
                return ActionResult(ok=False, output=err)
            lines = [
                (
                    "Dynamo worker GPU: pinned by UUID in the compose; up/down of Dynamo touches no other card "
                    "and never calls scripts/use-model.sh."
                )
            ]
            if sharers:
                lines.append("Running containers on the same card: " + "; ".join(self._describe(s) for s in sharers))
                lines.append(f"`up` is refused until they stop; stop_sharing_engine=true stops only {SHARING_ENGINE}.")
            else:
                lines.append("No other container is running on the Dynamo card.")
            lines.append(f"While Dynamo holds the card, router alias {ROUTER_ALIAS} and the Switchyard capable tier are dark.")
            probs = [Problem(severity="warn", code="dynamo.card.shared", message=self._describe(s),
                             fix="stop it, or run `up` with stop_sharing_engine=true" if s["name"] == SHARING_ENGINE else
                             f"docker stop {s['name']}") for s in sharers]
            lines += await self._machine_lines(sharers)
            return ActionResult(ok=True, output="\n".join(lines), problems=probs)
        if action_id == "up_mock":
            return await self._up_mock()
        if action_id == "down":
            r = await self.ctx.runner.run(self._compose_argv("down", profile_mock=True), timeout=180.0)
            return ActionResult(ok=r.returncode == 0, output=redact((r.stdout + r.stderr).strip())[-2000:])
        return await self._up_or_restart(action_id, params)

    async def _machine_lines(self, sharers: list[dict[str, Any]]) -> list[str]:
        """Machine-readable tail of the preflight output (the UI highlights these; the prose above is unchanged)."""
        names = {s["name"]: "running" for s in sharers}
        cur = await self._inspect([SHARING_ENGINE, WORKER])
        eng = cur.get(SHARING_ENGINE)
        if eng is not None and SHARING_ENGINE not in names:
            pinned = [
                i for r in (eng.get("HostConfig") or {}).get("DeviceRequests") or [] for i in r.get("DeviceIDs") or []
            ]
            if self._card_uuid in pinned:
                names[SHARING_ENGINE] = "stopped"
        out = [f"shares_card: {n} ({st})" for n, st in sorted(names.items())] or ["shares_card: none"]
        dark = WORKER in cur and self._state(cur[WORKER]) == "running" and names.get(SHARING_ENGINE) != "running"
        out.append(f"router_aliases_dark: {ROUTER_ALIAS}, switchyard-capable-tier" if dark else "router_aliases_dark: none")
        return out

    async def _up_mock(self) -> ActionResult:
        cur = await self._inspect([WORKER])
        if WORKER in cur and self._state(cur[WORKER]) == "running":
            return ActionResult(ok=False, output="refused: the real worker is running (same model name as the mocker)",
                                problems=[Problem(severity="error", code="dynamo.mock.worker_running",
                                                  message="stop the worker first", fix="run the console action `down`")])
        r = await self.ctx.runner.run(self._compose_argv("up", "-d", SVC_FRONTEND, SVC_MOCKER, profile_mock=True),
                                      timeout=300.0)
        return ActionResult(ok=r.returncode == 0, output=redact((r.stdout + r.stderr).strip())[-2000:])

    async def _up_or_restart(self, action_id: str, params: dict[str, object]) -> ActionResult:
        cur = await self._inspect([WORKER, MOCKER])
        worker_running = WORKER in cur and self._state(cur[WORKER]) == "running"
        out: list[str] = []
        if MOCKER in cur and self._state(cur[MOCKER]) == "running":
            return ActionResult(ok=False, output="refused: the CPU mocker is running (same model name)",
                                problems=[Problem(severity="error", code="dynamo.up.mocker_running",
                                                  message="the mocker must not run alongside the real worker",
                                                  fix="run the console action `down`, then `up`")])
        if not worker_running:
            sharers, err = await self._sharers()
            if err:
                return ActionResult(ok=False, output=err, problems=[Problem(
                    severity="error", code="dynamo.preflight.failed", message=err)])
            if sharers:
                want_stop = params.get("stop_sharing_engine") is True
                named = [s for s in sharers if s["name"] == SHARING_ENGINE]
                others = [s for s in sharers if s["name"] != SHARING_ENGINE]
                if not want_stop or not named or others:
                    why = "; ".join(self._describe(s) for s in sharers)
                    hint = "" if want_stop and named else f" Re-run with stop_sharing_engine=true to stop exactly {SHARING_ENGINE}."
                    if others:
                        hint = " Other containers on the card are never stopped by the console: " + ", ".join(
                            s["name"] for s in others) + "."
                    return ActionResult(ok=False, output=f"refused: the Workstation card is in use by {why}.{hint}",
                                        problems=[Problem(severity="error", code="dynamo.up.card_in_use",
                                                          message=f"card in use by {why}",
                                                          fix="run `up` with stop_sharing_engine=true" if not others else
                                                          "docker stop " + others[0]["name"])])
                stopped = await self._stop_sharing_engine(named[0])
                out.append(stopped.output)
                if not stopped.ok:
                    return ActionResult(ok=False, output="\n".join(out), problems=stopped.problems)
        tail = ["up", "-d"] + (["--force-recreate"] if action_id == "restart" else []) + [SVC_FRONTEND, SVC_WORKER]
        prev = cur.get(WORKER)
        prev_started = ((prev or {}).get("State") or {}).get("StartedAt")
        r = await self.ctx.runner.run(self._compose_argv(*tail), timeout=600.0)
        out.append(redact((r.stdout + r.stderr).strip())[-1500:])
        if r.returncode != 0:
            return ActionResult(ok=False, output="\n".join(out), problems=[Problem(
                severity="error", code="dynamo.up.failed", message="docker compose up failed")])
        ok, detail = await self._wait_healthy(prev_started if action_id == "restart" else None)
        out.append(detail)
        return ActionResult(ok=ok, output="\n".join(out), problems=[] if ok else [Problem(
            severity="error", code="dynamo.up.unhealthy", message=detail,
            fix=f"docker logs --tail 80 {WORKER}")])

    async def _stop_sharing_engine(self, s: dict[str, Any]) -> ActionResult:
        """Stops exactly SHARING_ENGINE: name + compose labels verified, `docker compose down` in ITS project, no -v."""
        cfg = str(s["config"]).split(",")[0].strip()
        workdir = str(s["workdir"])
        bad = (s["name"] != SHARING_ENGINE or s["project"] != SHARING_PROJECT or s["service"] != SHARING_SERVICE
               or not cfg.startswith("/") or Path(cfg).name not in _COMPOSE_NAMES
               or str(Path(cfg).parent) != workdir)
        if bad:
            return ActionResult(ok=False, output="refused: container labels do not match the expected engine",
                                problems=[Problem(severity="error", code="dynamo.stop.unverified",
                                                  message=f"{s['name']} failed name/label verification",
                                                  fix=f"stop it by hand: docker stop {s['name']}")])
        r = await self.ctx.runner.run(["docker", "compose", "-f", cfg, "down"], timeout=180.0, cwd=Path(workdir))
        again = (await self._inspect([SHARING_ENGINE])).get(SHARING_ENGINE)
        ok = r.returncode == 0 and self._state(again) != "running"
        return ActionResult(ok=ok, output=f"stopped {SHARING_ENGINE}: " + redact((r.stdout + r.stderr).strip())[-600:],
                            problems=[] if ok else [Problem(severity="error", code="dynamo.stop.failed",
                                                            message=f"{SHARING_ENGINE} is still running")])

    # ── interact ────────────────────────────────────────────────────────────────────────────
    def playground(self) -> PlaygroundSpec | None:
        models = list(self._models)
        if not models:
            try:
                base = self._compose_path()
                if base is not None:
                    wcmd, _ = _load_base_commands(base.read_text())
                    m = _find(split_command(wcmd)[1], "--served-model-name")
                    if m:
                        models = [m[0]]
            except (OSError, yaml.YAMLError, ParamError):
                pass
        if not models:
            models = [DEFAULT_MODEL]
        return PlaygroundSpec(wire="openai_chat", models=models, default_model=models[0],
                              note="Direct to the Dynamo frontend (no router, no Presidio masking, no auth). "
                                   "Model list refreshes on each status() call.")

    def playground_url(self) -> str | None:
        return self.base_url + "/v1/chat/completions"


__all__ = [
    "CommandResult",
    "DynamoAdapter",
    "ParamError",
    "histogram_quantile",
    "parse_log_line",
    "parse_prom",
    "redact",
    "render_override",
    "validate_values",
]
