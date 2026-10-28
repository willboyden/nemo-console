//! DCGM adapter: the dcgm-exporter on this host plus every DCGM host Prometheus scrapes (addendum 9.8). OWNER: workstream B.
//! Port of `backend/src/nemo_console/adapters/dcgm.py` (same probes, argv, texts, files and events).
//!
//! Facts this adapter is built on (evidence: tests/fixtures/dcgm, captured live 2026-10-24):
//!   * The exporter (`nvcr.io/nvidia/k8s/dcgm-exporter:4.6.1-4.8.4`) serves `/metrics`; the image tag is
//!     `<DCGM lib version>-<exporter version>` and exporters before 4.8.3 serve `/debug/pprof` unauthenticated
//!     (CVE-2026-47483).
//!   * Per-GPU families carry `gpu` and `modelName` (only these two become labels, never UUID/serial).
//!     `DCGM_FI_DEV_XID_ERRORS` is the value of the LAST XID and absent while none was seen.
//!   * Prometheus series of remote hosts carry `job` + `host`; this host's (job `dcgm`) carry no `host`: "local".
//!   * Diagnostics run `dcgmi diag -j` in a throwaway container from the full DCGM image, one GPU per run; the JSON
//!     contains the GPU serial number, which is dropped before anything is stored or shown.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use indexmap::IndexMap;
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::adapters::base::{
    AdapterContext, AdapterError, AdapterRef, CommandResult, EventStream, SystemAdapter,
};
use crate::config::{
    DCGM_COMPOSE_PROJECT, DCGM_DIAG_IMAGE, DCGM_EXPORTER_CONTAINER, DCGM_PROMETHEUS_JOBS, Settings,
};
use crate::domain::models::{
    ActionResult, ActionSpec, ApplyResult, Component, ConfigFile, ConfigFileSpec, Danger,
    Diagnostic, Endpoint, Health, MetricPoint, MetricsSnapshot, ParamSpec, ParamType, PostCheck,
    Problem, Severity, SystemEvent, SystemId, SystemStatus, ValidationResult, now_ts,
};
use crate::services::common::{redact, sha256_hex};
use crate::services::difflib;
use crate::services::plumbing as wsb;
use crate::services::pyfmt::str_repr;
use crate::services::pystr;

/// `DcgmAdapter.label` in Python.
pub const LABEL: &str = "DCGM";
/// First version without the unauthenticated /debug/pprof.
pub const FIXED_EXPORTER: [i64; 3] = [4, 8, 3];
/// The tag observability/.env.example pins.
pub const FIXED_TAG: &str = "4.6.1-4.8.4";
pub const KEEP_RUNS: usize = 100;
pub const EVENT_XID: &str = "dcgm.xid";
pub const EVENT_DIAG: &str = "dcgm.diag";
pub const MAX_CONFIG_BYTES: u64 = 256 * 1024;
pub const MAX_METRICS_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_GPUS: usize = 16;
pub const MAX_HOSTS: usize = 32;
pub const PROM_TTL_IMAGE: f64 = 60.0;
pub const COUNTERS_ID: &str = "counters";
/// The compose service a restart is restricted to (the project comes from Settings.dcgm_compose_project).
pub const COMPOSE_SERVICE: &str = "dcgm-exporter";

/// read-only root: the diagnostic needs a writable home/cwd (/tmp); the hostengine pid file goes to /tmp too.
pub const SH_PREFIX: &str =
    "cd /tmp; nv-hostengine --pid /tmp/nvhe.pid --home-dir /tmp >/dev/null 2>&1; sleep 2; ";
pub const DISCOVERY_CMD: &str = "cd /tmp; nv-hostengine --pid /tmp/nvhe.pid --home-dir /tmp >/dev/null 2>&1; sleep 2; dcgmi discovery -l";

/// `DIAG_CMD[level]` (the `{gpu}` placeholder is substituted with a validated integer only).
#[must_use]
pub fn diag_cmd(level: u8) -> String {
    format!("{SH_PREFIX}dcgmi diag -r {level} -i {{gpu}} -j")
}

/// `DIAG_TIMEOUT[level]`: level 2 runs minutes on a loaded card (the runner caps at 600 s).
#[must_use]
pub fn diag_timeout(level: u8) -> f64 {
    if level == 1 { 120.0 } else { 600.0 }
}

static JOB_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9_-]{1,64}\z").ok());
static FIELD_RE: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9_]+\z").ok());
static RUN_ID_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A[0-9]{8}T[0-9]{6}-[0-9]{3}-diag[12]-gpu[0-9]{1,2}\z").ok());
static IMAGE_TAG_RE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"dcgm-exporter:([0-9]+\.[0-9]+\.[0-9]+)-([0-9]+\.[0-9]+\.[0-9]+)").ok()
});
/// Python `\s` / `\S` over str: `str.isspace()` characters.
const PY_WS: &str = r"\t\n\x0b\x0c\r\x1c-\x1f \x{85}\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}";
static SAMPLE_RE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(&format!(
        r"\A([A-Za-z_:][A-Za-z0-9_:]*)(?:\{{(.*)\}})?[{PY_WS}]+([^{PY_WS}]+)(?:[{PY_WS}]+-?\d+)?[{PY_WS}]*\n?\z"
    ))
    .ok()
});
static LABEL_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"([A-Za-z_][A-Za-z0-9_]*)="((?:[^"\\]|\\.)*)""#).ok());
static UUID_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"GPU-[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}").ok());
static LONG_DIGITS_RE: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\b\d{9,}\b").ok());
static GPU_IDX_RE: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\A[0-9]{1,2}\z").ok());
static MODEL_STRIP_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"[^A-Za-z0-9._ -]").ok());
static LABEL_BAD_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"[^A-Za-z0-9._:/-]").ok());
static DASHES_RE: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"-{2,}").ok());

fn full(re: &LazyLock<Option<Regex>>, s: &str) -> bool {
    re.as_ref().is_some_and(|r| r.is_match(s))
}

fn sub(re: &LazyLock<Option<Regex>>, s: &str, rep: &str) -> String {
    match re.as_ref() {
        Some(r) => r.replace_all(s, regex::NoExpand(rep)).into_owned(),
        None => s.to_owned(),
    }
}

fn mask_uuids(s: &str) -> String {
    match UUID_RE.as_ref() {
        Some(r) => r
            .replace_all(s, |c: &regex::Captures<'_>| {
                format!("{}...", pystr::head(&c[0], 12))
            })
            .into_owned(),
        None => s.to_owned(),
    }
}

