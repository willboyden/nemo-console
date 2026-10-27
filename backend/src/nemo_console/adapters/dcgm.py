"""DCGM adapter: the dcgm-exporter on this host plus every DCGM host Prometheus scrapes (addendum 9.8).

Facts this adapter is built on (evidence: tests/dcgm/fixtures, captured live 2026-10-24 and redacted; marked
"synthetic" where a fixture was derived by hand):
  * The exporter (`nvcr.io/nvidia/k8s/dcgm-exporter:4.6.1-4.8.4`, container `ai-lab-observability-dcgm-exporter-1`)
    serves `/metrics` on 127.0.0.1:9400. Image tag = `<DCGM lib version>-<exporter version>`; the EXPORTER version is the
    part after the dash (CVE-2026-47483: `/debug/pprof` is served unauthenticated up to 4.8.3; 4.8.4 serves it only
    with `--enable-pprof`). It logs `Starting dcgm-exporter Version=4.8.4` and `Watching for changes in file`.
  * Per-GPU families carry labels `gpu` (index), `modelName`, `UUID`, `pci_bus_id`, `device`, `hostname`,
    `DCGM_FI_DRIVER_VERSION`. Only `gpu` and a shortened `modelName` are used as labels here (never UUID/serial).
    `DCGM_FI_PROF_*` families appear after the first collection cycle (10-20 s). `DCGM_FI_DEV_XID_ERRORS` is the
    VALUE OF THE LAST XID, not a count, and the series is absent while no XID was seen.
  * Prometheus series of the remote hosts carry `job` and `host`; this host's series (job `dcgm`) carry no `host`
    label, so they are named "local".
  * Diagnostics run in a throwaway container from the FULL DCGM image (`nv-hostengine` + `dcgmi diag`); the exporter
    image has no diagnostic plugins. Two different GPU models cannot be diagnosed as one group: one run per index.
    `dcgmi diag -j` prints one JSON document (`DCGM Diagnostic.test_categories[].tests[]`), which contains the GPU
    SERIAL NUMBER (`entity_groups[].entities[].serial_num`): it is dropped before anything is stored or shown.
"""
from __future__ import annotations

import asyncio
import contextlib
import difflib
import json
import math
import re
import shutil
import time
from collections import deque
from collections.abc import AsyncIterator, Callable
from pathlib import Path
from typing import Any

from nemo_console.adapters.base import AdapterContext, CommandResult, SystemAdapter
from nemo_console.config import DCGM_DIAG_IMAGE, DCGM_EXPORTER_CONTAINER, DCGM_PROMETHEUS_JOBS
from nemo_console.domain.models import (
    ActionResult,
    ActionSpec,
    ApplyResult,
    Component,
    ConfigFile,
    ConfigFileSpec,
    Diagnostic,
    Endpoint,
    Health,
    MetricPoint,
    MetricsSnapshot,
    ParamSpec,
    Problem,
    SystemEvent,
    SystemStatus,
    ValidationResult,
)
from nemo_console.services.common import redact, sha256_hex

# A cache stamped NEVER is always stale. time.monotonic() counts from boot, so a 0.0 stamp looked FRESH for the
# first TTL seconds after a reboot (a just-booted CI runner served the empty initial value and skipped the check).
NEVER = float("-inf")
FIXED_EXPORTER = (4, 8, 3)                  # first version without the unauthenticated /debug/pprof (NVD: 4.8.3)
FIXED_TAG = "4.6.1-4.8.4"                   # the tag observability/.env.example pins (digest: see that file)
KEEP_RUNS = 100
EVENT_XID = "dcgm.xid"
EVENT_DIAG = "dcgm.diag"
MAX_CONFIG_BYTES = 256 * 1024
MAX_METRICS_BYTES = 8 * 1024 * 1024
MAX_GPUS = 16
MAX_HOSTS = 32
DIAG_TIMEOUT = {1: 120.0, 2: 600.0}         # level 2 runs minutes on a loaded card (runner cap is 600 s)
PROM_TTL_IMAGE = 60.0
COUNTERS_ID = "counters"
JOB_RE = re.compile(r"[A-Za-z0-9_-]{1,64}")
FIELD_RE = re.compile(r"[A-Za-z0-9_]+")
RUN_ID_RE = re.compile(r"\d{8}T\d{6}-\d{3}-diag[12]-gpu\d{1,2}")
IMAGE_TAG_RE = re.compile(r"dcgm-exporter:(\d+\.\d+\.\d+)-(\d+\.\d+\.\d+)")
SAMPLE_RE = re.compile(r"^([A-Za-z_:][A-Za-z0-9_:]*)(?:\{(.*)\})?\s+(\S+)(?:\s+-?\d+)?\s*$")
LABEL_RE = re.compile(r'([A-Za-z_][A-Za-z0-9_]*)="((?:[^"\\]|\\.)*)"')
UUID_RE = re.compile(r"GPU-[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}")
LONG_DIGITS_RE = re.compile(r"\b\d{9,}\b")

# The in-container commands are CONSTANTS per action; only a validated integer GPU index is substituted.
# read-only root: the diagnostic needs a writable home/cwd (/tmp tmpfs), else it fails "No permission to create a file in
# directory /" (seen live). /run is NOT a tmpfs on purpose: it would hide the nvidia-persistenced socket the NVIDIA
# toolkit mounts there, and DCGM then reports "Persistence mode ... disabled" (seen live). The hostengine pid file
# therefore goes to /tmp (--pid), else the engine does not start and `dcgmi discovery` cannot connect (seen live).
SH_PREFIX = "cd /tmp; nv-hostengine --pid /tmp/nvhe.pid --home-dir /tmp >/dev/null 2>&1; sleep 2; "
DIAG_CMD = {1: SH_PREFIX + "dcgmi diag -r 1 -i {gpu} -j", 2: SH_PREFIX + "dcgmi diag -r 2 -i {gpu} -j"}
DISCOVERY_CMD = SH_PREFIX + "dcgmi discovery -l"

# Field names and types known to the exporter, parsed (commented rows included, they are documentation of valid
# fields) from /etc/dcgm-exporter/default-counters.csv and dcp-metrics-included.csv of the image
# nvcr.io/nvidia/k8s/dcgm-exporter:4.6.1-4.8.4 (86 names, both files agree on every type). g=gauge c=counter
# l=label. The 1.x-compatibility profile (lower-case names) is a different profile and is not accepted here.
KNOWN_FIELDS: dict[str, str] = {
    "DCGM_EXP_CLOCK_EVENTS_COUNT": "g", "DCGM_EXP_CLOCK_EVENTS_TOTAL": "c", "DCGM_EXP_GPU_HEALTH_STATUS": "g",
    "DCGM_EXP_P2P_STATUS": "g", "DCGM_EXP_XID_ERRORS_COUNT": "g", "DCGM_EXP_XID_ERRORS_TOTAL": "c",
    "DCGM_FI_DEV_BOARD_LIMIT_VIOLATION": "c", "DCGM_FI_DEV_BRAND": "l", "DCGM_FI_DEV_CORRECTABLE_REMAPPED_ROWS": "c",
    "DCGM_FI_DEV_DEC_UTIL": "g", "DCGM_FI_DEV_ECC_DBE_AGG_TOTAL": "c", "DCGM_FI_DEV_ECC_DBE_VOL_TOTAL": "c",
    "DCGM_FI_DEV_ECC_INFOROM_VER": "l", "DCGM_FI_DEV_ECC_SBE_AGG_TOTAL": "c", "DCGM_FI_DEV_ECC_SBE_VOL_TOTAL": "c",
    "DCGM_FI_DEV_ENC_UTIL": "g", "DCGM_FI_DEV_ENFORCED_POWER_LIMIT": "g", "DCGM_FI_DEV_FABRIC_HEALTH_MASK": "g",
    "DCGM_FI_DEV_FABRIC_HEALTH_SUMMARY": "g", "DCGM_FI_DEV_FB_FREE": "g", "DCGM_FI_DEV_FB_RESERVED": "g",
    "DCGM_FI_DEV_FB_USED": "g", "DCGM_FI_DEV_GPU_TEMP": "g", "DCGM_FI_DEV_GPU_UTIL": "g",
    "DCGM_FI_DEV_INFOROM_IMAGE_VER": "l", "DCGM_FI_DEV_LOW_UTIL_VIOLATION": "c", "DCGM_FI_DEV_MEMORY_TEMP": "g",
    "DCGM_FI_DEV_MEM_CLOCK": "g", "DCGM_FI_DEV_MEM_COPY_UTIL": "g", "DCGM_FI_DEV_NVLINK_BANDWIDTH_L0": "g",
    "DCGM_FI_DEV_NVLINK_BANDWIDTH_TOTAL": "g", "DCGM_FI_DEV_NVLINK_CRC_DATA_ERROR_TOTAL": "c",
    "DCGM_FI_DEV_NVLINK_CRC_ERROR_TOTAL": "c", "DCGM_FI_DEV_NVLINK_CRC_FLIT_ERROR_TOTAL": "c",
    "DCGM_FI_DEV_NVLINK_RECOVERY_ERROR_TOTAL": "c", "DCGM_FI_DEV_NVLINK_RECOVERY_EVENT_TOTAL": "c",
    "DCGM_FI_DEV_NVLINK_RECOVERY_FAILED_TOTAL": "c", "DCGM_FI_DEV_NVLINK_RECOVERY_SUCCESSFUL_TOTAL": "c",
    "DCGM_FI_DEV_NVLINK_RECOVERY_TOTAL": "c", "DCGM_FI_DEV_NVLINK_REPLAY_ERROR_TOTAL": "c",
    "DCGM_FI_DEV_NVLINK_REPLAY_TOTAL": "c", "DCGM_FI_DEV_OEM_INFOROM_VER": "l",
    "DCGM_FI_DEV_PCIE_REPLAY_COUNTER": "c", "DCGM_FI_DEV_POWER_INFOROM_VER": "l",
    "DCGM_FI_DEV_POWER_MGMT_LIMIT": "g", "DCGM_FI_DEV_POWER_USAGE": "g", "DCGM_FI_DEV_POWER_VIOLATION": "c",
    "DCGM_FI_DEV_RELIABILITY_VIOLATION": "c", "DCGM_FI_DEV_RETIRED_DBE": "c", "DCGM_FI_DEV_RETIRED_PENDING": "g",
    "DCGM_FI_DEV_RETIRED_SBE": "c", "DCGM_FI_DEV_ROW_REMAP_FAILURE": "g", "DCGM_FI_DEV_SERIAL": "l",
    "DCGM_FI_DEV_SM_CLOCK": "g", "DCGM_FI_DEV_SYNC_BOOST_VIOLATION": "c", "DCGM_FI_DEV_THERMAL_VIOLATION": "c",
    "DCGM_FI_DEV_TOTAL_ENERGY_CONSUMPTION": "c", "DCGM_FI_DEV_UNCORRECTABLE_REMAPPED_ROWS": "c",
    "DCGM_FI_DEV_VBIOS_VERSION": "l", "DCGM_FI_DEV_VGPU_LICENSE_STATUS": "g", "DCGM_FI_DEV_XID_ERRORS": "g",
    "DCGM_FI_DRIVER_VERSION": "l", "DCGM_FI_NVML_VERSION": "l", "DCGM_FI_PROF_DFMA_CYCLES_ACTIVE_TOTAL": "c",
    "DCGM_FI_PROF_DMMA_CYCLES_ACTIVE_TOTAL": "c", "DCGM_FI_PROF_DRAM_ACTIVE": "g",
    "DCGM_FI_PROF_FP16_CYCLES_ACTIVE_TOTAL": "c", "DCGM_FI_PROF_FP32_CYCLES_ACTIVE_TOTAL": "c",
    "DCGM_FI_PROF_FP64_CYCLES_ACTIVE_TOTAL": "c", "DCGM_FI_PROF_GR_ENGINE_ACTIVE": "g",
    "DCGM_FI_PROF_HMMA_CYCLES_ACTIVE_TOTAL": "c", "DCGM_FI_PROF_IMMA_CYCLES_ACTIVE_TOTAL": "c",
    "DCGM_FI_PROF_INT_CYCLES_ACTIVE_TOTAL": "c", "DCGM_FI_PROF_MMA_CYCLES_ACTIVE_TOTAL": "c",
    "DCGM_FI_PROF_PCIE_RX_BYTES": "g", "DCGM_FI_PROF_PCIE_RX_BYTES_TOTAL": "c", "DCGM_FI_PROF_PCIE_TX_BYTES": "g",
    "DCGM_FI_PROF_PCIE_TX_BYTES_TOTAL": "c", "DCGM_FI_PROF_PIPE_FP16_ACTIVE": "g",
    "DCGM_FI_PROF_PIPE_FP32_ACTIVE": "g", "DCGM_FI_PROF_PIPE_FP64_ACTIVE": "g",
    "DCGM_FI_PROF_PIPE_TENSOR_ACTIVE": "g", "DCGM_FI_PROF_SM_ACTIVE": "g",
    "DCGM_FI_PROF_SM_CYCLES_ACTIVE_TOTAL": "c", "DCGM_FI_PROF_SM_CYCLES_ELAPSED_TOTAL": "c",
    "DCGM_FI_PROF_SM_OCCUPANCY": "g",
}
TYPE_LETTER = {"gauge": "g", "counter": "c", "label": "l"}

# (family, key, unit, scale). Prefix `dcgm.`; labels gpu + model. Utilisation percentages become ratios, MiB become B.
GPU_METRICS: tuple[tuple[str, str, str, float], ...] = (
    ("DCGM_FI_DEV_GPU_TEMP", "dcgm.gpu_temp_c", "C", 1.0),
    ("DCGM_FI_DEV_POWER_USAGE", "dcgm.power_w", "W", 1.0),
    ("DCGM_FI_DEV_GPU_UTIL", "dcgm.gpu_util", "ratio", 0.01),
    ("DCGM_FI_DEV_MEM_COPY_UTIL", "dcgm.mem_util", "ratio", 0.01),
    ("DCGM_FI_DEV_FB_USED", "dcgm.fb_used_b", "B", 1048576.0),
    ("DCGM_FI_DEV_FB_FREE", "dcgm.fb_free_b", "B", 1048576.0),
    ("DCGM_FI_DEV_SM_CLOCK", "dcgm.sm_clock_mhz", "MHz", 1.0),
    ("DCGM_FI_DEV_XID_ERRORS", "dcgm.xid_last", "1", 1.0),
    ("DCGM_EXP_XID_ERRORS_TOTAL", "dcgm.xid_total", "1", 1.0),
    ("DCGM_FI_DEV_PCIE_REPLAY_COUNTER", "dcgm.pcie_replay_total", "1", 1.0),
    ("DCGM_FI_PROF_GR_ENGINE_ACTIVE", "dcgm.gr_active", "ratio", 1.0),
    ("DCGM_FI_PROF_PIPE_TENSOR_ACTIVE", "dcgm.tensor_active", "ratio", 1.0),
    ("DCGM_FI_PROF_DRAM_ACTIVE", "dcgm.dram_active", "ratio", 1.0),
    ("DCGM_FI_PROF_PCIE_TX_BYTES", "dcgm.pcie_tx_bps", "B/s", 1.0),
    ("DCGM_FI_PROF_PCIE_RX_BYTES", "dcgm.pcie_rx_bps", "B/s", 1.0),
)