/// Field names and types known to the exporter (default-counters.csv + dcp-metrics-included.csv of
/// `nvcr.io/nvidia/k8s/dcgm-exporter:4.6.1-4.8.4`, 86 names). g=gauge c=counter l=label.
pub const KNOWN_FIELDS: [(&str, char); 86] = [
    ("DCGM_EXP_CLOCK_EVENTS_COUNT", 'g'),
    ("DCGM_EXP_CLOCK_EVENTS_TOTAL", 'c'),
    ("DCGM_EXP_GPU_HEALTH_STATUS", 'g'),
    ("DCGM_EXP_P2P_STATUS", 'g'),
    ("DCGM_EXP_XID_ERRORS_COUNT", 'g'),
    ("DCGM_EXP_XID_ERRORS_TOTAL", 'c'),
    ("DCGM_FI_DEV_BOARD_LIMIT_VIOLATION", 'c'),
    ("DCGM_FI_DEV_BRAND", 'l'),
    ("DCGM_FI_DEV_CORRECTABLE_REMAPPED_ROWS", 'c'),
    ("DCGM_FI_DEV_DEC_UTIL", 'g'),
    ("DCGM_FI_DEV_ECC_DBE_AGG_TOTAL", 'c'),
    ("DCGM_FI_DEV_ECC_DBE_VOL_TOTAL", 'c'),
    ("DCGM_FI_DEV_ECC_INFOROM_VER", 'l'),
    ("DCGM_FI_DEV_ECC_SBE_AGG_TOTAL", 'c'),
    ("DCGM_FI_DEV_ECC_SBE_VOL_TOTAL", 'c'),
    ("DCGM_FI_DEV_ENC_UTIL", 'g'),
    ("DCGM_FI_DEV_ENFORCED_POWER_LIMIT", 'g'),
    ("DCGM_FI_DEV_FABRIC_HEALTH_MASK", 'g'),
    ("DCGM_FI_DEV_FABRIC_HEALTH_SUMMARY", 'g'),
    ("DCGM_FI_DEV_FB_FREE", 'g'),
    ("DCGM_FI_DEV_FB_RESERVED", 'g'),
    ("DCGM_FI_DEV_FB_USED", 'g'),
    ("DCGM_FI_DEV_GPU_TEMP", 'g'),
    ("DCGM_FI_DEV_GPU_UTIL", 'g'),
    ("DCGM_FI_DEV_INFOROM_IMAGE_VER", 'l'),
    ("DCGM_FI_DEV_LOW_UTIL_VIOLATION", 'c'),
    ("DCGM_FI_DEV_MEMORY_TEMP", 'g'),
    ("DCGM_FI_DEV_MEM_CLOCK", 'g'),
    ("DCGM_FI_DEV_MEM_COPY_UTIL", 'g'),
    ("DCGM_FI_DEV_NVLINK_BANDWIDTH_L0", 'g'),
    ("DCGM_FI_DEV_NVLINK_BANDWIDTH_TOTAL", 'g'),
    ("DCGM_FI_DEV_NVLINK_CRC_DATA_ERROR_TOTAL", 'c'),
    ("DCGM_FI_DEV_NVLINK_CRC_ERROR_TOTAL", 'c'),
    ("DCGM_FI_DEV_NVLINK_CRC_FLIT_ERROR_TOTAL", 'c'),
    ("DCGM_FI_DEV_NVLINK_RECOVERY_ERROR_TOTAL", 'c'),
    ("DCGM_FI_DEV_NVLINK_RECOVERY_EVENT_TOTAL", 'c'),
    ("DCGM_FI_DEV_NVLINK_RECOVERY_FAILED_TOTAL", 'c'),
    ("DCGM_FI_DEV_NVLINK_RECOVERY_SUCCESSFUL_TOTAL", 'c'),
    ("DCGM_FI_DEV_NVLINK_RECOVERY_TOTAL", 'c'),
    ("DCGM_FI_DEV_NVLINK_REPLAY_ERROR_TOTAL", 'c'),
    ("DCGM_FI_DEV_NVLINK_REPLAY_TOTAL", 'c'),
    ("DCGM_FI_DEV_OEM_INFOROM_VER", 'l'),
    ("DCGM_FI_DEV_PCIE_REPLAY_COUNTER", 'c'),
    ("DCGM_FI_DEV_POWER_INFOROM_VER", 'l'),
    ("DCGM_FI_DEV_POWER_MGMT_LIMIT", 'g'),
    ("DCGM_FI_DEV_POWER_USAGE", 'g'),
    ("DCGM_FI_DEV_POWER_VIOLATION", 'c'),
    ("DCGM_FI_DEV_RELIABILITY_VIOLATION", 'c'),
    ("DCGM_FI_DEV_RETIRED_DBE", 'c'),
    ("DCGM_FI_DEV_RETIRED_PENDING", 'g'),
    ("DCGM_FI_DEV_RETIRED_SBE", 'c'),
    ("DCGM_FI_DEV_ROW_REMAP_FAILURE", 'g'),
    ("DCGM_FI_DEV_SERIAL", 'l'),
    ("DCGM_FI_DEV_SM_CLOCK", 'g'),
    ("DCGM_FI_DEV_SYNC_BOOST_VIOLATION", 'c'),
    ("DCGM_FI_DEV_THERMAL_VIOLATION", 'c'),
    ("DCGM_FI_DEV_TOTAL_ENERGY_CONSUMPTION", 'c'),
    ("DCGM_FI_DEV_UNCORRECTABLE_REMAPPED_ROWS", 'c'),
    ("DCGM_FI_DEV_VBIOS_VERSION", 'l'),
    ("DCGM_FI_DEV_VGPU_LICENSE_STATUS", 'g'),
    ("DCGM_FI_DEV_XID_ERRORS", 'g'),
    ("DCGM_FI_DRIVER_VERSION", 'l'),
    ("DCGM_FI_NVML_VERSION", 'l'),
    ("DCGM_FI_PROF_DFMA_CYCLES_ACTIVE_TOTAL", 'c'),
    ("DCGM_FI_PROF_DMMA_CYCLES_ACTIVE_TOTAL", 'c'),
    ("DCGM_FI_PROF_DRAM_ACTIVE", 'g'),
    ("DCGM_FI_PROF_FP16_CYCLES_ACTIVE_TOTAL", 'c'),
    ("DCGM_FI_PROF_FP32_CYCLES_ACTIVE_TOTAL", 'c'),
    ("DCGM_FI_PROF_FP64_CYCLES_ACTIVE_TOTAL", 'c'),
    ("DCGM_FI_PROF_GR_ENGINE_ACTIVE", 'g'),
    ("DCGM_FI_PROF_HMMA_CYCLES_ACTIVE_TOTAL", 'c'),
    ("DCGM_FI_PROF_IMMA_CYCLES_ACTIVE_TOTAL", 'c'),
    ("DCGM_FI_PROF_INT_CYCLES_ACTIVE_TOTAL", 'c'),
    ("DCGM_FI_PROF_MMA_CYCLES_ACTIVE_TOTAL", 'c'),
    ("DCGM_FI_PROF_PCIE_RX_BYTES", 'g'),
    ("DCGM_FI_PROF_PCIE_RX_BYTES_TOTAL", 'c'),
    ("DCGM_FI_PROF_PCIE_TX_BYTES", 'g'),
    ("DCGM_FI_PROF_PCIE_TX_BYTES_TOTAL", 'c'),
    ("DCGM_FI_PROF_PIPE_FP16_ACTIVE", 'g'),
    ("DCGM_FI_PROF_PIPE_FP32_ACTIVE", 'g'),
    ("DCGM_FI_PROF_PIPE_FP64_ACTIVE", 'g'),
    ("DCGM_FI_PROF_PIPE_TENSOR_ACTIVE", 'g'),
    ("DCGM_FI_PROF_SM_ACTIVE", 'g'),
    ("DCGM_FI_PROF_SM_CYCLES_ACTIVE_TOTAL", 'c'),
    ("DCGM_FI_PROF_SM_CYCLES_ELAPSED_TOTAL", 'c'),
    ("DCGM_FI_PROF_SM_OCCUPANCY", 'g'),
];

/// `KNOWN_FIELDS.get(field)`.
#[must_use]
pub fn known_field(field: &str) -> Option<char> {
    KNOWN_FIELDS
        .iter()
        .find(|(f, _)| *f == field)
        .map(|(_, t)| *t)
}

/// Label-only fields never appear as families of their own.
fn is_known_label_family(f: &str) -> bool {
    known_field(f) == Some('l')
}

/// (family, key, unit, scale): utilisation percentages become ratios, MiB become B.
pub const GPU_METRICS: [(&str, &str, &str, f64); 15] = [
    ("DCGM_FI_DEV_GPU_TEMP", "dcgm.gpu_temp_c", "C", 1.0),
    ("DCGM_FI_DEV_POWER_USAGE", "dcgm.power_w", "W", 1.0),
    ("DCGM_FI_DEV_GPU_UTIL", "dcgm.gpu_util", "ratio", 0.01),
    ("DCGM_FI_DEV_MEM_COPY_UTIL", "dcgm.mem_util", "ratio", 0.01),
    ("DCGM_FI_DEV_FB_USED", "dcgm.fb_used_b", "B", 1_048_576.0),
    ("DCGM_FI_DEV_FB_FREE", "dcgm.fb_free_b", "B", 1_048_576.0),
    ("DCGM_FI_DEV_SM_CLOCK", "dcgm.sm_clock_mhz", "MHz", 1.0),
    ("DCGM_FI_DEV_XID_ERRORS", "dcgm.xid_last", "1", 1.0),
    ("DCGM_EXP_XID_ERRORS_TOTAL", "dcgm.xid_total", "1", 1.0),
    (
        "DCGM_FI_DEV_PCIE_REPLAY_COUNTER",
        "dcgm.pcie_replay_total",
        "1",
        1.0,
    ),
    (
        "DCGM_FI_PROF_GR_ENGINE_ACTIVE",
        "dcgm.gr_active",
        "ratio",
        1.0,
    ),
    (
        "DCGM_FI_PROF_PIPE_TENSOR_ACTIVE",
        "dcgm.tensor_active",
        "ratio",
        1.0,
    ),
    ("DCGM_FI_PROF_DRAM_ACTIVE", "dcgm.dram_active", "ratio", 1.0),
    ("DCGM_FI_PROF_PCIE_TX_BYTES", "dcgm.pcie_tx_bps", "B/s", 1.0),
    ("DCGM_FI_PROF_PCIE_RX_BYTES", "dcgm.pcie_rx_bps", "B/s", 1.0),
];

/// `{family: [(labels, value)]}` in first-appearance order (a Python dict).
pub type Samples = IndexMap<String, Vec<(IndexMap<String, String>, f64)>>;

/// Prometheus text format -> samples. Comments, NaN/Inf and malformed lines are skipped.
#[must_use]
pub fn parse_exposition(text: &str) -> Samples {
    let mut out = Samples::new();
    let (Some(sample_re), Some(label_re)) = (SAMPLE_RE.as_ref(), LABEL_RE.as_ref()) else {
        return out;
    };
    for line in pystr::splitlines(text) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(m) = sample_re.captures(line) else {
            continue;
        };
        let Some(value) = m.get(3).and_then(|v| wsb::py_float(v.as_str())) else {
            continue;
        };
        if !value.is_finite() {
            continue;
        }
        let mut labels: IndexMap<String, String> = IndexMap::new();
        for c in label_re.captures_iter(m.get(2).map_or("", |x| x.as_str())) {
            let k = c.get(1).map_or("", |x| x.as_str()).to_owned();
            let v = c
                .get(2)
                .map_or("", |x| x.as_str())
                .replace("\\\"", "\"")
                .replace("\\\\", "\\");
            labels.insert(k, v);
        }
        out.entry(m.get(1).map_or("", |x| x.as_str()).to_owned())
            .or_default()
            .push((labels, value));
    }
    out
}

/// `...dcgm-exporter:4.6.1-4.8.4[@sha256:..]` -> "4.8.4" (the exporter part).
#[must_use]
pub fn exporter_version(image: Option<&str>) -> Option<String> {
    IMAGE_TAG_RE
        .as_ref()?
        .captures(image.unwrap_or(""))
        .and_then(|c| c.get(2))
        .map(|m| m.as_str().to_owned())
}

/// `tuple(int(p) for p in v.split(".") if p.isdigit())`.
#[must_use]
pub fn version_tuple(v: &str) -> Vec<i64> {
    v.split('.')
        .filter(|p| pystr::is_ascii_digits(p))
        .map(|p| p.parse::<i64>().unwrap_or(i64::MAX))
        .collect()
}

/// Display name for text: "RTX PRO 6000 Blackwell Max-Q Workstation".
#[must_use]
pub fn short_model(name: &str) -> String {
    let n = name.strip_prefix("NVIDIA ").unwrap_or(name);
    let n = n.strip_suffix(" Edition").unwrap_or(n);
    pystr::head(&sub(&MODEL_STRIP_RE, n, ""), 40).to_owned()
}

/// Label values must fit the metrics key charset, or the series can never be charted.
#[must_use]
pub fn clean_label(v: &str) -> String {
    let s = sub(&DASHES_RE, &sub(&LABEL_BAD_RE, pystr::strip(v), "-"), "-");
    pystr::head(&s, 40).trim_matches('-').to_owned()
}

fn diag(severity: Severity, line: Option<usize>, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        severity,
        message: message.into(),
        line: line.and_then(|l| i64::try_from(l).ok()),
    }
}