Samples = dict[str, list[tuple[dict[str, str], float]]]
Done = Callable[..., ActionResult]          # run_action's `done(ok, output, problems=None)`


def parse_exposition(text: str) -> Samples:
    """Prometheus text format -> {family: [(labels, value)]}. Comments, NaN/Inf and malformed lines are skipped."""
    out: Samples = {}
    for line in text.splitlines():
        if not line or line.startswith("#"):
            continue
        m = SAMPLE_RE.match(line)
        if m is None:
            continue
        try:
            value = float(m.group(3))
        except ValueError:
            continue
        if not math.isfinite(value):
            continue
        labels = {k: v.replace('\\"', '"').replace("\\\\", "\\") for k, v in LABEL_RE.findall(m.group(2) or "")}
        out.setdefault(m.group(1), []).append((labels, value))
    return out


def exporter_version(image: str | None) -> str | None:
    """`...dcgm-exporter:4.6.1-4.8.4[@sha256:..]` -> "4.8.4" (the exporter part); None when the tag has no such shape."""
    m = IMAGE_TAG_RE.search(image or "")
    return m.group(2) if m else None


def version_tuple(v: str) -> tuple[int, ...]:
    return tuple(int(p) for p in v.split(".") if p.isdigit())


def short_model(name: str) -> str:
    """Display name for text (status, diag output): 'RTX PRO 6000 Blackwell Max-Q Workstation'."""
    return re.sub(r"[^A-Za-z0-9._ -]", "", name.removeprefix("NVIDIA ").removesuffix(" Edition"))[:40]


def _clean_label(v: str) -> str:
    """Label values must fit metrics_keys.KEY_RE's label charset, or the series can never be charted."""
    return re.sub(r"-{2,}", "-", re.sub(r"[^A-Za-z0-9._:/-]", "-", v.strip()))[:40].strip("-")


def parse_counters(content: str) -> tuple[list[tuple[int, str, str]], list[Diagnostic]]:
    """Every non-comment, non-blank line must be `FIELD, type, help`. Returns ([(line, FIELD, type)], diagnostics)."""
    rows: list[tuple[int, str, str]] = []
    diags: list[Diagnostic] = []
    seen: dict[str, int] = {}
    for no, raw in enumerate(content.splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = [p.strip() for p in line.split(",", 2)]
        if len(parts) < 3:
            diags.append(Diagnostic(severity="error", line=no,
                                    message="expected `FIELD, type, help text` (three comma-separated columns)"))
            continue
        field, mtype, help_text = parts
        mtype = mtype.lower()
        if not FIELD_RE.fullmatch(field):
            diags.append(Diagnostic(severity="error", line=no, message=f"{field[:60]!r} is not a field name"))
            continue
        if mtype not in TYPE_LETTER:
            diags.append(Diagnostic(severity="error", line=no,
                                    message=f"type {mtype[:20]!r} must be one of gauge, counter, label"))
            continue
        if not help_text:
            diags.append(Diagnostic(severity="warn", line=no, message=f"{field} has an empty help text"))
        if field in seen:
            diags.append(Diagnostic(severity="error", line=no,
                                    message=f"{field} is already configured on line {seen[field]}"))
            continue
        seen[field] = no
        known = KNOWN_FIELDS.get(field)
        if known is None:
            close = difflib.get_close_matches(field, list(KNOWN_FIELDS), n=1, cutoff=0.85)
            if field.startswith("dcgm_"):
                diags.append(Diagnostic(severity="error", line=no, message=(
                    f"{field} is a 1.x-compatibility name; this exporter profile uses DCGM_FI_* / DCGM_EXP_* names")))
            elif close:
                diags.append(Diagnostic(severity="error", line=no,
                                        message=f"unknown field {field}; did you mean {close[0]}?"))
            else:
                diags.append(Diagnostic(severity="warn", line=no, message=(
                    f"{field} is not in the field lists shipped in the exporter image; if it is a newer DCGM "
                    "field this is fine, otherwise the exporter will refuse it")))
        elif (known == "l") != (mtype == "label"):
            diags.append(Diagnostic(severity="warn", line=no, message=(
                f"{field} is a {'label' if known == 'l' else 'metric'} field in the shipped lists but is "
                f"configured as {mtype}")))
        if field == "DCGM_FI_DEV_SERIAL":
            diags.append(Diagnostic(severity="warn", line=no, message=(
                "the GPU serial number would be attached to every series and stored by Prometheus")))
        rows.append((no, field, mtype))
    if not any(t != "label" for _, _, t in rows):
        diags.append(Diagnostic(severity="error", message=(
            "no active gauge/counter line: the exporter would serve no GPU metric (custom files REPLACE the "
            "default set)")))
    return rows, diags


def _dict(v: object) -> dict[str, Any]:
    return v if isinstance(v, dict) else {}


def _list(v: object) -> list[Any]:
    return v if isinstance(v, list) else []


def _strip_serials(v: Any) -> Any:
    """Recursively drop serial-number keys and mask long digit runs / GPU UUIDs in strings (stored and shown data)."""
    if isinstance(v, dict):
        return {str(k): _strip_serials(x) for k, x in v.items() if "serial" not in str(k).lower()}
    if isinstance(v, list):
        return [_strip_serials(x) for x in v]
    if isinstance(v, str):
        return LONG_DIGITS_RE.sub("[id]", UUID_RE.sub(lambda m: m.group(0)[:12] + "...", v))
    return v


def summarize_diag(doc: dict[str, Any]) -> tuple[str, list[dict[str, str]], list[str]]:
    """(verdict, [{name, status}], warning texts) from `dcgmi diag -j`. verdict: pass | warn | fail | error."""
    tests: list[dict[str, str]] = []
    notes: list[str] = []
    top = _dict(doc.get("DCGM Diagnostic"))
    for cat in _list(top.get("test_categories")):
        for t in _list(_dict(cat).get("tests")):
            td = _dict(t)
            status = _dict(td.get("test_summary")).get("status")
            results = [_dict(r) for r in _list(td.get("results"))]
            for w in _list(_dict(td.get("test_summary")).get("warnings")):
                text = _dict(w).get("warning")
                if isinstance(text, str):
                    notes.append(text[:300])
            if not isinstance(status, str):
                statuses = [str(r.get("status")) for r in results if r.get("status")]
                status = next((s for s in statuses if s.lower().startswith("fail")), None) \
                    or next((s for s in statuses if s.lower().startswith("warn")), None) \
                    or (statuses[0] if statuses else "Unknown")
            tests.append({"name": str(td.get("name", "?"))[:60], "status": status[:20]})
            for r in results:
                for key in ("warnings", "info"):
                    for w in _list(r.get(key)):
                        text = _dict(w).get("warning") or _dict(w).get("info") or (w if isinstance(w, str) else None)
                        if isinstance(text, str) and key == "warnings":
                            notes.append(text[:300])
    low = [t["status"].lower() for t in tests]
    if not tests:
        return "error", tests, notes
    if any(s.startswith("fail") for s in low):
        return "fail", tests, notes
    if any(s.startswith("warn") for s in low) or notes:
        return "warn", tests, notes
    return "pass", tests, notes


class DcgmAdapter(SystemAdapter):
    id = "dcgm"
    label = "DCGM"

    # Defaults equal the Settings defaults; container.py overrides them from the real settings via configure().
    exporter_container: str = DCGM_EXPORTER_CONTAINER
    diag_image: str = DCGM_DIAG_IMAGE
    prometheus_jobs: tuple[str, ...] = tuple(DCGM_PROMETHEUS_JOBS)
    compose_project = "ai-lab-observability"      # a restart is only sent to this project's dcgm-exporter service
                                                  # (the core may override: Settings.dcgm_compose_project)
    compose_service = "dcgm-exporter"

    # ctx.urls: dcgm (this host's exporter), prometheus. ctx.paths: observability_dir, observability_compose,
    # dcgm_counters. ctx.state_dir/dcgm/runs holds the diagnostic runs.
    def __init__(self, ctx: AdapterContext) -> None:
        super().__init__(ctx)
        self.follow_interval = 5.0
        self.poll_interval = 2.0          # post_apply /metrics poll
        self.apply_timeout = 60.0
        self.settle = 1.5                 # let the exporter's file watcher (200 ms debounce) see an edit
        self._gpus: dict[str, str] = {}   # gpu index -> short model, from the last good scrape
        self._baseline: set[str] = set()  # non-PROF families served before an apply (rollback yardstick)
        self._freeze_until = 0.0          # no baseline updates while an apply may be in flight
        self._xid_seen: dict[str, float] = {}
        self._events: deque[tuple[int, SystemEvent]] = deque(maxlen=200)
        self._seq = 0
        self._image_cache: tuple[float, bool] = (NEVER, True)   # NEVER, not 0.0: monotonic() counts from boot
        self._diag_lock = asyncio.Lock()

    def configure(self, *, exporter_container: str, diag_image: str, prometheus_jobs: list[str],
                  compose_project: str | None = None) -> None:
        self.exporter_container = exporter_container
        if compose_project is not None:
            self.compose_project = compose_project
        self.diag_image = diag_image
        self.prometheus_jobs = tuple(prometheus_jobs)

    # ── helpers ─────────────────────────────────────────────────────────────────────────────
    @property
    def _exporter_url(self) -> str:
        return self.ctx.urls.get("dcgm", "http://127.0.0.1:9400").rstrip("/")

    @property
    def _prom_url(self) -> str:
        return self.ctx.urls.get("prometheus", "http://127.0.0.1:9090").rstrip("/")

    @property
    def _runs_dir(self) -> Path:
        return self.ctx.state_dir / "dcgm" / "runs"

    @property
    def _jobs_regex(self) -> str:
        return "|".join(j for j in self.prometheus_jobs if JOB_RE.fullmatch(j))

    async def _run(self, argv: list[str], *, timeout: float, max_bytes: int = 256_000) -> CommandResult:
        try:
            return await self.ctx.runner.run(argv, timeout=timeout, max_bytes=max_bytes)
        except Exception as e:  # noqa: BLE001 - not allowlisted / not on PATH: data, not an exception
            return CommandResult(returncode=127, stdout="", stderr=f"{type(e).__name__}: {e}"[:300])

    def _push(self, ev: SystemEvent) -> None:
        self._seq += 1
        self._events.append((self._seq, ev))

    # ── exporter /metrics ───────────────────────────────────────────────────────────────────
    async def _scrape(self) -> tuple[Samples | None, str]:
        try:
            resp = await self.ctx.http.get(f"{self._exporter_url}/metrics", timeout=5.0, follow_redirects=False)
        except Exception as e:  # noqa: BLE001
            return None, type(e).__name__
        if resp.status_code != 200:
            return None, f"HTTP {resp.status_code}"
        if len(resp.content) > MAX_METRICS_BYTES:
            return None, "response too large"
        return parse_exposition(resp.text), ""

    def _observe(self, samples: Samples) -> None:
        """Learn the GPUs and the baseline family set, and turn XID movement into events. Idempotent per scrape."""
        gpus: dict[str, str] = {}
        for fam, rows in samples.items():
            if fam.startswith("DCGM_FI_DEV_"):
                for labels, _ in rows:
                    g = labels.get("gpu", "")
                    if g.isdigit() and len(g) <= 3 and len(gpus) < MAX_GPUS:
                        gpus[g] = short_model(labels.get("modelName", ""))
        if gpus:
            self._gpus = gpus
        if time.monotonic() >= self._freeze_until:
            fams = {f for f in samples if f.startswith(("DCGM_FI_DEV_", "DCGM_FI_PROF_", "DCGM_EXP_"))}
            if fams:
                self._baseline = fams
        for labels, v in samples.get("DCGM_FI_DEV_XID_ERRORS", []):
            g = labels.get("gpu", "")
            if not g.isdigit():
                continue
            if v > 0 and self._xid_seen.get(g) != v:
                self._push(SystemEvent(ts=time.time(), system="dcgm", kind=EVENT_XID,
                                       summary=f"GPU {g}: XID {int(v)}",
                                       data={"gpu": g, "xid": int(v), "source": "last_xid"}))
            self._xid_seen[g] = v
        for labels, v in samples.get("DCGM_EXP_XID_ERRORS_TOTAL", []):
            g = labels.get("gpu", "")
            if not g.isdigit():
                continue
            prev = self._xid_seen.get("total:" + g, 0.0)
            if v > prev:
                self._push(SystemEvent(ts=time.time(), system="dcgm", kind=EVENT_XID,
                                       summary=f"GPU {g}: {int(v - prev)} new XID error(s), {int(v)} in total",
                                       data={"gpu": g, "xid_total": int(v), "new": int(v - prev),
                                             "source": "xid_total"}))
            self._xid_seen["total:" + g] = v

    def _xid_nonzero(self) -> dict[str, float]:
        out = {g: v for g, v in self._xid_seen.items() if not g.startswith("total:") and v > 0}
        out.update({g[6:]: v for g, v in self._xid_seen.items() if g.startswith("total:") and v > 0})
        return out

    # ── Prometheus (loopback only) ──────────────────────────────────────────────────────────
    async def _prom(self, expr: str) -> list[dict[str, Any]] | None:
        try:
            resp = await self.ctx.http.get(f"{self._prom_url}/api/v1/query", params={"query": expr}, timeout=5.0,
                                           follow_redirects=False)
            if resp.status_code != 200:
                return None
            d = resp.json()
        except Exception:  # noqa: BLE001
            return None
        if not isinstance(d, dict) or d.get("status") != "success":
            return None
        res = _dict(d.get("data")).get("result")
        return [r for r in _list(res) if isinstance(r, dict)][:200]

    @staticmethod
    def _host_of(metric: dict[str, Any]) -> tuple[str, str]:
        job = _clean_label(str(metric.get("job", "?"))) or "?"
        host = metric.get("host")
        return job, _clean_label(str(host)) if isinstance(host, str) and host else ("local" if job == "dcgm" else job)

    @staticmethod
    def _val(row: dict[str, Any]) -> float | None:
        try:
            v = float(_list(row.get("value"))[1])
        except (IndexError, TypeError, ValueError):
            return None
        return v if math.isfinite(v) else None

    async def _hosts_up(self) -> list[tuple[str, str, bool]] | None:
        jobs = self._jobs_regex
        if not jobs:
            return []
        rows = await self._prom(f'up{{job=~"{jobs}"}}')
        if rows is None:
            return None
        out: list[tuple[str, str, bool]] = []
        for r in rows[:MAX_HOSTS]:
            v = self._val(r)
            if v is not None:
                job, host = self._host_of(_dict(r.get("metric")))
                out.append((job, host, v >= 1))
        return sorted(out)

    # ── docker ──────────────────────────────────────────────────────────────────────────────
    async def _inspect(self) -> tuple[str, str | None, str]:
        """(state, image without digest, detail). state: running|stopped|failed|absent|unknown."""
        fmt = "{{.State.Status}}|{{.State.Running}}|{{.Config.Image}}|{{.State.StartedAt}}|{{.State.ExitCode}}"
        r = await self._run(["docker", "inspect", "--format", fmt, self.exporter_container], timeout=10.0,
                            max_bytes=8192)
        if r.returncode != 0:
            low = (r.stderr + r.stdout).lower()
            return ("absent" if "no such" in low else "unknown"), None, redact(r.stderr.strip())[:160]
        parts = r.stdout.strip().split("|")
        if len(parts) < 5:
            return "unknown", None, "unexpected docker inspect output"
        status, running, image, started, code = parts[:5]
        image = image.split("@", 1)[0]
        if running == "true":
            return "running", image, f"started {started[:19]}"
        if status in ("restarting", "dead") or (status == "exited" and code not in ("0", "")):
            return "failed", image, f"{status}, exit code {code}"
        return "stopped", image, status

    async def _identity_problem(self) -> str | None:
        """None when the configured container is the observability project's dcgm-exporter service (compose
        labels); otherwise why a restart is refused."""
        fmt = '{{index .Config.Labels "com.docker.compose.project"}}|{{index .Config.Labels "com.docker.compose.service"}}'
        r = await self._run(["docker", "inspect", "--format", fmt, self.exporter_container], timeout=10.0,
                            max_bytes=4096)
        if r.returncode != 0:
            return f"cannot inspect {self.exporter_container}: {redact(r.stderr.strip())[:160]}"
        got = r.stdout.strip()
        want = f"{self.compose_project}|{self.compose_service}"
        if got != want:
            return (f"refusing to restart {self.exporter_container}: its compose labels are {got!r}, expected "
                    f"{want!r}")
        return None

    def _compose_fix(self) -> str:
        compose = self.ctx.paths.get("observability_compose")
        if compose is not None:
            return f"docker compose -f {compose} --profile gpu up -d dcgm-exporter"
        return "cd observability && docker compose --profile gpu up -d dcgm-exporter"

    def _bump_fix(self) -> str:
        return (f"set DCGM_VERSION={FIXED_TAG}@sha256:<digest> in observability/.env (the exact line is in "
                f"observability/.env.example), then: {self._compose_fix()}")

    # ── observe ─────────────────────────────────────────────────────────────────────────────
    async def _diag_image_present(self) -> bool:
        ts, present = self._image_cache
        if time.monotonic() - ts < PROM_TTL_IMAGE:
            return present
        r = await self._run(["docker", "image", "inspect", "--format", "{{.Id}}", self.diag_image], timeout=10.0,
                            max_bytes=4096)
        present = r.returncode == 0 or r.returncode == 127          # 127: no docker at all -> not this problem
        self._image_cache = (time.monotonic(), present)
        return present

    async def status(self) -> SystemStatus:
        now = time.time()
        (state, image, detail), (samples, scrape_err), hosts = await asyncio.gather(
            self._inspect(), self._scrape(), self._hosts_up())
        if samples is not None:
            self._observe(samples)
        problems: list[Problem] = []
        comps: list[Component] = []
        ver = exporter_version(image)
        comps.append(Component(kind="container", name=self.exporter_container, state=state, image=image,
                               detail=(f"exporter {ver}; " if ver else "") + detail))
        comps.append(Component(kind="http", name="exporter /metrics", state="running" if samples is not None else
                               "stopped", detail=("answers" if samples is not None else scrape_err)))
        if samples is None:
            problems.append(Problem(severity="error", code="dcgm.exporter.down",
                                    message=f"the exporter at {self._exporter_url}/metrics does not answer "
                                            f"({scrape_err}); container state: {state}",
                                    fix=self._compose_fix()))
        if state == "running" and image and ver is None:
            problems.append(Problem(severity="info", code="dcgm.exporter.version_unknown",
                                    message=f"cannot read the exporter version from the image tag {image!r}",
                                    fix="pin the tag as <dcgm version>-<exporter version>, e.g. "
                                        f"{FIXED_TAG}, via DCGM_VERSION in observability/.env"))
        if ver is not None and version_tuple(ver) < FIXED_EXPORTER:
            problems.append(Problem(
                severity="error", code="dcgm.exporter.vulnerable",
                message=f"dcgm-exporter {ver} is older than 4.8.3: CVE-2026-47483, /debug/pprof is served "
                        "unauthenticated", fix=self._bump_fix()))
        if samples is not None:
            with contextlib.suppress(Exception):    # nothing listening: the probe has nothing to report
                resp = await self.ctx.http.get(f"{self._exporter_url}/debug/pprof/", timeout=5.0,
                                               follow_redirects=False)
                if resp.status_code == 200:
                    problems.append(Problem(
                        severity="error", code="dcgm.exporter.pprof_exposed",
                        message="GET /debug/pprof/ answers 200 on the exporter: heap and goroutine dumps are "
                                "readable by anything that reaches the port", fix=self._bump_fix()))
        gpu_n = len(self._gpus) if samples is not None else 0
        if samples is not None and gpu_n == 0:
            problems.append(Problem(severity="warn", code="dcgm.gpus.none",
                                    message="the exporter answers but reports no GPU",
                                    fix=f"docker logs --tail 50 {self.exporter_container}"))
        xids = self._xid_nonzero() if samples is not None else {}
        if xids:
            listing = ", ".join(f"GPU {g}: {int(v)}" for g, v in sorted(xids.items()))
            problems.append(Problem(
                severity="warn", code="dcgm.xid.nonzero", message=f"XID errors reported ({listing})",
                fix="journalctl -k | grep -i 'NVRM: Xid'   # then look the code up in NVIDIA's XID catalog"))
        up_n = 0
        if hosts is None:
            comps.append(Component(kind="http", name="prometheus", state="stopped", detail="query failed"))
            problems.append(Problem(
                severity="warn", code="dcgm.prometheus.unreachable",
                message=f"Prometheus at {self._prom_url} did not answer; other hosts' GPUs cannot be listed",
                fix="docker compose -f observability/docker-compose.yml up -d prometheus"))
        else:
            comps.append(Component(kind="http", name="prometheus", state="running", detail="answers"))
            down: list[str] = []
            for job, host, up in hosts:
                comps.append(Component(kind="http", name=f"{host} ({job})", state="running" if up else "stopped",
                                       detail="scraped by Prometheus" if up else "scrape failing (up == 0)"))
                up_n += up
                if not up:
                    down.append(f"{host} ({job})")
            seen_jobs = {j for j, _, _ in hosts}
            for job in self.prometheus_jobs:
                if job not in seen_jobs and JOB_RE.fullmatch(job):
                    down.append(f"{job} (no target)")
            if down:
                problems.append(Problem(
                    severity="warn", code="dcgm.host.down",
                    message=f"{len(down)} DCGM target(s) are not being scraped: {', '.join(down[:8])}",
                    fix="open http://127.0.0.1:9090/targets?search=dcgm for the scrape error; a powered-off "
                        "host is expected to be listed here"))
        if samples is not None and not await self._diag_image_present():
            problems.append(Problem(
                severity="info", code="dcgm.diag.image_missing",
                message="the diagnostics image is not present locally; diagnostics need it (runs use --pull never)",
                fix=f"docker pull {self.diag_image}"))
        health: Health
        if samples is None:
            health = "down"
        elif any(p.severity in ("error", "warn") for p in problems):
            health = "degraded"
        else:
            health = "up"
        total = len(hosts) if hosts else 0
        summary = (f"{gpu_n} GPU(s) local" if samples is not None else "exporter not answering")
        summary += f" · {up_n}/{total} DCGM hosts scraped" if hosts is not None else " · Prometheus unreachable"
        if ver:
            summary += f" · exporter {ver}"
        return SystemStatus(
            system="dcgm", health=health, version=ver, summary=summary,
            endpoints=[Endpoint(name="exporter metrics", url=f"{self._exporter_url}/metrics", role="metrics",
                                auth="none"),
                       Endpoint(name="prometheus", url=self._prom_url, role="api", auth="none")],
            components=comps, problems=problems, checked_at=now)

    async def metrics(self) -> MetricsSnapshot:
        now = time.time()
        jobs = self._jobs_regex
        sel = f'{{job=~"{jobs}"}}' if jobs else None
        (samples, err) = await self._scrape()
        pts: list[MetricPoint] = []
        if samples is not None:
            self._observe(samples)
            for fam, key, unit, scale in GPU_METRICS:
                for labels, v in samples.get(fam, []):
                    g = labels.get("gpu", "")
                    if not g.isdigit() or len(g) > 3:
                        continue
                    pts.append(MetricPoint(key=key, value=v * scale, unit=unit,
                                           labels={"gpu": g, "model": _clean_label(short_model(labels.get("modelName", "")))}))
        fleet_err = ""
        if sel is not None:
            up, cnt, tmax, pwr = await asyncio.gather(
                self._hosts_up(), self._prom(f"count by (job, host) (DCGM_FI_DEV_GPU_TEMP{sel})"),
                self._prom(f"max by (job, host) (DCGM_FI_DEV_GPU_TEMP{sel})"),
                self._prom(f"sum by (job, host) (DCGM_FI_DEV_POWER_USAGE{sel})"))
            if up is None:
                fleet_err = "Prometheus did not answer; no fleet rollups"
            for job, host, ok in up or []:
                pts.append(MetricPoint(key="dcgm.host_up", value=1.0 if ok else 0.0, unit="1",
                                       labels={"host": host, "job": job}))
            for rows, key, unit in ((cnt, "dcgm.fleet_gpus", "1"), (tmax, "dcgm.fleet_temp_max_c", "C"),
                                    (pwr, "dcgm.fleet_power_w", "W")):
                for r in (rows or [])[:MAX_HOSTS]:
                    val = self._val(r)
                    if val is not None:
                        job, host = self._host_of(_dict(r.get("metric")))
                        pts.append(MetricPoint(key=key, value=val, unit=unit, labels={"host": host, "job": job}))
        if not pts:
            return MetricsSnapshot(system="dcgm", ts=now, available=False,
                                   error=f"exporter: {err or 'no GPU metrics'}; {fleet_err or 'no fleet data'}"[:300])
        error = None
        if samples is None:
            error = f"local exporter not answering ({err}); showing the fleet rollups only"
        elif fleet_err:
            error = fleet_err
        return MetricsSnapshot(system="dcgm", ts=now, available=True, points=pts, error=error)

    # ── events ──────────────────────────────────────────────────────────────────────────────
    def _diag_dirs(self) -> list[Path]:
        try:
            return sorted((p for p in self._runs_dir.iterdir() if RUN_ID_RE.fullmatch(p.name) and p.is_dir()),
                          key=lambda p: p.name, reverse=True)
        except OSError:
            return []

    @staticmethod
    def _load_meta(run_dir: Path) -> dict[str, Any] | None:
        try:
            d = json.loads((run_dir / "run.json").read_text())
        except (OSError, ValueError):
            return None
        return d if isinstance(d, dict) else None

    @staticmethod
    def _diag_event(meta: dict[str, Any]) -> SystemEvent:
        tests = [t for t in _list(meta.get("tests")) if isinstance(t, dict)]
        passed = sum(1 for t in tests if str(t.get("status", "")).lower().startswith("pass"))
        summary = (f"diag level {meta.get('level')} on GPU {meta.get('gpu')}: {meta.get('verdict')} "
                   f"({passed}/{len(tests)} tests passed)")
        data = {k: meta.get(k) for k in ("run_id", "gpu", "level", "verdict", "duration_ms", "model")}
        data["tests"] = {str(t.get("name")): str(t.get("status")) for t in tests[:20]}
        ts = meta.get("ts")
        return SystemEvent(ts=float(ts) if isinstance(ts, int | float) else 0.0, system="dcgm", kind=EVENT_DIAG,
                           summary=summary[:300], data=data)

    def _diag_events(self, limit: int) -> list[SystemEvent]:
        out: list[SystemEvent] = []
        for d in self._diag_dirs()[: max(limit, 0)]:
            meta = self._load_meta(d)
            if meta is not None:
                out.append(self._diag_event(meta))
        return out

    async def recent_events(self, limit: int = 100) -> list[SystemEvent]:
        limit = max(0, min(limit, 200))
        events = await asyncio.to_thread(self._diag_events, limit)
        events += [ev for _, ev in self._events]
        events.sort(key=lambda e: e.ts, reverse=True)
        return events[:limit]

    async def follow_events(self, since_ts: float | None = None) -> AsyncIterator[SystemEvent]:
        seen_runs: set[str] = set()
        seq = self._seq if since_ts is None else 0
        first = True
        while True:
            samples, _ = await self._scrape()
            if samples is not None:
                self._observe(samples)
            fresh: list[SystemEvent] = []
            for n, ev in list(self._events):
                if n > seq:
                    seq = n
                    if not (first and since_ts is not None and ev.ts <= since_ts):
                        fresh.append(ev)
            for ev in sorted(await asyncio.to_thread(self._diag_events, 50), key=lambda e: e.ts):
                rid = str(ev.data.get("run_id"))
                if rid in seen_runs:
                    continue
                seen_runs.add(rid)
                if first and (since_ts is None or ev.ts <= since_ts):
                    continue
                fresh.append(ev)
            first = False
            for ev in sorted(fresh, key=lambda e: e.ts):
                yield ev
            await asyncio.sleep(self.follow_interval)

    # ── configure: counters.csv ─────────────────────────────────────────────────────────────
    def config_files(self) -> list[ConfigFileSpec]:
        if self.ctx.paths.get("dcgm_counters") is None:
            return []
        return [ConfigFileSpec(
            id=COUNTERS_ID, label="Exporter counters", display_path="observability/dcgm/counters.csv",
            description="The DCGM fields the exporter collects, one `FIELD, type, help` row each (a custom file "
                        "REPLACES the default set; commented rows are documentation). The directory is mounted "
                        "read-only into the exporter, which re-reads the file when it changes.",
            format="text", editable=True,
            validators=[("field names and types checked against the lists shipped in the exporter image "
                         f"(default-counters.csv + dcp-metrics-included.csv, {len(KNOWN_FIELDS)} names)")],
            authoritative=False,
            apply_effect="applied by the exporter itself within ~1 s (no restart); with 'restart' the exporter "
                         "container is restarted (~10 s without metrics). DCGM_FI_PROF_* families return after the "
                         "first collection cycle (10-20 s). The core rolls the file back if /metrics stops "
                         "answering or a family that was served disappears.")]

    def config_path(self, file_id: str) -> Path:
        p = self.ctx.paths.get("dcgm_counters")
        if file_id != COUNTERS_ID or p is None:
            raise KeyError(file_id)
        if p.is_symlink() or not p.is_file():
            raise KeyError(file_id)
        return p

    async def read_config(self, file_id: str) -> ConfigFile:
        p = self.config_path(file_id)
        try:
            st = p.stat()
            if st.st_size > MAX_CONFIG_BYTES:
                raise FileNotFoundError(file_id)
            content = p.read_text()
        except OSError:
            raise FileNotFoundError(file_id) from None
        return ConfigFile(spec=self.config_files()[0], content=content, sha256=sha256_hex(content.encode()),
                          mtime=st.st_mtime)

    async def validate_config(self, file_id: str, content: str) -> ValidationResult:
        if file_id != COUNTERS_ID:
            raise KeyError(file_id)
        name = "dcgm-exporter field list check"
        note = (f"A proxy check: field names and types come from the CSV lists shipped in the dcgm-exporter image "
                f"({len(KNOWN_FIELDS)} names). The exporter accepts more fields than those lists name, so an "
                "unlisted name without a close match is only a warning. Whether the exporter really serves each "
                "family is decided after the apply by polling /metrics.")
        if len(content.encode()) > MAX_CONFIG_BYTES:
            return ValidationResult(ok=False, validator=name, authoritative=False, note=note, diagnostics=[
                Diagnostic(severity="error", message="file larger than 256 KiB")])
        _, diags = parse_counters(content)
        # an apply normally follows a validate: freeze the rollback yardstick at what is served right now
        samples, _ = await self._scrape()
        if samples is not None:
            self._observe(samples)
        self._freeze_until = time.monotonic() + 300.0
        return ValidationResult(ok=not any(d.severity == "error" for d in diags), validator=name,
                                authoritative=False, diagnostics=diags, note=note)

    async def _logged_errors(self, since: float) -> list[str]:
        stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(since))
        r = await self._run(["docker", "logs", "--since", stamp, "--tail", "200", self.exporter_container],
                            timeout=15.0, max_bytes=64_000)
        if r.returncode != 0:
            return []
        lines = (r.stdout + "\n" + r.stderr).splitlines()
        return [redact(ln.strip())[:240] for ln in lines if "level=ERROR" in ln][:3]

    async def post_apply(self, file_id: str, *, restart: bool) -> ApplyResult:
        path = self.config_path(file_id)
        began = time.time() - 2.0
        notes: list[str] = []
        restarted = False
        if restart:
            why = await self._identity_problem()
            if why is not None:
                return ApplyResult(ok=False, restarted=False, post_check="unhealthy", detail=why)
            r = await self._run(["docker", "restart", self.exporter_container], timeout=90.0, max_bytes=4096)
            if r.returncode != 0:
                return ApplyResult(ok=False, restarted=False, post_check="unhealthy",
                                   detail=f"docker restart failed: {redact(r.stderr.strip())[:200]}")
            restarted = True
        else:
            await asyncio.sleep(self.settle)
        try:
            rows, _ = parse_counters(path.read_text())
        except OSError:
            rows = []
        configured = {f for _, f, t in rows if t != "label"}
        required = {f for f in self._baseline if f in configured and "_PROF_" not in f}
        prof = {f for f in self._baseline if f in configured and "_PROF_" in f}
        deadline = time.monotonic() + self.apply_timeout
        present: set[str] = set()
        answered = False
        last_err = ""
        while True:
            samples, last_err = await self._scrape()
            if samples is not None:
                answered = True
                present = {f for f in samples if f.startswith(("DCGM_FI_", "DCGM_EXP_"))}
                extra = {f for f in present if f not in configured and f not in KNOWN_LABEL_FAMILIES}
                if required <= present and prof <= present and (restarted or not extra):
                    break
            if time.monotonic() >= deadline:
                break
            await asyncio.sleep(self.poll_interval)
        if not answered:
            return ApplyResult(ok=False, restarted=restarted, post_check="unhealthy",
                               detail=f"/metrics did not answer within {self.apply_timeout:.0f} s ({last_err})")
        missing = sorted(required - present)
        if missing:
            return ApplyResult(ok=False, restarted=restarted, post_check="unhealthy", detail=(
                "families served before the change are missing: " + ", ".join(missing[:10])))
        errs = await self._logged_errors(began)
        if errs:
            return ApplyResult(ok=False, restarted=restarted, post_check="unhealthy",
                               detail="the exporter logged errors after the change: " + " | ".join(errs))
        stale = sorted(f for f in present if f not in configured and f not in KNOWN_LABEL_FAMILIES)
        if stale and not restarted:
            return ApplyResult(ok=False, restarted=False, post_check="unhealthy", detail=(
                "the exporter still serves fields that are no longer in the file (" + ", ".join(stale[:6]) +
                "): it did not re-read the file; apply with restart"))
        if prof - present:
            notes.append("DCGM_FI_PROF_* families not served yet: " + ", ".join(sorted(prof - present)[:5]))
        self._freeze_until = 0.0
        self._baseline = {f for f in present}
        return ApplyResult(ok=True, restarted=restarted, post_check="healthy", detail=(
            f"/metrics answers with {len(present)} DCGM families" + (f"; {'; '.join(notes)}" if notes else "")))

    # ── act ─────────────────────────────────────────────────────────────────────────────────
    def _gpu_param(self) -> ParamSpec:
        known = sorted(self._gpus, key=int)
        return ParamSpec(
            key="gpu", label="GPU index", type="enum" if known else "int", choices=known or None,
            default=known[0] if known else 0, min=None if known else 0, max=None if known else MAX_GPUS - 1,
            requires_restart=False,
            help="One GPU per run: the two cards are different models and DCGM refuses a mixed group. "
                 + ("; ".join(f"{g} = {m}" for g, m in sorted(self._gpus.items(), key=lambda kv: int(kv[0])))
                    if known else "No GPU discovered yet."))

    def actions(self) -> list[ActionSpec]:
        return [
            ActionSpec(id="diag", label="Diagnostic, level 1", danger="disruptive",
                       description="`dcgmi diag -r 1` on one GPU in a throwaway container with SYS_ADMIN and GPU "
                                   "access (software deployment checks, a few seconds, no load). Stored as a run; the GPU serial number is "
                                   "never shown.", params=[self._gpu_param()]),
            ActionSpec(id="diag_level2", label="Diagnostic, level 2", danger="disruptive",
                       description="`dcgmi diag -r 2` on one GPU: loads the card for minutes (memory and PCIe "
                                   "tests). While a model is served on that card, inference stalls or slows; "
                                   "run it only when the card is idle.", params=[self._gpu_param()]),
            ActionSpec(id="discovery", label="Discover GPUs", danger="safe",
                       description="`dcgmi discovery -l` in a throwaway container: the GPUs DCGM sees "
                                   "(UUIDs shortened)."),
            ActionSpec(id="restart_exporter", label="Restart exporter", danger="disruptive",
                       description=f"`docker restart {self.exporter_container}`: /metrics is unavailable for "
                                   "~10 s and DCGM_FI_PROF_* families take 10-20 s to return; Prometheus shows a "
                                   "gap."),
        ]

    def _docker_run(self, name: str, script: str) -> list[str]:
        return ["docker", "run", "--rm", "--pull", "never", "--name", name, "--gpus", "all", "--cap-drop", "ALL",
                "--cap-add", "SYS_ADMIN", "--network", "none", "--security-opt", "no-new-privileges", "--read-only",
                "--tmpfs", "/tmp", "--pids-limit", "256", "--memory", "2g",
                "--entrypoint", "/bin/sh", self.diag_image, "-c", script]

    async def _force_remove(self, name: str) -> None:
        await self._run(["docker", "rm", "-f", name], timeout=30.0, max_bytes=2048)

    async def run_action(self, action_id: str, params: dict[str, object]) -> ActionResult:
        t0 = time.monotonic()

        def done(ok: bool, out: str, problems: list[Problem] | None = None) -> ActionResult:
            return ActionResult(ok=ok, output=redact(out)[:20_000], duration_ms=int((time.monotonic() - t0) * 1000),
                                problems=problems or [])

        if action_id not in {"diag", "diag_level2", "discovery", "restart_exporter"}:
            raise KeyError(action_id)
        if action_id == "restart_exporter":
            why = await self._identity_problem()
            if why is not None:
                return done(False, why)
            r = await self._run(["docker", "restart", self.exporter_container], timeout=90.0, max_bytes=4096)
            if r.returncode != 0:
                return done(False, f"docker restart failed: {r.stderr.strip()}")
            deadline = time.monotonic() + self.apply_timeout
            while time.monotonic() < deadline:
                samples, _ = await self._scrape()
                if samples is not None:
                    self._observe(samples)
                    return done(True, f"restarted; /metrics answers again after {time.monotonic() - t0:.0f} s")
                await asyncio.sleep(self.poll_interval)
            return done(False, "restarted, but /metrics did not answer within the wait",
                        [Problem(severity="error", code="dcgm.exporter.down",
                                 message="the exporter does not answer after the restart",
                                 fix=f"docker logs --tail 50 {self.exporter_container}")])
        if action_id == "discovery":
            return await self._discovery(done)
        level = 1 if action_id == "diag" else 2
        gpu = self._check_gpu(params.get("gpu"))
        if gpu is None:
            known = ", ".join(sorted(self._gpus, key=int)) or "none discovered yet"
            return done(False, f"gpu must be an integer index of a discovered GPU ({known})")
        return await self._diag(level, gpu, done)

    def _check_gpu(self, raw: object) -> int | None:
        if isinstance(raw, bool):
            return None
        s = str(raw).strip() if isinstance(raw, int | str) else ""
        if not re.fullmatch(r"\d{1,2}", s):
            return None
        n = int(s)
        if n >= MAX_GPUS or (self._gpus and str(n) not in self._gpus):
            return None
        return n

    def _image_problem(self) -> Problem:
        return Problem(severity="error", code="dcgm.diag.image_missing",
                       message="the DCGM diagnostics image is not present locally (runs use --pull never)",
                       fix=f"docker pull {self.diag_image}")

    async def _discovery(self, done: Done) -> ActionResult:
        name = f"nemo-console-dcgm-disc-{int(time.time() * 1000) % 10**9}"
        r = await self._run(self._docker_run(name, DISCOVERY_CMD), timeout=90.0, max_bytes=64_000)
        if r.timed_out:
            await self._force_remove(name)
        if r.returncode != 0:
            low = r.stderr.lower()
            probs = [self._image_problem()] if "no such image" in low or "unable to find image" in low else []
            return done(False, f"discovery failed (rc {r.returncode}): {r.stderr.strip()[-600:]}", probs)
        text = UUID_RE.sub(lambda m: m.group(0)[:12] + "...", r.stdout)
        return done(True, text.strip())

    def _new_run_dir(self, level: int, gpu: int) -> tuple[str, Path]:
        stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime()) + f"-{int(time.time() * 1000) % 1000:03d}"
        run_id = f"{stamp}-diag{level}-gpu{gpu}"
        run_dir = self._runs_dir / run_id
        run_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
        return run_id, run_dir

    def _prune(self) -> None:
        for d in self._diag_dirs()[KEEP_RUNS:]:
            shutil.rmtree(d, ignore_errors=True)

    @staticmethod
    def _write_json(path: Path, doc: dict[str, Any]) -> None:
        tmp = path.with_name("." + path.name + ".tmp")
        tmp.write_text(json.dumps(doc, indent=1))
        tmp.chmod(0o600)
        tmp.replace(path)

    async def _diag(self, level: int, gpu: int, done: Done) -> ActionResult:
        if self._diag_lock.locked():
            return done(False, "another diagnostic is running; wait for it to finish")
        async with self._diag_lock:
            run_id, run_dir = await asyncio.to_thread(self._new_run_dir, level, gpu)
            name = f"nemo-console-dcgm-{run_id.lower()}"
            t0 = time.monotonic()
            r = await self._run(self._docker_run(name, DIAG_CMD[level].format(gpu=gpu)), timeout=DIAG_TIMEOUT[level],
                                max_bytes=512_000)
            if r.timed_out:
                await self._force_remove(name)
            duration = int((time.monotonic() - t0) * 1000)
            doc: dict[str, Any] | None = None
            tests: list[dict[str, str]]
            a, b = r.stdout.find("{"), r.stdout.rfind("}")
            if a >= 0 < b:
                with contextlib.suppress(ValueError):
                    loaded = json.loads(r.stdout[a:b + 1])
                    doc = loaded if isinstance(loaded, dict) else None
            low = r.stderr.lower()
            problems: list[Problem] = []
            if doc is None:
                verdict, tests, notes = "error", [], [redact((r.stderr or r.stdout).strip())[-300:]]
                if r.timed_out:
                    notes = [f"timed out after {DIAG_TIMEOUT[level]:.0f} s (container removed)"]
                if "no such image" in low or "unable to find image" in low:
                    problems.append(self._image_problem())
            else:
                verdict, tests, notes = summarize_diag(_strip_serials(doc))
            model = self._gpus.get(str(gpu), "")
            meta: dict[str, Any] = {"run_id": run_id, "ts": time.time(), "gpu": gpu, "level": level,
                                    "verdict": verdict, "duration_ms": duration, "model": model, "tests": tests,
                                    "notes": notes[:10], "returncode": r.returncode}
            with contextlib.suppress(OSError):
                await asyncio.to_thread(self._write_json, run_dir / "run.json", meta)
                if doc is not None:
                    await asyncio.to_thread(self._write_json, run_dir / "output.json", _strip_serials(doc))
                await asyncio.to_thread(self._prune)
            self._image_cache = (NEVER, True)
            where = f"GPU {gpu} ({model})" if model else f"GPU {gpu}"
            lines = [f"DCGM diagnostic level {level} on {where}: {verdict.upper()} in {duration / 1000:.1f} s",
                     f"run: {run_id}"]
            lines += [f"  {t['name']:<22} {t['status']}" for t in tests]
            lines += [f"  note: {n}" for n in notes[:10]]
            return done(verdict in ("pass", "warn"), "\n".join(lines), problems)


# Label-only fields never appear as families of their own (they are labels on the others).
KNOWN_LABEL_FAMILIES: frozenset[str] = frozenset(f for f, t in KNOWN_FIELDS.items() if t == "l")