/// Every non-comment, non-blank line must be `FIELD, type, help`. Returns ([(line, FIELD, type)], diagnostics).
#[must_use]
pub fn parse_counters(content: &str) -> (Vec<(usize, String, String)>, Vec<Diagnostic>) {
    let mut rows: Vec<(usize, String, String)> = Vec::new();
    let mut diags: Vec<Diagnostic> = Vec::new();
    let mut seen: IndexMap<String, usize> = IndexMap::new();
    let names: Vec<&str> = KNOWN_FIELDS.iter().map(|(f, _)| *f).collect();
    for (i, raw) in pystr::splitlines(content).into_iter().enumerate() {
        let no = i + 1;
        let line = pystr::strip(raw);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, ',').map(pystr::strip).collect();
        if parts.len() < 3 {
            diags.push(diag(
                Severity::Error,
                Some(no),
                "expected `FIELD, type, help text` (three comma-separated columns)",
            ));
            continue;
        }
        let (field, help_text) = (parts[0], parts[2]);
        let mtype = parts[1].to_lowercase();
        if !full(&FIELD_RE, field) {
            diags.push(diag(
                Severity::Error,
                Some(no),
                format!("{} is not a field name", str_repr(pystr::head(field, 60))),
            ));
            continue;
        }
        if !matches!(mtype.as_str(), "gauge" | "counter" | "label") {
            diags.push(diag(
                Severity::Error,
                Some(no),
                format!(
                    "type {} must be one of gauge, counter, label",
                    str_repr(pystr::head(&mtype, 20))
                ),
            ));
            continue;
        }
        if help_text.is_empty() {
            diags.push(diag(
                Severity::Warn,
                Some(no),
                format!("{field} has an empty help text"),
            ));
        }
        if let Some(first) = seen.get(field) {
            diags.push(diag(
                Severity::Error,
                Some(no),
                format!("{field} is already configured on line {first}"),
            ));
            continue;
        }
        seen.insert(field.to_owned(), no);
        match known_field(field) {
            None => {
                let close = difflib::get_close_matches(field, &names, 1, 0.85);
                if field.starts_with("dcgm_") {
                    diags.push(diag(
                        Severity::Error,
                        Some(no),
                        format!("{field} is a 1.x-compatibility name; this exporter profile uses DCGM_FI_* / DCGM_EXP_* names"),
                    ));
                } else if let Some(c) = close.first() {
                    diags.push(diag(
                        Severity::Error,
                        Some(no),
                        format!("unknown field {field}; did you mean {c}?"),
                    ));
                } else {
                    diags.push(diag(
                        Severity::Warn,
                        Some(no),
                        format!(
                            "{field} is not in the field lists shipped in the exporter image; if it is a newer DCGM field this is fine, otherwise the exporter will refuse it"
                        ),
                    ));
                }
            }
            Some(known) => {
                if (known == 'l') != (mtype == "label") {
                    let kind = if known == 'l' { "label" } else { "metric" };
                    diags.push(diag(
                        Severity::Warn,
                        Some(no),
                        format!("{field} is a {kind} field in the shipped lists but is configured as {mtype}"),
                    ));
                }
            }
        }
        if field == "DCGM_FI_DEV_SERIAL" {
            diags.push(diag(
                Severity::Warn,
                Some(no),
                "the GPU serial number would be attached to every series and stored by Prometheus",
            ));
        }
        rows.push((no, field.to_owned(), mtype));
    }
    if !rows.iter().any(|(_, _, t)| t != "label") {
        diags.push(diag(
            Severity::Error,
            None,
            "no active gauge/counter line: the exporter would serve no GPU metric (custom files REPLACE the default set)",
        ));
    }
    (rows, diags)
}

fn as_obj(v: Option<&Value>) -> Option<&Map<String, Value>> {
    v.and_then(Value::as_object)
}

fn as_list(v: Option<&Value>) -> &[Value] {
    v.and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// Recursively drop serial-number keys and mask long digit runs / GPU UUIDs in strings.
#[must_use]
pub fn strip_serials(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(k, _)| !k.to_lowercase().contains("serial"))
                .map(|(k, x)| (k.clone(), strip_serials(x)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(strip_serials).collect()),
        Value::String(s) => Value::String(sub(&LONG_DIGITS_RE, &mask_uuids(s), "[id]")),
        other => other.clone(),
    }
}

/// (verdict, [(name, status)], warning texts) from `dcgmi diag -j`. verdict: pass | warn | fail | error.
#[must_use]
pub fn summarize_diag(doc: &Value) -> (String, Vec<(String, String)>, Vec<String>) {
    let mut tests: Vec<(String, String)> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let empty = Map::new();
    let top = as_obj(doc.get("DCGM Diagnostic"));
    for cat in as_list(top.and_then(|t| t.get("test_categories"))) {
        for t in as_list(as_obj(Some(cat)).and_then(|c| c.get("tests"))) {
            let td = t.as_object().unwrap_or(&empty);
            let summary = as_obj(td.get("test_summary"));
            let mut status: Option<String> = summary
                .and_then(|s| s.get("status"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let results: Vec<&Map<String, Value>> = as_list(td.get("results"))
                .iter()
                .map(|r| r.as_object().unwrap_or(&empty))
                .collect();
            for w in as_list(summary.and_then(|s| s.get("warnings"))) {
                if let Some(text) = as_obj(Some(w))
                    .and_then(|o| o.get("warning"))
                    .and_then(Value::as_str)
                {
                    notes.push(pystr::head(text, 300).to_owned());
                }
            }
            if status.is_none() {
                let statuses: Vec<String> = results
                    .iter()
                    .filter(|r| wsb::json_truthy(r.get("status")))
                    .map(|r| wsb::json_get_str(r.get("status")))
                    .collect();
                status = Some(
                    statuses
                        .iter()
                        .find(|s| s.to_lowercase().starts_with("fail"))
                        .or_else(|| {
                            statuses
                                .iter()
                                .find(|s| s.to_lowercase().starts_with("warn"))
                        })
                        .or_else(|| statuses.first())
                        .cloned()
                        .unwrap_or_else(|| "Unknown".to_owned()),
                );
            }
            let name = match td.get("name") {
                Some(v) => wsb::json_str(v),
                None => "?".to_owned(),
            };
            tests.push((
                pystr::head(&name, 60).to_owned(),
                pystr::head(&status.unwrap_or_default(), 20).to_owned(),
            ));
            for r in &results {
                for key in ["warnings", "info"] {
                    for w in as_list(r.get(key)) {
                        let wd = w.as_object();
                        let pick = |k: &str| {
                            wd.and_then(|o| o.get(k))
                                .filter(|v| wsb::json_truthy(Some(v)))
                        };
                        let text = pick("warning")
                            .or_else(|| pick("info"))
                            .or(if w.is_string() { Some(w) } else { None });
                        if let Some(Value::String(t)) = text
                            && key == "warnings"
                        {
                            notes.push(pystr::head(t, 300).to_owned());
                        }
                    }
                }
            }
        }
    }
    let low: Vec<String> = tests.iter().map(|(_, s)| s.to_lowercase()).collect();
    let verdict = if tests.is_empty() {
        "error"
    } else if low.iter().any(|s| s.starts_with("fail")) {
        "fail"
    } else if low.iter().any(|s| s.starts_with("warn")) || !notes.is_empty() {
        "warn"
    } else {
        "pass"
    };
    (verdict.to_owned(), tests, notes)
}

/// Intervals the tests shorten (Python instance attributes).
#[derive(Debug, Clone, Copy)]
pub struct DcgmTiming {
    pub follow_interval: f64,
    /// post_apply / restart `/metrics` poll
    pub poll_interval: f64,
    pub apply_timeout: f64,
    /// let the exporter's file watcher (200 ms debounce) see an edit
    pub settle: f64,
}

impl Default for DcgmTiming {
    fn default() -> Self {
        DcgmTiming {
            follow_interval: 5.0,
            poll_interval: 2.0,
            apply_timeout: 60.0,
            settle: 1.5,
        }
    }
}

/// The deployment names (Settings: dcgm_exporter_container, dcgm_diag_image, dcgm_prometheus_jobs,
/// dcgm_compose_project).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DcgmNames {
    pub exporter_container: String,
    pub diag_image: String,
    pub prometheus_jobs: Vec<String>,
    pub compose_project: String,
    pub compose_service: String,
}

impl Default for DcgmNames {
    fn default() -> Self {
        DcgmNames {
            exporter_container: DCGM_EXPORTER_CONTAINER.to_owned(),
            diag_image: DCGM_DIAG_IMAGE.to_owned(),
            prometheus_jobs: DCGM_PROMETHEUS_JOBS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            compose_project: DCGM_COMPOSE_PROJECT.to_owned(),
            compose_service: COMPOSE_SERVICE.to_owned(),
        }
    }
}

#[derive(Default)]
struct St {
    gpus: IndexMap<String, String>,
    baseline: HashSet<String>,
    freeze_until: Option<Instant>,
    xid_seen: IndexMap<String, f64>,
    events: VecDeque<(u64, SystemEvent)>,
    seq: u64,
    image_cache: Option<(Instant, bool)>,
}

pub struct DcgmAdapter {
    ctx: AdapterContext,
    names: Mutex<DcgmNames>,
    timing: Mutex<DcgmTiming>,
    st: Mutex<St>,
    diag_lock: tokio::sync::Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

type Scrape = (Option<Samples>, String);

impl DcgmAdapter {
    #[must_use]
    pub fn new(ctx: AdapterContext) -> Self {
        DcgmAdapter {
            ctx,
            names: Mutex::new(DcgmNames::default()),
            timing: Mutex::new(DcgmTiming::default()),
            st: Mutex::new(St::default()),
            diag_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// What the container's registry builds (A-7): `DcgmAdapter(ctx)` configured from the settings.
    #[must_use]
    pub fn from_settings(ctx: &AdapterContext, cfg: &Settings) -> Self {
        let a = DcgmAdapter::new(ctx.clone());
        a.configure(
            &cfg.dcgm_exporter_container,
            &cfg.dcgm_diag_image,
            &cfg.dcgm_prometheus_jobs,
            Some(&cfg.dcgm_compose_project),
        );
        a
    }

    /// `configure(exporter_container=, diag_image=, prometheus_jobs=, compose_project=)`.
    pub fn configure(
        &self,
        exporter_container: &str,
        diag_image: &str,
        prometheus_jobs: &[String],
        compose_project: Option<&str>,
    ) {
        let mut n = lock(&self.names);
        exporter_container.clone_into(&mut n.exporter_container);
        if let Some(p) = compose_project {
            p.clone_into(&mut n.compose_project);
        }
        diag_image.clone_into(&mut n.diag_image);
        n.prometheus_jobs = prometheus_jobs.to_vec();
    }

    #[must_use]
    pub fn names(&self) -> DcgmNames {
        lock(&self.names).clone()
    }

    pub fn set_timing(&self, t: DcgmTiming) {
        *lock(&self.timing) = t;
    }

    #[must_use]
    pub fn timing(&self) -> DcgmTiming {
        *lock(&self.timing)
    }

    /// The families the last scrape/apply saw (the rollback yardstick).
    #[must_use]
    pub fn baseline(&self) -> HashSet<String> {
        lock(&self.st).baseline.clone()
    }

    /// GPU index -> short model, from the last good scrape.
    #[must_use]
    pub fn gpus(&self) -> IndexMap<String, String> {
        lock(&self.st).gpus.clone()
    }

    #[must_use]
    pub fn ctx(&self) -> &AdapterContext {
        &self.ctx
    }

    // ── helpers ─────────────────────────────────────────────────────────────────────────────
    fn exporter_url(&self) -> String {
        self.ctx
            .urls
            .get("dcgm")
            .map_or("http://127.0.0.1:9400", String::as_str)
            .trim_end_matches('/')
            .to_owned()
    }

    fn prom_url(&self) -> String {
        self.ctx
            .urls
            .get("prometheus")
            .map_or("http://127.0.0.1:9090", String::as_str)
            .trim_end_matches('/')
            .to_owned()
    }

    fn runs_dir(&self) -> PathBuf {
        self.ctx.state_dir.join("dcgm").join("runs")
    }

    fn jobs_regex(&self) -> String {
        let n = lock(&self.names);
        n.prometheus_jobs
            .iter()
            .filter(|j| full(&JOB_RE, j))
            .cloned()
            .collect::<Vec<_>>()
            .join("|")
    }

    async fn run(&self, argv: &[String], timeout: f64, max_bytes: usize) -> CommandResult {
        wsb::run(&self.ctx, argv, wsb::opts(timeout, max_bytes)).await
    }

    fn push(st: &mut St, ev: SystemEvent) {
        st.seq += 1;
        let seq = st.seq;
        st.events.push_back((seq, ev));
        while st.events.len() > 200 {
            st.events.pop_front();
        }
    }

    // ── exporter /metrics ───────────────────────────────────────────────────────────────────
    async fn scrape(&self) -> Scrape {
        let resp = match self
            .ctx
            .http
            .get(&format!("{}/metrics", self.exporter_url()), 5.0)
            .await
        {
            Ok(r) => r,
            Err(e) => return (None, e.class.to_owned()),
        };
        if resp.status != 200 {
            return (None, format!("HTTP {}", resp.status));
        }
        if resp.body.len() > MAX_METRICS_BYTES {
            return (None, "response too large".to_owned());
        }
        (Some(parse_exposition(&resp.text())), String::new())
    }

    /// Learn the GPUs and the baseline family set, and turn XID movement into events. Idempotent per scrape.
    fn observe(&self, samples: &Samples) {
        let mut st = lock(&self.st);
        let mut gpus: IndexMap<String, String> = IndexMap::new();
        for (fam, rows) in samples {
            if fam.starts_with("DCGM_FI_DEV_") {
                for (labels, _) in rows {
                    let g = labels.get("gpu").map_or("", String::as_str);
                    if pystr::is_ascii_digits(g) && g.chars().count() <= 3 && gpus.len() < MAX_GPUS
                    {
                        gpus.insert(
                            g.to_owned(),
                            short_model(labels.get("modelName").map_or("", String::as_str)),
                        );
                    }
                }
            }
        }
        if !gpus.is_empty() {
            st.gpus = gpus;
        }
        if st.freeze_until.is_none_or(|t| Instant::now() >= t) {
            let fams: HashSet<String> = samples
                .keys()
                .filter(|f| {
                    f.starts_with("DCGM_FI_DEV_")
                        || f.starts_with("DCGM_FI_PROF_")
                        || f.starts_with("DCGM_EXP_")
                })
                .cloned()
                .collect();
            if !fams.is_empty() {
                st.baseline = fams;
            }
        }
        for (labels, v) in samples
            .get("DCGM_FI_DEV_XID_ERRORS")
            .map_or(&[][..], Vec::as_slice)
        {
            let g = labels.get("gpu").map_or("", String::as_str).to_owned();
            if !pystr::is_ascii_digits(&g) {
                continue;
            }
            if *v > 0.0 && st.xid_seen.get(&g) != Some(v) {
                let xid = wsb::py_int_of(*v);
                let ev = SystemEvent {
                    ts: now_ts(),
                    system: SystemId::Dcgm,
                    kind: EVENT_XID.to_owned(),
                    summary: format!("GPU {g}: XID {xid}"),
                    data: obj(json!({"gpu": g, "xid": xid, "source": "last_xid"})),
                };
                Self::push(&mut st, ev);
            }
            st.xid_seen.insert(g, *v);
        }
        for (labels, v) in samples
            .get("DCGM_EXP_XID_ERRORS_TOTAL")
            .map_or(&[][..], Vec::as_slice)
        {
            let g = labels.get("gpu").map_or("", String::as_str).to_owned();
            if !pystr::is_ascii_digits(&g) {
                continue;
            }
            let key = format!("total:{g}");
            let prev = st.xid_seen.get(&key).copied().unwrap_or(0.0);
            if *v > prev {
                let (total, new) = (wsb::py_int_of(*v), wsb::py_int_of(*v - prev));
                let ev = SystemEvent {
                    ts: now_ts(),
                    system: SystemId::Dcgm,
                    kind: EVENT_XID.to_owned(),
                    summary: format!("GPU {g}: {new} new XID error(s), {total} in total"),
                    data: obj(
                        json!({"gpu": g, "xid_total": total, "new": new, "source": "xid_total"}),
                    ),
                };
                Self::push(&mut st, ev);
            }
            st.xid_seen.insert(key, *v);
        }
    }

    fn xid_nonzero(&self) -> IndexMap<String, f64> {
        let st = lock(&self.st);
        let mut out: IndexMap<String, f64> = IndexMap::new();
        for (g, v) in &st.xid_seen {
            if !g.starts_with("total:") && *v > 0.0 {
                out.insert(g.clone(), *v);
            }
        }
        for (g, v) in &st.xid_seen {
            if let Some(gpu) = g.strip_prefix("total:")
                && *v > 0.0
            {
                out.insert(gpu.to_owned(), *v);
            }
        }
        out
    }

    // ── Prometheus (loopback only) ──────────────────────────────────────────────────────────
    async fn prom(&self, expr: &str) -> Option<Vec<Map<String, Value>>> {
        let url = format!("{}/api/v1/query", self.prom_url());
        let resp = self
            .ctx
            .http
            .get_query(&url, &[("query", expr)], 5.0)
            .await
            .ok()?;
        if resp.status != 200 {
            return None;
        }
        let d = resp.json().ok()?;
        let d = d.as_object()?;
        if d.get("status").and_then(Value::as_str) != Some("success") {
            return None;
        }
        let res = as_obj(d.get("data")).and_then(|x| x.get("result"));
        Some(
            as_list(res)
                .iter()
                .filter_map(|r| r.as_object().cloned())
                .take(200)
                .collect(),
        )
    }

    fn host_of(metric: Option<&Map<String, Value>>) -> (String, String) {
        let job_raw = match metric.and_then(|m| m.get("job")) {
            Some(v) => wsb::json_str(v),
            None => "?".to_owned(),
        };
        let mut job = clean_label(&job_raw);
        if job.is_empty() {
            "?".clone_into(&mut job);
        }
        let host = match metric.and_then(|m| m.get("host")) {
            Some(Value::String(h)) if !h.is_empty() => clean_label(h),
            _ => {
                if job == "dcgm" {
                    "local".to_owned()
                } else {
                    job.clone()
                }
            }
        };
        (job, host)
    }

    fn val(row: &Map<String, Value>) -> Option<f64> {
        let v = row
            .get("value")
            .and_then(Value::as_array)
            .and_then(|a| a.get(1))
            .and_then(wsb::json_float)?;
        v.is_finite().then_some(v)
    }

    async fn hosts_up(&self) -> Option<Vec<(String, String, bool)>> {
        let jobs = self.jobs_regex();
        if jobs.is_empty() {
            return Some(Vec::new());
        }
        let rows = self.prom(&format!("up{{job=~\"{jobs}\"}}")).await?;
        let mut out = Vec::new();
        for r in rows.iter().take(MAX_HOSTS) {
            if let Some(v) = Self::val(r) {
                let (job, host) = Self::host_of(r.get("metric").and_then(Value::as_object));
                out.push((job, host, v >= 1.0));
            }
        }
        out.sort();
        Some(out)
    }

    // ── docker ──────────────────────────────────────────────────────────────────────────────
    /// (state, image without digest, detail). state: running|stopped|failed|absent|unknown.
    async fn inspect(&self) -> (String, Option<String>, String) {
        let container = lock(&self.names).exporter_container.clone();
        let fmt = "{{.State.Status}}|{{.State.Running}}|{{.Config.Image}}|{{.State.StartedAt}}|{{.State.ExitCode}}";
        let r = self
            .run(
                &wsb::argv(&["docker", "inspect", "--format", fmt, &container]),
                10.0,
                8192,
            )
            .await;
        if r.returncode != 0 {
            let low = format!("{}{}", r.stderr, r.stdout).to_lowercase();
            let state = if low.contains("no such") {
                "absent"
            } else {
                "unknown"
            };
            return (
                state.to_owned(),
                None,
                pystr::head(&redact(pystr::strip(&r.stderr)), 160).to_owned(),
            );
        }
        let out = pystr::strip(&r.stdout).to_owned();
        let parts: Vec<&str> = out.split('|').collect();
        if parts.len() < 5 {
            return (
                "unknown".to_owned(),
                None,
                "unexpected docker inspect output".to_owned(),
            );
        }
        let (status, running, image, started, code) =
            (parts[0], parts[1], parts[2], parts[3], parts[4]);
        let image = image.split('@').next().unwrap_or("").to_owned();
        if running == "true" {
            return (
                "running".to_owned(),
                Some(image),
                format!("started {}", pystr::head(started, 19)),
            );
        }
        if status == "restarting"
            || status == "dead"
            || (status == "exited" && !(code == "0" || code.is_empty()))
        {
            return (
                "failed".to_owned(),
                Some(image),
                format!("{status}, exit code {code}"),
            );
        }
        ("stopped".to_owned(), Some(image), status.to_owned())
    }

    /// None when the configured container is the observability project's dcgm-exporter service (compose labels);
    /// otherwise why a restart is refused.
    async fn identity_problem(&self) -> Option<String> {
        let n = self.names();
        let fmt = r#"{{index .Config.Labels "com.docker.compose.project"}}|{{index .Config.Labels "com.docker.compose.service"}}"#;
        let r = self
            .run(
                &wsb::argv(&["docker", "inspect", "--format", fmt, &n.exporter_container]),
                10.0,
                4096,
            )
            .await;
        if r.returncode != 0 {
            return Some(format!(
                "cannot inspect {}: {}",
                n.exporter_container,
                pystr::head(&redact(pystr::strip(&r.stderr)), 160)
            ));
        }
        let got = pystr::strip(&r.stdout);
        let want = format!("{}|{}", n.compose_project, n.compose_service);
        if got != want {
            return Some(format!(
                "refusing to restart {}: its compose labels are {}, expected {}",
                n.exporter_container,
                str_repr(got),
                str_repr(&want)
            ));
        }
        None
    }

    fn compose_fix(&self) -> String {
        match self.ctx.paths.get("observability_compose") {
            Some(c) => format!(
                "docker compose -f {} --profile gpu up -d dcgm-exporter",
                c.display()
            ),
            None => {
                "cd observability && docker compose --profile gpu up -d dcgm-exporter".to_owned()
            }
        }
    }

    fn bump_fix(&self) -> String {
        format!(
            "set DCGM_VERSION={FIXED_TAG}@sha256:<digest> in observability/.env (the exact line is in observability/.env.example), then: {}",
            self.compose_fix()
        )
    }

    // ── observe ─────────────────────────────────────────────────────────────────────────────
    async fn diag_image_present(&self) -> bool {
        if let Some((ts, present)) = lock(&self.st).image_cache
            && ts.elapsed() < Duration::from_secs_f64(PROM_TTL_IMAGE)
        {
            return present;
        }
        let image = lock(&self.names).diag_image.clone();
        let r = self
            .run(
                &wsb::argv(&["docker", "image", "inspect", "--format", "{{.Id}}", &image]),
                10.0,
                4096,
            )
            .await;
        let present = r.returncode == 0 || r.returncode == 127; // 127: no docker at all -> not this problem
        lock(&self.st).image_cache = Some((Instant::now(), present));
        present
    }

    async fn status_impl(&self) -> SystemStatus {
        let now = now_ts();
        let ((state, image, detail), (samples, scrape_err), hosts) =
            tokio::join!(self.inspect(), self.scrape(), self.hosts_up());
        if let Some(s) = &samples {
            self.observe(s);
        }
        let names = self.names();
        let mut problems: Vec<Problem> = Vec::new();
        let mut comps: Vec<Component> = Vec::new();
        let ver = exporter_version(image.as_deref());
        comps.push(Component {
            kind: "container".into(),
            name: names.exporter_container.clone(),
            state: state.clone(),
            image: image.clone(),
            detail: Some(format!(
                "{}{detail}",
                ver.as_ref()
                    .map(|v| format!("exporter {v}; "))
                    .unwrap_or_default()
            )),
        });
        comps.push(Component {
            kind: "http".into(),
            name: "exporter /metrics".into(),
            state: if samples.is_some() {
                "running"
            } else {
                "stopped"
            }
            .into(),
            image: None,
            detail: Some(if samples.is_some() {
                "answers".to_owned()
            } else {
                scrape_err.clone()
            }),
        });
        if samples.is_none() {
            problems.push(Problem::new(
                Severity::Error,
                "dcgm.exporter.down",
                format!("the exporter at {}/metrics does not answer ({scrape_err}); container state: {state}", self.exporter_url()),
                Some(self.compose_fix()),
            ));
        }
        if state == "running" && image.as_deref().is_some_and(|i| !i.is_empty()) && ver.is_none() {
            problems.push(Problem::new(
                Severity::Info,
                "dcgm.exporter.version_unknown",
                format!("cannot read the exporter version from the image tag {}", str_repr(image.as_deref().unwrap_or(""))),
                Some(format!("pin the tag as <dcgm version>-<exporter version>, e.g. {FIXED_TAG}, via DCGM_VERSION in observability/.env")),
            ));
        }
        if let Some(v) = &ver
            && version_tuple(v) < FIXED_EXPORTER.to_vec()
        {
            problems.push(Problem::new(
                Severity::Error,
                "dcgm.exporter.vulnerable",
                format!("dcgm-exporter {v} is older than 4.8.3: CVE-2026-47483, /debug/pprof is served unauthenticated"),
                Some(self.bump_fix()),
            ));
        }
        if samples.is_some()
            && let Ok(resp) = self
                .ctx
                .http
                .get(&format!("{}/debug/pprof/", self.exporter_url()), 5.0)
                .await
            && resp.status == 200
        {
            problems.push(Problem::new(
                Severity::Error,
                "dcgm.exporter.pprof_exposed",
                "GET /debug/pprof/ answers 200 on the exporter: heap and goroutine dumps are readable by anything that reaches the port",
                Some(self.bump_fix()),
            ));
        }
        let gpu_n = if samples.is_some() {
            lock(&self.st).gpus.len()
        } else {
            0
        };
        if samples.is_some() && gpu_n == 0 {
            problems.push(Problem::new(
                Severity::Warn,
                "dcgm.gpus.none",
                "the exporter answers but reports no GPU",
                Some(format!(
                    "docker logs --tail 50 {}",
                    names.exporter_container
                )),
            ));
        }
        let xids = if samples.is_some() {
            self.xid_nonzero()
        } else {
            IndexMap::new()
        };
        if !xids.is_empty() {
            let mut items: Vec<(&String, &f64)> = xids.iter().collect();
            items.sort_by(|a, b| a.0.cmp(b.0));
            let listing = items
                .iter()
                .map(|(g, v)| format!("GPU {g}: {}", wsb::py_int_of(**v)))
                .collect::<Vec<_>>()
                .join(", ");
            problems.push(Problem::new(
                Severity::Warn,
                "dcgm.xid.nonzero",
                format!("XID errors reported ({listing})"),
                Some("journalctl -k | grep -i 'NVRM: Xid'   # then look the code up in NVIDIA's XID catalog".to_owned()),
            ));
        }
        let mut up_n = 0usize;
        match &hosts {
            None => {
                comps.push(Component {
                    kind: "http".into(),
                    name: "prometheus".into(),
                    state: "stopped".into(),
                    image: None,
                    detail: Some("query failed".into()),
                });
                problems.push(Problem::new(
                    Severity::Warn,
                    "dcgm.prometheus.unreachable",
                    format!(
                        "Prometheus at {} did not answer; other hosts' GPUs cannot be listed",
                        self.prom_url()
                    ),
                    Some(
                        "docker compose -f observability/docker-compose.yml up -d prometheus"
                            .to_owned(),
                    ),
                ));
            }
            Some(hosts) => {
                comps.push(Component {
                    kind: "http".into(),
                    name: "prometheus".into(),
                    state: "running".into(),
                    image: None,
                    detail: Some("answers".into()),
                });
                let mut down: Vec<String> = Vec::new();
                for (job, host, up) in hosts {
                    comps.push(Component {
                        kind: "http".into(),
                        name: format!("{host} ({job})"),
                        state: if *up { "running" } else { "stopped" }.into(),
                        image: None,
                        detail: Some(
                            if *up {
                                "scraped by Prometheus"
                            } else {
                                "scrape failing (up == 0)"
                            }
                            .into(),
                        ),
                    });
                    up_n += usize::from(*up);
                    if !*up {
                        down.push(format!("{host} ({job})"));
                    }
                }
                let seen_jobs: HashSet<&String> = hosts.iter().map(|(j, _, _)| j).collect();
                for job in &names.prometheus_jobs {
                    if !seen_jobs.contains(job) && full(&JOB_RE, job) {
                        down.push(format!("{job} (no target)"));
                    }
                }
                if !down.is_empty() {
                    problems.push(Problem::new(
                        Severity::Warn,
                        "dcgm.host.down",
                        format!(
                            "{} DCGM target(s) are not being scraped: {}",
                            down.len(),
                            down.iter().take(8).cloned().collect::<Vec<_>>().join(", ")
                        ),
                        Some(
                            "open http://127.0.0.1:9090/targets?search=dcgm for the scrape error; a powered-off host is expected to be listed here"
                                .to_owned(),
                        ),
                    ));
                }
            }
        }
        if samples.is_some() && !self.diag_image_present().await {
            problems.push(Problem::new(
                Severity::Info,
                "dcgm.diag.image_missing",
                "the diagnostics image is not present locally; diagnostics need it (runs use --pull never)",
                Some(format!("docker pull {}", names.diag_image)),
            ));
        }
        let health = if samples.is_none() {
            Health::Down
        } else if problems
            .iter()
            .any(|p| matches!(p.severity, Severity::Error | Severity::Warn))
        {
            Health::Degraded
        } else {
            Health::Up
        };
        let total = hosts.as_ref().map_or(0, Vec::len);
        let mut summary = if samples.is_some() {
            format!("{gpu_n} GPU(s) local")
        } else {
            "exporter not answering".to_owned()
        };
        if hosts.is_some() {
            summary.push_str(&format!(" · {up_n}/{total} DCGM hosts scraped"));
        } else {
            summary.push_str(" · Prometheus unreachable");
        }
        if let Some(v) = ver.as_ref().filter(|v| !v.is_empty()) {
            summary.push_str(&format!(" · exporter {v}"));
        }
        SystemStatus {
            system: SystemId::Dcgm,
            health,
            version: ver,
            summary,
            endpoints: vec![
                Endpoint {
                    name: "exporter metrics".into(),
                    url: format!("{}/metrics", self.exporter_url()),
                    role: "metrics".into(),
                    auth: "none".into(),
                },
                Endpoint {
                    name: "prometheus".into(),
                    url: self.prom_url(),
                    role: "api".into(),
                    auth: "none".into(),
                },
            ],
            components: comps,
            problems,
            checked_at: now,
        }
    }

    async fn metrics_impl(&self) -> MetricsSnapshot {
        let now = now_ts();
        let jobs = self.jobs_regex();
        let sel = if jobs.is_empty() {
            None
        } else {
            Some(format!("{{job=~\"{jobs}\"}}"))
        };
        let (samples, err) = self.scrape().await;
        let mut pts: Vec<MetricPoint> = Vec::new();
        if let Some(s) = &samples {
            self.observe(s);
            for (fam, key, unit, scale) in GPU_METRICS {
                for (labels, v) in s.get(fam).map_or(&[][..], Vec::as_slice) {
                    let g = labels.get("gpu").map_or("", String::as_str);
                    if !pystr::is_ascii_digits(g) || g.chars().count() > 3 {
                        continue;
                    }
                    let mut l = IndexMap::new();
                    l.insert("gpu".to_owned(), g.to_owned());
                    l.insert(
                        "model".to_owned(),
                        clean_label(&short_model(
                            labels.get("modelName").map_or("", String::as_str),
                        )),
                    );
                    pts.push(MetricPoint {
                        key: key.to_owned(),
                        value: v * scale,
                        unit: unit.to_owned(),
                        labels: l,
                    });
                }
            }
        }
        let mut fleet_err = String::new();
        if let Some(sel) = &sel {
            let (q_cnt, q_max, q_pwr) = (
                format!("count by (job, host) (DCGM_FI_DEV_GPU_TEMP{sel})"),
                format!("max by (job, host) (DCGM_FI_DEV_GPU_TEMP{sel})"),
                format!("sum by (job, host) (DCGM_FI_DEV_POWER_USAGE{sel})"),
            );
            let (up, cnt, tmax, pwr) = tokio::join!(
                self.hosts_up(),
                self.prom(&q_cnt),
                self.prom(&q_max),
                self.prom(&q_pwr)
            );
            if up.is_none() {
                "Prometheus did not answer; no fleet rollups".clone_into(&mut fleet_err);
            }
            for (job, host, ok) in up.unwrap_or_default() {
                pts.push(point(
                    "dcgm.host_up",
                    if ok { 1.0 } else { 0.0 },
                    "1",
                    &host,
                    &job,
                ));
            }
            for (rows, key, unit) in [
                (cnt, "dcgm.fleet_gpus", "1"),
                (tmax, "dcgm.fleet_temp_max_c", "C"),
                (pwr, "dcgm.fleet_power_w", "W"),
            ] {
                for r in rows.unwrap_or_default().iter().take(MAX_HOSTS) {
                    if let Some(val) = Self::val(r) {
                        let (job, host) = Self::host_of(r.get("metric").and_then(Value::as_object));
                        pts.push(point(key, val, unit, &host, &job));
                    }
                }
            }
        }
        if pts.is_empty() {
            let e = format!(
                "exporter: {}; {}",
                if err.is_empty() {
                    "no GPU metrics"
                } else {
                    &err
                },
                if fleet_err.is_empty() {
                    "no fleet data"
                } else {
                    &fleet_err
                }
            );
            return MetricsSnapshot {
                system: SystemId::Dcgm,
                ts: now,
                available: false,
                points: Vec::new(),
                error: Some(pystr::head(&e, 300).to_owned()),
            };
        }
        let error = if samples.is_none() {
            Some(format!(
                "local exporter not answering ({err}); showing the fleet rollups only"
            ))
        } else if !fleet_err.is_empty() {
            Some(fleet_err)
        } else {
            None
        };
        MetricsSnapshot {
            system: SystemId::Dcgm,
            ts: now,
            available: true,
            points: pts,
            error,
        }
    }

    // ── events ──────────────────────────────────────────────────────────────────────────────
    fn diag_dirs(&self) -> Vec<PathBuf> {
        let Ok(rd) = std::fs::read_dir(self.runs_dir()) else {
            return Vec::new();
        };
        let mut dirs: Vec<PathBuf> = rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| full(&RUN_ID_RE, n))
                    && p.is_dir()
            })
            .collect();
        dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        dirs
    }

    fn diag_event(meta: &Map<String, Value>) -> SystemEvent {
        let tests: Vec<&Map<String, Value>> = as_list(meta.get("tests"))
            .iter()
            .filter_map(Value::as_object)
            .collect();
        let passed = tests
            .iter()
            .filter(|t| match t.get("status") {
                Some(v) => wsb::json_str(v).to_lowercase().starts_with("pass"),
                None => false,
            })
            .count();
        let g = |k: &str| wsb::json_get_str(meta.get(k));
        let summary = format!(
            "diag level {} on GPU {}: {} ({passed}/{} tests passed)",
            g("level"),
            g("gpu"),
            g("verdict"),
            tests.len()
        );
        let mut data = Map::new();
        for k in ["run_id", "gpu", "level", "verdict", "duration_ms", "model"] {
            data.insert(k.to_owned(), meta.get(k).cloned().unwrap_or(Value::Null));
        }
        let mut tmap = Map::new();
        for t in tests.iter().take(20) {
            tmap.insert(
                wsb::json_get_str(t.get("name")),
                Value::String(wsb::json_get_str(t.get("status"))),
            );
        }
        data.insert("tests".to_owned(), Value::Object(tmap));
        let ts = match meta.get("ts") {
            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
            Some(Value::Bool(true)) => 1.0,
            _ => 0.0,
        };
        SystemEvent {
            ts,
            system: SystemId::Dcgm,
            kind: EVENT_DIAG.to_owned(),
            summary: pystr::head(&summary, 300).to_owned(),
            data,
        }
    }

    fn diag_events(&self, limit: usize) -> Vec<SystemEvent> {
        self.diag_dirs()
            .iter()
            .take(limit)
            .filter_map(|d| wsb::load_json_dict(&d.join("run.json")))
            .map(|m| Self::diag_event(&m))
            .collect()
    }

    // ── configure ───────────────────────────────────────────────────────────────────────────
    fn counters_spec() -> ConfigFileSpec {
        ConfigFileSpec {
            id: COUNTERS_ID.into(),
            label: "Exporter counters".into(),
            description: "The DCGM fields the exporter collects, one `FIELD, type, help` row each (a custom file REPLACES the default set; commented rows are documentation). The directory is mounted read-only into the exporter, which re-reads the file when it changes.".into(),
            display_path: "observability/dcgm/counters.csv".into(),
            format: "text".into(),
            editable: true,
            why_readonly: None,
            validators: vec![format!(
                "field names and types checked against the lists shipped in the exporter image (default-counters.csv + dcp-metrics-included.csv, {} names)",
                KNOWN_FIELDS.len()
            )],
            authoritative: false,
            apply_effect: "applied by the exporter itself within ~1 s (no restart); with 'restart' the exporter container is restarted (~10 s without metrics). DCGM_FI_PROF_* families return after the first collection cycle (10-20 s). The core rolls the file back if /metrics stops answering or a family that was served disappears.".into(),
        }
    }

    async fn logged_errors(&self, since: f64) -> Vec<String> {
        let stamp = wsb::gmtime(since, false);
        let container = lock(&self.names).exporter_container.clone();
        let r = self
            .run(
                &wsb::argv(&[
                    "docker", "logs", "--since", &stamp, "--tail", "200", &container,
                ]),
                15.0,
                64_000,
            )
            .await;
        if r.returncode != 0 {
            return Vec::new();
        }
        let all = format!("{}\n{}", r.stdout, r.stderr);
        pystr::splitlines(&all)
            .into_iter()
            .filter(|ln| ln.contains("level=ERROR"))
            .map(|ln| pystr::head(&redact(pystr::strip(ln)), 240).to_owned())
            .take(3)
            .collect()
    }

    async fn post_apply_impl(
        &self,
        file_id: &str,
        restart: bool,
    ) -> Result<ApplyResult, AdapterError> {
        let path = self.config_path(file_id)?;
        let began = now_ts() - 2.0;
        let timing = self.timing();
        let mut notes: Vec<String> = Vec::new();
        let mut restarted = false;
        let unhealthy = |restarted: bool, detail: String| ApplyResult {
            ok: false,
            new_sha256: None,
            revision_id: None,
            restarted,
            post_check: PostCheck::Unhealthy,
            detail,
            rolled_back: false,
        };
        if restart {
            if let Some(why) = self.identity_problem().await {
                return Ok(unhealthy(false, why));
            }
            let container = lock(&self.names).exporter_container.clone();
            let r = self
                .run(&wsb::argv(&["docker", "restart", &container]), 90.0, 4096)
                .await;
            if r.returncode != 0 {
                return Ok(unhealthy(
                    false,
                    format!(
                        "docker restart failed: {}",
                        pystr::head(&redact(pystr::strip(&r.stderr)), 200)
                    ),
                ));
            }
            restarted = true;
        } else {
            wsb::sleep_s(timing.settle).await;
        }
        let rows = match std::fs::read(&path) {
            Ok(b) => match String::from_utf8(b) {
                Ok(text) => parse_counters(&text).0,
                Err(_) => {
                    return Err(AdapterError::failed(
                        "UnicodeDecodeError",
                        "'utf-8' codec can't decode the counters file",
                    ));
                }
            },
            Err(_) => Vec::new(),
        };
        let configured: HashSet<String> = rows
            .iter()
            .filter(|(_, _, t)| t != "label")
            .map(|(_, f, _)| f.clone())
            .collect();
        let baseline = self.baseline();
        let required: HashSet<String> = baseline
            .iter()
            .filter(|f| configured.contains(*f) && !f.contains("_PROF_"))
            .cloned()
            .collect();
        let prof: HashSet<String> = baseline
            .iter()
            .filter(|f| configured.contains(*f) && f.contains("_PROF_"))
            .cloned()
            .collect();
        let deadline = Instant::now() + Duration::from_secs_f64(timing.apply_timeout.max(0.0));
        let mut present: HashSet<String> = HashSet::new();
        let mut answered = false;
        let mut last_err;
        loop {
            let (samples, err) = self.scrape().await;
            last_err = err;
            if let Some(s) = samples {
                answered = true;
                present = s
                    .keys()
                    .filter(|f| f.starts_with("DCGM_FI_") || f.starts_with("DCGM_EXP_"))
                    .cloned()
                    .collect();
                let extra = present
                    .iter()
                    .any(|f| !configured.contains(f) && !is_known_label_family(f));
                if required.is_subset(&present) && prof.is_subset(&present) && (restarted || !extra)
                {
                    break;
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            wsb::sleep_s(timing.poll_interval).await;
        }
        if !answered {
            return Ok(unhealthy(
                restarted,
                format!(
                    "/metrics did not answer within {:.0} s ({last_err})",
                    timing.apply_timeout
                ),
            ));
        }
        let mut missing: Vec<&String> = required.difference(&present).collect();
        missing.sort();
        if !missing.is_empty() {
            return Ok(unhealthy(
                restarted,
                format!(
                    "families served before the change are missing: {}",
                    missing
                        .iter()
                        .take(10)
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        let errs = self.logged_errors(began).await;
        if !errs.is_empty() {
            return Ok(unhealthy(
                restarted,
                format!(
                    "the exporter logged errors after the change: {}",
                    errs.join(" | ")
                ),
            ));
        }
        let mut stale: Vec<&String> = present
            .iter()
            .filter(|f| !configured.contains(*f) && !is_known_label_family(f))
            .collect();
        stale.sort();
        if !stale.is_empty() && !restarted {
            return Ok(unhealthy(
                false,
                format!(
                    "the exporter still serves fields that are no longer in the file ({}): it did not re-read the file; apply with restart",
                    stale
                        .iter()
                        .take(6)
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        let mut late: Vec<&String> = prof.difference(&present).collect();
        late.sort();
        if !late.is_empty() {
            notes.push(format!(
                "DCGM_FI_PROF_* families not served yet: {}",
                late.iter()
                    .take(5)
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        {
            let mut st = lock(&self.st);
            st.freeze_until = None;
            st.baseline = present.clone();
        }
        let mut detail = format!("/metrics answers with {} DCGM families", present.len());
        if !notes.is_empty() {
            detail.push_str(&format!("; {}", notes.join("; ")));
        }
        Ok(ApplyResult {
            ok: true,
            new_sha256: None,
            revision_id: None,
            restarted,
            post_check: PostCheck::Healthy,
            detail,
            rolled_back: false,
        })
    }

    // ── act ─────────────────────────────────────────────────────────────────────────────────
    fn gpu_param(&self) -> ParamSpec {
        let mut known: Vec<(String, String)> = self.gpus().into_iter().collect();
        known.sort_by_key(|(g, _)| g.parse::<i64>().unwrap_or(i64::MAX));
        let keys: Vec<String> = known.iter().map(|(g, _)| g.clone()).collect();
        let help = format!(
            "One GPU per run: the two cards are different models and DCGM refuses a mixed group. {}",
            if keys.is_empty() {
                "No GPU discovered yet.".to_owned()
            } else {
                known
                    .iter()
                    .map(|(g, m)| format!("{g} = {m}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            }
        );
        let mut p = ParamSpec::new(
            "gpu",
            "GPU index",
            &help,
            if keys.is_empty() {
                ParamType::Int
            } else {
                ParamType::Enum
            },
        );
        p.default = keys.first().map_or(json!(0), |k| json!(k));
        #[allow(clippy::cast_precision_loss)]
        let max = (MAX_GPUS - 1) as f64;
        p.min = if keys.is_empty() { Some(0.0) } else { None };
        p.max = if keys.is_empty() { Some(max) } else { None };
        p.choices = if keys.is_empty() { None } else { Some(keys) };
        p.requires_restart = false;
        p
    }

    fn docker_run(&self, name: &str, script: &str) -> Vec<String> {
        let image = lock(&self.names).diag_image.clone();
        wsb::argv(&[
            "docker",
            "run",
            "--rm",
            "--pull",
            "never",
            "--name",
            name,
            "--gpus",
            "all",
            "--cap-drop",
            "ALL",
            "--cap-add",
            "SYS_ADMIN",
            "--network",
            "none",
            "--security-opt",
            "no-new-privileges",
            "--read-only",
            "--tmpfs",
            "/tmp",
            "--pids-limit",
            "256",
            "--memory",
            "2g",
            "--entrypoint",
            "/bin/sh",
            &image,
            "-c",
            script,
        ])
    }

    async fn force_remove(&self, name: &str) {
        self.run(&wsb::argv(&["docker", "rm", "-f", name]), 30.0, 2048)
            .await;
    }

    /// `_check_gpu`: an int or str holding 1-2 digits, below MAX_GPUS and (once known) a discovered GPU.
    #[must_use]
    pub fn check_gpu(&self, raw: Option<&Value>) -> Option<u32> {
        let s = match raw {
            Some(Value::Bool(_)) | None => return None,
            Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.to_string(),
            Some(Value::String(s)) => pystr::strip(s).to_owned(),
            Some(_) => String::new(),
        };
        if !full(&GPU_IDX_RE, &s) {
            return None;
        }
        let n: u32 = s.parse().ok()?;
        let gpus = self.gpus();
        if n as usize >= MAX_GPUS || (!gpus.is_empty() && !gpus.contains_key(&n.to_string())) {
            return None;
        }
        Some(n)
    }

    fn image_problem(&self) -> Problem {
        Problem::new(
            Severity::Error,
            "dcgm.diag.image_missing",
            "the DCGM diagnostics image is not present locally (runs use --pull never)",
            Some(format!("docker pull {}", lock(&self.names).diag_image)),
        )
    }

    async fn discovery(&self, t0: Instant) -> ActionResult {
        #[allow(clippy::cast_possible_truncation)]
        let ms = ((now_ts() * 1000.0) as i64).rem_euclid(1_000_000_000);
        let name = format!("nemo-console-dcgm-disc-{ms}");
        let r = self
            .run(&self.docker_run(&name, DISCOVERY_CMD), 90.0, 64_000)
            .await;
        if r.timed_out {
            self.force_remove(&name).await;
        }
        if r.returncode != 0 {
            let low = r.stderr.to_lowercase();
            let probs = if low.contains("no such image") || low.contains("unable to find image") {
                vec![self.image_problem()]
            } else {
                Vec::new()
            };
            return done(
                t0,
                false,
                &format!(
                    "discovery failed (rc {}): {}",
                    r.returncode,
                    pystr::tail(pystr::strip(&r.stderr), 600)
                ),
                probs,
            );
        }
        done(t0, true, pystr::strip(&mask_uuids(&r.stdout)), Vec::new())
    }

    fn new_run_dir(&self, level: u8, gpu: u32) -> std::io::Result<(String, PathBuf)> {
        let run_id = format!("{}-diag{level}-gpu{gpu}", wsb::run_stamp());
        let run_dir = self.runs_dir().join(&run_id);
        wsb::mkdir_leaf(&run_dir, 0o700)?;
        Ok((run_id, run_dir))
    }

    fn prune(&self) {
        for d in self.diag_dirs().iter().skip(KEEP_RUNS) {
            wsb::rmtree(d);
        }
    }

    async fn diag(&self, level: u8, gpu: u32, t0: Instant) -> Result<ActionResult, AdapterError> {
        let Ok(_guard) = self.diag_lock.try_lock() else {
            return Ok(done(
                t0,
                false,
                "another diagnostic is running; wait for it to finish",
                Vec::new(),
            ));
        };
        let (run_id, run_dir) = self
            .new_run_dir(level, gpu)
            .map_err(|e| AdapterError::failed(io_class(&e), e.to_string()))?;
        let name = format!("nemo-console-dcgm-{}", run_id.to_lowercase());
        let started = Instant::now();
        let script = diag_cmd(level).replace("{gpu}", &gpu.to_string());
        let r = self
            .run(
                &self.docker_run(&name, &script),
                diag_timeout(level),
                512_000,
            )
            .await;
        if r.timed_out {
            self.force_remove(&name).await;
        }
        let duration = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        let mut doc: Option<Value> = None;
        if let (Some(a), Some(b)) = (r.stdout.find('{'), r.stdout.rfind('}'))
            && b > 0
            && a <= b
            && let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(&r.stdout[a..=b])
        {
            doc = Some(v);
        }
        let low = r.stderr.to_lowercase();
        let mut problems: Vec<Problem> = Vec::new();
        let (verdict, tests, mut notes) = match &doc {
            None => {
                let src = if r.stderr.is_empty() {
                    &r.stdout
                } else {
                    &r.stderr
                };
                let mut notes = vec![pystr::tail(&redact(pystr::strip(src)), 300).to_owned()];
                if r.timed_out {
                    notes = vec![format!(
                        "timed out after {:.0} s (container removed)",
                        diag_timeout(level)
                    )];
                }
                if low.contains("no such image") || low.contains("unable to find image") {
                    problems.push(self.image_problem());
                }
                ("error".to_owned(), Vec::new(), notes)
            }
            Some(d) => summarize_diag(&strip_serials(d)),
        };
        notes.truncate(10);
        let model = self
            .gpus()
            .get(&gpu.to_string())
            .cloned()
            .unwrap_or_default();
        let tests_json: Vec<Value> = tests
            .iter()
            .map(|(n, s)| json!({"name": n, "status": s}))
            .collect();
        let meta = json!({
            "run_id": run_id, "ts": now_ts(), "gpu": gpu, "level": level, "verdict": verdict, "duration_ms": duration,
            "model": model, "tests": tests_json, "notes": notes, "returncode": r.returncode,
        });
        let write = || -> std::io::Result<()> {
            wsb::write_private(&run_dir.join("run.json"), &wsb::json_indent1(&meta))?;
            if let Some(d) = &doc {
                wsb::write_private(
                    &run_dir.join("output.json"),
                    &wsb::json_indent1(&strip_serials(d)),
                )?;
            }
            self.prune();
            Ok(())
        };
        let _ = write();
        lock(&self.st).image_cache = None;
        let where_ = if model.is_empty() {
            format!("GPU {gpu}")
        } else {
            format!("GPU {gpu} ({model})")
        };
        #[allow(clippy::cast_precision_loss)]
        let secs = duration as f64 / 1000.0;
        let mut lines = vec![
            format!(
                "DCGM diagnostic level {level} on {where_}: {} in {secs:.1} s",
                verdict.to_uppercase()
            ),
            format!("run: {run_id}"),
        ];
        for (n, s) in &tests {
            lines.push(format!("  {} {s}", pystr::ljust(n, 22)));
        }
        for n in &notes {
            lines.push(format!("  note: {n}"));
        }
        Ok(done(
            t0,
            verdict == "pass" || verdict == "warn",
            &lines.join("\n"),
            problems,
        ))
    }
}

/// `done(ok, output, problems)` of run_action: output redacted and capped, duration since the action began.
fn done(t0: Instant, ok: bool, out: &str, problems: Vec<Problem>) -> ActionResult {
    ActionResult {
        ok,
        output: pystr::head(&redact(out), 20_000).to_owned(),
        duration_ms: i64::try_from(t0.elapsed().as_millis()).unwrap_or(i64::MAX),
        problems,
    }
}

fn io_class(e: &std::io::Error) -> &'static str {
    match e.kind() {
        std::io::ErrorKind::NotFound => "FileNotFoundError",
        std::io::ErrorKind::PermissionDenied => "PermissionError",
        std::io::ErrorKind::AlreadyExists => "FileExistsError",
        _ => "OSError",
    }
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

fn point(key: &str, value: f64, unit: &str, host: &str, job: &str) -> MetricPoint {
    let mut labels = IndexMap::new();
    labels.insert("host".to_owned(), host.to_owned());
    labels.insert("job".to_owned(), job.to_owned());
    MetricPoint {
        key: key.to_owned(),
        value,
        unit: unit.to_owned(),
        labels,
    }
}

fn counters_path(ctx: &AdapterContext) -> Option<&Path> {
    ctx.paths.get("dcgm_counters").map(PathBuf::as_path)
}

#[async_trait]
impl SystemAdapter for DcgmAdapter {
    fn id(&self) -> SystemId {
        SystemId::Dcgm
    }

    fn label(&self) -> String {
        LABEL.to_owned()
    }

    async fn status(&self) -> Result<SystemStatus, AdapterError> {
        Ok(self.status_impl().await)
    }

    async fn metrics(&self) -> Result<MetricsSnapshot, AdapterError> {
        Ok(self.metrics_impl().await)
    }

    async fn recent_events(&self, limit: usize) -> Result<Vec<SystemEvent>, AdapterError> {
        let limit = limit.min(200);
        let mut events = self.diag_events(limit);
        events.extend(lock(&self.st).events.iter().map(|(_, e)| e.clone()));
        events.sort_by(|a, b| b.ts.total_cmp(&a.ts));
        events.truncate(limit);
        Ok(events)
    }

    fn follow_events(self: Arc<Self>, since_ts: Option<f64>) -> EventStream {
        Box::pin(async_stream::stream! {
            let mut seen_runs: HashSet<String> = HashSet::new();
            let mut seq = if since_ts.is_none() { lock(&self.st).seq } else { 0 };
            let mut first = true;
            loop {
                let (samples, _) = self.scrape().await;
                if let Some(s) = &samples {
                    self.observe(s);
                }
                let mut fresh: Vec<SystemEvent> = Vec::new();
                let ring: Vec<(u64, SystemEvent)> = lock(&self.st).events.iter().cloned().collect();
                for (n, ev) in ring {
                    if n > seq {
                        seq = n;
                        if !(first && since_ts.is_some_and(|s| ev.ts <= s)) {
                            fresh.push(ev);
                        }
                    }
                }
                let mut diags = self.diag_events(50);
                diags.sort_by(|a, b| a.ts.total_cmp(&b.ts));
                for ev in diags {
                    let rid = wsb::json_get_str(ev.data.get("run_id"));
                    if seen_runs.contains(&rid) {
                        continue;
                    }
                    seen_runs.insert(rid);
                    if first && since_ts.is_none_or(|s| ev.ts <= s) {
                        continue;
                    }
                    fresh.push(ev);
                }
                first = false;
                fresh.sort_by(|a, b| a.ts.total_cmp(&b.ts));
                for ev in fresh {
                    yield Ok(ev);
                }
                wsb::sleep_s(self.timing().follow_interval).await;
            }
        })
    }

    fn config_files(&self) -> Result<Vec<ConfigFileSpec>, AdapterError> {
        if counters_path(&self.ctx).is_none() {
            return Ok(Vec::new());
        }
        Ok(vec![Self::counters_spec()])
    }

    fn config_path(&self, file_id: &str) -> Result<PathBuf, AdapterError> {
        let p = counters_path(&self.ctx).filter(|_| file_id == COUNTERS_ID);
        let Some(p) = p else {
            return Err(AdapterError::UnknownId(file_id.to_owned()));
        };
        if p.is_symlink() || !p.is_file() {
            return Err(AdapterError::UnknownId(file_id.to_owned()));
        }
        Ok(p.to_path_buf())
    }

    async fn read_config(&self, file_id: &str) -> Result<ConfigFile, AdapterError> {
        let p = self.config_path(file_id)?;
        let md =
            std::fs::metadata(&p).map_err(|_| AdapterError::FileNotFound(file_id.to_owned()))?;
        if md.len() > MAX_CONFIG_BYTES {
            return Err(AdapterError::FileNotFound(file_id.to_owned()));
        }
        let bytes =
            std::fs::read(&p).map_err(|_| AdapterError::FileNotFound(file_id.to_owned()))?;
        let content = String::from_utf8(bytes).map_err(|_| {
            AdapterError::failed(
                "UnicodeDecodeError",
                "'utf-8' codec can't decode the counters file",
            )
        })?;
        Ok(ConfigFile {
            spec: Self::counters_spec(),
            sha256: sha256_hex(content.as_bytes()),
            content,
            mtime: crate::services::config_store::mtime_f64(&md).unwrap_or(0.0),
        })
    }

    async fn validate_config(
        &self,
        file_id: &str,
        content: &str,
    ) -> Result<ValidationResult, AdapterError> {
        if file_id != COUNTERS_ID {
            return Err(AdapterError::UnknownId(file_id.to_owned()));
        }
        let name = "dcgm-exporter field list check".to_owned();
        let note = format!(
            "A proxy check: field names and types come from the CSV lists shipped in the dcgm-exporter image ({} names). The exporter accepts more fields than those lists name, so an unlisted name without a close match is only a warning. Whether the exporter really serves each family is decided after the apply by polling /metrics.",
            KNOWN_FIELDS.len()
        );
        if content.len() as u64 > MAX_CONFIG_BYTES {
            return Ok(ValidationResult {
                ok: false,
                validator: name,
                authoritative: false,
                diagnostics: vec![diag(Severity::Error, None, "file larger than 256 KiB")],
                note: Some(note),
            });
        }
        let (_, diags) = parse_counters(content);
        // an apply normally follows a validate: freeze the rollback yardstick at what is served right now
        let (samples, _) = self.scrape().await;
        if let Some(s) = &samples {
            self.observe(s);
        }
        lock(&self.st).freeze_until = Some(Instant::now() + Duration::from_secs(300));
        Ok(ValidationResult {
            ok: !diags.iter().any(|d| d.severity == Severity::Error),
            validator: name,
            authoritative: false,
            diagnostics: diags,
            note: Some(note),
        })
    }

    async fn post_apply(&self, file_id: &str, restart: bool) -> Result<ApplyResult, AdapterError> {
        self.post_apply_impl(file_id, restart).await
    }

    fn actions(&self) -> Result<Vec<ActionSpec>, AdapterError> {
        let container = lock(&self.names).exporter_container.clone();
        let mut diag1 = ActionSpec::new(
            "diag",
            "Diagnostic, level 1",
            "`dcgmi diag -r 1` on one GPU in a throwaway container with SYS_ADMIN and GPU access (software deployment checks, a few seconds, no load). Stored as a run; the GPU serial number is never shown.",
            Danger::Disruptive,
        );
        diag1.params = vec![self.gpu_param()];
        let mut diag2 = ActionSpec::new(
            "diag_level2",
            "Diagnostic, level 2",
            "`dcgmi diag -r 2` on one GPU: loads the card for minutes (memory and PCIe tests). While a model is served on that card, inference stalls or slows; run it only when the card is idle.",
            Danger::Disruptive,
        );
        diag2.params = vec![self.gpu_param()];
        Ok(vec![
            diag1,
            diag2,
            ActionSpec::new(
                "discovery",
                "Discover GPUs",
                "`dcgmi discovery -l` in a throwaway container: the GPUs DCGM sees (UUIDs shortened).",
                Danger::Safe,
            ),
            ActionSpec::new(
                "restart_exporter",
                "Restart exporter",
                &format!(
                    "`docker restart {container}`: /metrics is unavailable for ~10 s and DCGM_FI_PROF_* families take 10-20 s to return; Prometheus shows a gap."
                ),
                Danger::Disruptive,
            ),
        ])
    }

    async fn run_action(
        &self,
        action_id: &str,
        params: &Map<String, Value>,
    ) -> Result<ActionResult, AdapterError> {
        let t0 = Instant::now();
        if !matches!(
            action_id,
            "diag" | "diag_level2" | "discovery" | "restart_exporter"
        ) {
            return Err(AdapterError::UnknownId(action_id.to_owned()));
        }
        if action_id == "restart_exporter" {
            if let Some(why) = self.identity_problem().await {
                return Ok(done(t0, false, &why, Vec::new()));
            }
            let container = lock(&self.names).exporter_container.clone();
            let r = self
                .run(&wsb::argv(&["docker", "restart", &container]), 90.0, 4096)
                .await;
            if r.returncode != 0 {
                return Ok(done(
                    t0,
                    false,
                    &format!("docker restart failed: {}", pystr::strip(&r.stderr)),
                    Vec::new(),
                ));
            }
            let timing = self.timing();
            let deadline = Instant::now() + Duration::from_secs_f64(timing.apply_timeout.max(0.0));
            while Instant::now() < deadline {
                let (samples, _) = self.scrape().await;
                if let Some(s) = samples {
                    self.observe(&s);
                    let secs = t0.elapsed().as_secs_f64();
                    return Ok(done(
                        t0,
                        true,
                        &format!("restarted; /metrics answers again after {secs:.0} s"),
                        Vec::new(),
                    ));
                }
                wsb::sleep_s(timing.poll_interval).await;
            }
            return Ok(done(
                t0,
                false,
                "restarted, but /metrics did not answer within the wait",
                vec![Problem::new(
                    Severity::Error,
                    "dcgm.exporter.down",
                    "the exporter does not answer after the restart",
                    Some(format!("docker logs --tail 50 {container}")),
                )],
            ));
        }
        if action_id == "discovery" {
            return Ok(self.discovery(t0).await);
        }
        let level = if action_id == "diag" { 1 } else { 2 };
        let Some(gpu) = self.check_gpu(params.get("gpu")) else {
            let mut known: Vec<String> = self.gpus().keys().cloned().collect();
            known.sort_by_key(|g| g.parse::<i64>().unwrap_or(i64::MAX));
            let known = if known.is_empty() {
                "none discovered yet".to_owned()
            } else {
                known.join(", ")
            };
            return Ok(done(
                t0,
                false,
                &format!("gpu must be an integer index of a discovered GPU ({known})"),
                Vec::new(),
            ));
        };
        self.diag(level, gpu, t0).await
    }
}

/// Build the adapter (Python: registry `build_adapter` + `container._configure_dcgm`).
pub fn build(ctx: &AdapterContext, cfg: &Settings) -> Result<AdapterRef, AdapterError> {
    Ok(Arc::new(DcgmAdapter::from_settings(ctx, cfg)))
}
