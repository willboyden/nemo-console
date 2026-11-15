//! `GET /capture`: which capture levels NeMo Relay is configured for, and what the console can see of each.
//!
//! The configuration comes from `plugins.toml` (the adapter already reads it for its status page). Only names, flags
//! and numbers are taken from it; of an endpoint only `host:port` leaves this module, never its path, query, user
//! info or headers. What was observed comes from the trace index.

use indexmap::IndexMap;
use serde_json::{Map, Value, json};

use super::ids::is_ident;
use super::snapshot::Snapshot;
use crate::services::pytoml::{self, Table, TomlVal};

/// "Something was written in the last 15 minutes" / "within 7 days".
const ACTIVE_S: f64 = 15.0 * 60.0;
const IDLE_S: f64 = 7.0 * 86_400.0;
/// Resource samples (addendum 9.17): `active` while the newest is no older than this many intervals; the interval
/// assumed when the configuration does not give one.
const RESOURCE_INTERVALS: f64 = 3.0;
const RESOURCE_DEFAULT_INTERVAL_S: f64 = 60.0;
/// The command the `relay.resources.off` hint names.
pub const RESOURCES_ON: &str = "python3 -I ops/nemo-relay-levels.py resources on";

/// One OTLP endpoint of the configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// `host:port`.
    pub target: Option<String>,
    /// `http://host:port/` when the host is a loopback address reached over plain HTTP: the only thing the
    /// reachability probe may ask.
    pub probe_url: Option<String>,
    pub projection: Option<String>,
    pub transport: Option<String>,
    pub service_name: Option<String>,
}

fn tbl(v: Option<&TomlVal>) -> Option<&Table> {
    v.and_then(TomlVal::as_table)
}

fn arr(v: Option<&TomlVal>) -> &[TomlVal] {
    match v {
        Some(TomlVal::Array(a)) => a,
        _ => &[],
    }
}

fn flag(t: Option<&Table>, key: &str) -> Option<bool> {
    match t?.get(key)? {
        TomlVal::Bool(b) => Some(*b),
        _ => None,
    }
}

fn ident(t: Option<&Table>, key: &str) -> Option<String> {
    t?.get(key)?
        .as_str()
        .filter(|s| is_ident(s))
        .map(str::to_owned)
}

fn int(t: Option<&Table>, key: &str) -> Option<i64> {
    match t?.get(key)? {
        TomlVal::Int(i) => i64::try_from(*i).ok(),
        _ => None,
    }
}

/// `host:port` of an endpoint, and the URL the probe may use. Accepts a URL or a bare authority; anything with user
/// info, or that does not parse, gives nothing.
#[must_use]
pub fn endpoint_target(raw: &str) -> (Option<String>, Option<String>) {
    let text = raw.trim();
    if text.is_empty() || text.len() > 2048 || text.chars().any(|c| c.is_control() || c == ' ') {
        return (None, None);
    }
    let with_scheme = if text.contains("://") {
        text.to_owned()
    } else {
        // a bare authority (gRPC endpoints are often written that way)
        format!("http://{text}")
    };
    let Ok(u) = url::Url::parse(&with_scheme) else {
        return (None, None);
    };
    if !u.username().is_empty() || u.password().is_some() {
        return (None, None);
    }
    let (Some(host), Some(port)) = (u.host(), u.port_or_known_default()) else {
        return (None, None);
    };
    let (host_text, loopback) = match &host {
        url::Host::Ipv4(a) => (a.to_string(), a.is_loopback()),
        url::Host::Ipv6(a) => (format!("[{a}]"), a.is_loopback()),
        url::Host::Domain(d) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            if !d
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'.')
            {
                return (None, None);
            }
            let lo = d == "localhost";
            (d, lo)
        }
    };
    let target = format!("{host_text}:{port}");
    let probe = (loopback && u.scheme() == "http").then(|| format!("http://{target}/"));
    (Some(target), probe)
}

fn endpoint_of(t: &Table, with_projection: bool) -> Endpoint {
    let (target, probe_url) = t
        .get("endpoint")
        .and_then(TomlVal::as_str)
        .map_or((None, None), endpoint_target);
    Endpoint {
        target,
        probe_url,
        projection: if with_projection {
            ident(Some(t), "type")
        } else {
            None
        },
        transport: ident(Some(t), "transport"),
        service_name: ident(Some(t), "service_name"),
    }
}

/// What `plugins.toml` says about capturing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CaptureConfig {
    /// False when the file is missing or does not parse: every level is then unknown.
    pub readable: bool,
    pub observability: bool,
    pub atof: bool,
    /// `(type, file name, mode)` per sink.
    pub atof_sinks: Vec<(Option<String>, Option<String>, Option<String>)>,
    pub atif: bool,
    pub atif_agent: Option<String>,
    pub otel: bool,
    pub traces: Vec<Endpoint>,
    pub logs: bool,
    pub logs_severity: Option<String>,
    pub logs_endpoints: Vec<Endpoint>,
    pub metrics: bool,
    pub metrics_interval_ms: Option<i64>,
    pub metrics_endpoints: Vec<Endpoint>,
    pub full_payloads: bool,
    pub redaction: bool,
    pub detectors: Vec<String>,
    /// A `resource_metrics` component (addendum 9.17) that is not switched off.
    pub resource_metrics: bool,
    /// Its `polling.enabled` and `polling.interval_millis`.
    pub resource_polling: Option<bool>,
    pub resource_interval_ms: Option<i64>,
    /// Its `measurement_scope` (identifier-checked), `gpu.enabled`, `network.enabled`.
    pub resource_scope: Option<String>,
    pub resource_gpu: Option<bool>,
    pub resource_network: Option<bool>,
    pub pricing: bool,
}

/// A file name of a sink: one plain path component, identifier-shaped.
fn file_name(t: &Table) -> Option<String> {
    t.get("filename")
        .and_then(TomlVal::as_str)
        .filter(|s| is_ident(s) && !s.contains('/') && *s != "." && *s != "..")
        .map(str::to_owned)
}

/// Read the capture settings out of the text of `plugins.toml`.
#[must_use]
pub fn parse_config(text: &str) -> CaptureConfig {
    let Ok(doc) = pytoml::loads(text) else {
        return CaptureConfig::default();
    };
    let mut c = CaptureConfig {
        readable: true,
        ..CaptureConfig::default()
    };
    let empty: IndexMap<String, TomlVal> = IndexMap::new();
    for comp in arr(doc.get("components")) {
        let Some(comp) = comp.as_table() else {
            continue;
        };
        if flag(Some(comp), "enabled") == Some(false) {
            continue;
        }
        let cfg = tbl(comp.get("config")).unwrap_or(&empty);
        match comp.get("kind").and_then(TomlVal::as_str) {
            Some("pii_redaction") => {
                c.redaction = true;
                for p in arr(cfg.get("profiles")) {
                    let b = tbl(p.as_table().and_then(|t| t.get("builtin")));
                    if let Some(d) = ident(b, "detector").or_else(|| ident(b, "preset"))
                        && c.detectors.len() < 32
                        && !c.detectors.contains(&d)
                    {
                        c.detectors.push(d);
                    }
                }
            }
            Some("pricing" | "model_pricing") => c.pricing = true,
            // Relay 0.11's resource sampler, a component of its own (the first one counts)
            Some("resource_metrics") if !c.resource_metrics => {
                c.resource_metrics = true;
                let polling = tbl(cfg.get("polling"));
                c.resource_polling = flag(polling, "enabled");
                c.resource_interval_ms = int(polling, "interval_millis");
                c.resource_scope = ident(Some(cfg), "measurement_scope");
                c.resource_gpu = flag(tbl(cfg.get("gpu")), "enabled");
                c.resource_network = flag(tbl(cfg.get("network")), "enabled");
            }
            Some("observability") => {
                c.observability = true;
                c.full_payloads = flag(Some(cfg), "enable_full_payloads") == Some(true);
                let atof = tbl(cfg.get("atof"));
                c.atof = flag(atof, "enabled") == Some(true);
                for s in arr(atof.and_then(|a| a.get("sinks"))).iter().take(8) {
                    if let Some(s) = s.as_table() {
                        c.atof_sinks.push((
                            ident(Some(s), "type"),
                            file_name(s),
                            ident(Some(s), "mode"),
                        ));
                    }
                }
                let atif = tbl(cfg.get("atif"));
                c.atif = flag(atif, "enabled") == Some(true);
                c.atif_agent = ident(atif, "agent_name");
                let otel = tbl(cfg.get("opentelemetry"));
                c.otel = flag(otel, "enabled") == Some(true);
                // trace endpoints: the list at the top of the section, or the `traces` table / list of version 4
                let mut trace_tables: Vec<&Table> = arr(otel.and_then(|o| o.get("endpoints")))
                    .iter()
                    .filter_map(TomlVal::as_table)
                    .collect();
                match otel.and_then(|o| o.get("traces")) {
                    Some(TomlVal::Array(a)) => {
                        trace_tables.extend(a.iter().filter_map(TomlVal::as_table));
                    }
                    Some(TomlVal::Table(t)) => {
                        trace_tables
                            .extend(arr(t.get("endpoints")).iter().filter_map(TomlVal::as_table));
                    }
                    _ => {}
                }
                c.traces = trace_tables
                    .iter()
                    .take(16)
                    .map(|t| endpoint_of(t, true))
                    .collect();
                let logs = tbl(otel.and_then(|o| o.get("logs")));
                c.logs = c.otel && flag(logs, "enabled") == Some(true);
                c.logs_severity = ident(logs, "minimum_severity");
                c.logs_endpoints = arr(logs.and_then(|l| l.get("endpoints")))
                    .iter()
                    .filter_map(TomlVal::as_table)
                    .take(16)
                    .map(|t| endpoint_of(t, false))
                    .collect();
                let metrics = tbl(otel.and_then(|o| o.get("metrics")));
                c.metrics = c.otel && flag(metrics, "enabled") == Some(true);
                c.metrics_interval_ms = int(metrics, "export_interval_millis");
                c.metrics_endpoints = arr(metrics.and_then(|m| m.get("endpoints")))
                    .iter()
                    .filter_map(TomlVal::as_table)
                    .take(16)
                    .map(|t| endpoint_of(t, false))
                    .collect();
            }
            _ => {}
        }
    }
    c
}

/// What the index saw, for the levels the console can observe.
#[derive(Clone, Copy, Debug, Default)]
pub struct Observed {
    /// False when the index is off: nothing can be said.
    pub indexed: bool,
    pub atof_last_ts: Option<f64>,
    pub atof_events: u64,
    pub atof_bytes: u64,
    pub atof_files: u64,
    pub atif_last_ts: Option<f64>,
    pub atif_trajectories: u64,
    pub atif_bytes: u64,
    /// The newest `nemo.relay.resource_metrics` mark (addendum 9.17).
    pub resource_last_ts: Option<f64>,
}

impl Observed {
    #[must_use]
    pub fn of(snap: &Snapshot) -> Observed {
        let core = &*snap.core;
        let gateway: Vec<&super::atif::Traj> = core
            .trajectories
            .items
            .iter()
            .filter(|t| t.source == super::atif::GATEWAY)
            .collect();
        let resource_last_ts = snap.info.resource_last_us.and_then(super::ids::secs);
        // the ATOF and payload rows follow the TRAFFIC (`d.last_us` leaves resource samples out): a sampler that
        // writes a mark a minute must not make an idle capture look active
        let atof_last_ts = super::ids::secs(core.d.last_us);
        #[allow(clippy::cast_precision_loss)]
        Observed {
            indexed: snap.info.state != "disabled",
            atof_last_ts,
            atof_events: snap.info.events,
            atof_bytes: snap.info.files.iter().map(|f| f.bytes).sum(),
            atof_files: snap.info.files.len() as u64,
            atif_last_ts: gateway
                .iter()
                .map(|t| t.mtime_us)
                .max()
                .map(|us| us as f64 / 1e6),
            atif_trajectories: gateway.len() as u64,
            atif_bytes: gateway.iter().map(|t| t.bytes).sum(),
            resource_last_ts,
        }
    }
}

fn state(enabled: bool, observable: bool, last_ts: Option<f64>, now: f64) -> &'static str {
    if !enabled {
        return "off";
    }
    if !observable {
        return "unknown";
    }
    match last_ts {
        Some(t) if now - t <= ACTIVE_S => "active",
        Some(t) if now - t <= IDLE_S => "idle",
        // on, and nothing (recent) to show for it
        _ => "stale",
    }
}

fn endpoint_json(e: &Endpoint, reachable: &IndexMap<String, bool>, with_projection: bool) -> Value {
    let r = e
        .probe_url
        .as_ref()
        .and_then(|u| reachable.get(u))
        .map_or(Value::Null, |b| json!(b));
    if with_projection {
        json!({"projection": e.projection, "target": e.target, "transport": e.transport, "service_name": e.service_name, "reachable": r})
    } else {
        json!({"target": e.target, "transport": e.transport, "reachable": r})
    }
}

/// The URLs the reachability probe may ask: loopback OTLP endpoints, each once.
#[must_use]
pub fn probe_urls(c: &CaptureConfig) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in c
        .traces
        .iter()
        .chain(&c.logs_endpoints)
        .chain(&c.metrics_endpoints)
    {
        if let Some(u) = &e.probe_url
            && !out.contains(u)
            && out.len() < 16
        {
            out.push(u.clone());
        }
    }
    out
}

/// `Capture`. `reachable`: the probe results by probe URL (an endpoint that was not probed has `reachable: null`).
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn capture(
    c: &CaptureConfig,
    o: &Observed,
    relay_version: Option<&str>,
    reachable: &IndexMap<String, bool>,
    now: f64,
) -> Value {
    let on = c.readable && c.observability;
    let atof = on && c.atof;
    let atif = on && c.atif;
    let otel = on && c.otel;
    let traces = otel && !c.traces.is_empty();
    let logs = otel && c.logs;
    let metrics = otel && c.metrics;
    // a log or metric section without endpoints of its own is exported once per trace endpoint
    let derived = |own: &[Endpoint], enabled: bool| enabled && own.is_empty();
    let signal_endpoints = |own: &[Endpoint], enabled: bool| -> Vec<Value> {
        let list: &[Endpoint] = if derived(own, enabled) {
            &c.traces
        } else {
            own
        };
        list.iter()
            .map(|e| endpoint_json(e, reachable, false))
            .collect()
    };
    let level = |id: &str,
                 label: &str,
                 enabled: bool,
                 st: &str,
                 detail: Value,
                 observed: Value|
     -> Value {
        json!({"id": id, "label": label, "enabled": enabled, "state": st, "detail": detail, "observed": observed})
    };
    let unknown_all = !c.readable;
    let st = |enabled: bool, observable: bool, last: Option<f64>| -> &'static str {
        if unknown_all {
            "unknown"
        } else {
            state(enabled, observable, last, now)
        }
    };
    let num = |v: Option<f64>| {
        v.and_then(serde_json::Number::from_f64)
            .map_or(Value::Null, Value::Number)
    };
    let atof_observed = if o.indexed {
        json!({"last_ts": num(o.atof_last_ts), "events": o.atof_events, "bytes": o.atof_bytes, "files": o.atof_files})
    } else {
        json!({"last_ts": null, "events": null, "bytes": null, "files": null})
    };
    // resource samples (addendum 9.17): on = a resource_metrics component with polling on
    let resources_on = c.readable && c.resource_metrics && c.resource_polling == Some(true);
    let interval_s = c
        .resource_interval_ms
        .filter(|ms| *ms > 0 && resources_on)
        .map(|ms| {
            #[allow(clippy::cast_precision_loss)]
            let s = ms as f64 / 1000.0;
            s
        });
    let resource_state = resource_state(
        c.readable,
        resources_on,
        o.indexed,
        o.resource_last_ts,
        interval_s.unwrap_or(RESOURCE_DEFAULT_INTERVAL_S),
        now,
    );
    let resource_observed = if o.indexed {
        json!({"last_ts": num(o.resource_last_ts)})
    } else {
        json!({"last_ts": null})
    };
    let atif_observed = if o.indexed {
        json!({"last_ts": num(o.atif_last_ts), "trajectories": o.atif_trajectories, "bytes": o.atif_bytes})
    } else {
        json!({"last_ts": null, "trajectories": null, "bytes": null})
    };
    let mut levels = vec![
        level(
            "atof",
            "ATOF event log (JSON lines on disk)",
            atof,
            st(atof, o.indexed, o.atof_last_ts),
            json!({"sinks": c.atof_sinks.iter().map(|(t, f, m)| json!({"type": t, "file": f, "mode": m})).collect::<Vec<_>>()}),
            atof_observed,
        ),
        level(
            "atif",
            "ATIF trajectories (one file per session)",
            atif,
            st(atif, o.indexed, o.atif_last_ts),
            json!({"agent_name": c.atif_agent}),
            atif_observed,
        ),
        level(
            "otel_traces",
            "OpenTelemetry traces",
            traces,
            st(traces, false, None),
            json!({"endpoints": c.traces.iter().map(|e| endpoint_json(e, reachable, true)).collect::<Vec<_>>()}),
            json!({}),
        ),
        level(
            "otel_logs",
            "OpenTelemetry logs (marks as log records)",
            logs,
            st(logs, false, None),
            json!({"minimum_severity": c.logs_severity, "derived": derived(&c.logs_endpoints, logs), "endpoints": signal_endpoints(&c.logs_endpoints, logs)}),
            json!({}),
        ),
        level(
            "otel_metrics",
            "OpenTelemetry metrics",
            metrics,
            st(metrics, false, None),
            json!({"export_interval_ms": c.metrics_interval_ms, "derived": derived(&c.metrics_endpoints, metrics), "endpoints": signal_endpoints(&c.metrics_endpoints, metrics)}),
            json!({}),
        ),
        level(
            "payloads",
            "Prompt and completion payloads",
            atof || atif || traces,
            // payloads travel with the events: they are as fresh as the event log
            st(atof || atif || traces, o.indexed && atof, o.atof_last_ts),
            json!({"mode": if c.full_payloads { "full" } else { "turn_filtered" }}),
            json!({}),
        ),
        level(
            "redaction",
            "Redaction of secrets and personal data",
            c.readable && c.redaction,
            st(c.readable && c.redaction, false, None),
            json!({"detectors": c.detectors}),
            json!({}),
        ),
        level(
            "resource_metrics",
            "Gateway resource metrics (CPU, memory, disk of the gateway process)",
            resources_on,
            resource_state,
            json!({"polling": c.resource_polling, "interval_ms": c.resource_interval_ms}),
            resource_observed,
        ),
        level(
            "pricing",
            "Pricing and cost attribution",
            c.readable && c.pricing,
            st(c.readable && c.pricing, false, None),
            json!({}),
            json!({}),
        ),
    ];
    if let Some(row) = levels
        .iter_mut()
        .find(|l| l["id"] == "resource_metrics")
        .and_then(Value::as_object_mut)
    {
        row.insert("group".to_owned(), json!("other"));
    }
    let has = |p: &str| traces && c.traces.iter().any(|e| e.projection.as_deref() == Some(p));
    let mut hints: Vec<Value> = Vec::new();
    let mut hint =
        |code: &str, message: String| hints.push(json!({"code": code, "message": message}));
    if !c.readable {
        hint(
            "capture.config.unreadable",
            "plugins.toml could not be read or does not parse, so the capture configuration is unknown.".to_owned(),
        );
    } else if !c.observability {
        hint(
            "capture.observability.off",
            "No observability component is enabled in plugins.toml: Relay records nothing."
                .to_owned(),
        );
    } else {
        if !atof {
            hint(
                "capture.atof.off",
                "The ATOF event log is off: the trace explorer has no events to read.".to_owned(),
            );
        }
        if !atif {
            hint(
                "capture.atif.off",
                "ATIF trajectories are off: no step-by-step record of a session is written."
                    .to_owned(),
            );
        }
        if traces && has("gen_ai") && !has("full") && !has("openinference") {
            hint(
                "capture.projection.gen_ai_only",
                "Only the gen_ai projection is exported: messages, tool payloads and marks are not in the OTLP traces."
                    .to_owned(),
            );
        }
        if otel && !logs {
            hint(
                "capture.logs.marks_not_exported",
                "Marks are not exported as OTLP logs: routing decisions and hook marks exist only in the local ATOF files."
                    .to_owned(),
            );
        }
        if otel && !metrics {
            hint(
                "capture.metrics.off",
                "OTLP metrics are off: the measurements Relay records are not exported.".to_owned(),
            );
        }
        for (name, own, enabled) in [
            ("logs", &c.logs_endpoints, logs),
            ("metrics", &c.metrics_endpoints, metrics),
        ] {
            if derived(own, enabled) && c.traces.len() > 1 {
                hint(
                    "capture.otlp.duplicate_export",
                    format!(
                        "OTLP {name} have no endpoints of their own, so Relay exports them once per trace endpoint: {} copies.",
                        c.traces.len()
                    ),
                );
            }
        }
        if (atof || atif) && !c.full_payloads {
            hint(
                "capture.payloads.turn_filtered",
                "Full payloads are off (enable_full_payloads = false): payloads are turn-filtered, so parts of a long conversation can be missing from the content views."
                    .to_owned(),
            );
        }
        if (atof || atif) && !c.redaction {
            hint(
                "capture.redaction.off",
                "No redaction component is enabled: the capture files can contain secrets that were pasted into a prompt."
                    .to_owned(),
            );
        }
        if !c.pricing {
            hint(
                "capture.pricing.off",
                "Pricing is not configured: token counts are recorded, cost is not.".to_owned(),
            );
        }
        let down: Vec<&str> = c
            .traces
            .iter()
            .chain(&c.logs_endpoints)
            .chain(&c.metrics_endpoints)
            .filter(|e| e.probe_url.as_ref().and_then(|u| reachable.get(u)) == Some(&false))
            .filter_map(|e| e.target.as_deref())
            .collect();
        if let Some(first) = down.first() {
            hint(
                "capture.otlp.unreachable",
                format!(
                    "The OTLP endpoint {first} does not answer from this machine: nothing exported to it arrives."
                ),
            );
        }
    }
    if resource_state == "off" {
        hint(
            "relay.resources.off",
            format!(
                "Gateway resource metrics are off: the gateway's CPU, memory and disk are not sampled. Add a `resource_metrics` component with polling enabled to plugins.toml and restart the gateway (in the lab: `{RESOURCES_ON}`)."
            ),
        );
    }
    let mut projections = Map::new();
    for p in ["gen_ai", "openinference", "full"] {
        projections.insert(p.to_owned(), json!(has(p)));
    }
    let resources = if c.readable {
        json!({
            "configured": c.resource_metrics,
            "enabled": resources_on,
            "scope": c.resource_scope,
            "interval_s": interval_s,
            "gpu": c.resource_gpu == Some(true),
            "network": c.resource_network == Some(true),
        })
    } else {
        json!({"configured": null, "enabled": null, "scope": null, "interval_s": null, "gpu": null, "network": null})
    };
    json!({
        "relay_version": relay_version.filter(|v| is_ident(v)),
        "levels": levels,
        "projections": projections,
        "hints": hints,
        "resources": resources,
    })
}

/// The `resource_metrics` level (addendum 9.17): `active` while the newest sample is no older than three intervals,
/// `stale` when older, `idle` when on and none seen, `off` when not on; `unknown` when the file cannot be read or
/// the index is off.
fn resource_state(
    readable: bool,
    on: bool,
    indexed: bool,
    last_ts: Option<f64>,
    interval_s: f64,
    now: f64,
) -> &'static str {
    if !readable {
        return "unknown";
    }
    if !on {
        return "off";
    }
    if !indexed {
        return "unknown";
    }
    match last_ts {
        Some(t) if now - t <= RESOURCE_INTERVALS * interval_s => "active",
        Some(_) => "stale",
        None => "idle",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_show_host_and_port_only() {
        assert_eq!(
            endpoint_target("http://127.0.0.1:4318/v1/traces?token=SECRET#frag"),
            (
                Some("127.0.0.1:4318".to_owned()),
                Some("http://127.0.0.1:4318/".to_owned())
            )
        );
        assert_eq!(
            endpoint_target("http://localhost:4318"),
            (
                Some("localhost:4318".to_owned()),
                Some("http://localhost:4318/".to_owned())
            )
        );
        assert_eq!(
            endpoint_target("http://[::1]:4318/v1/traces"),
            (
                Some("[::1]:4318".to_owned()),
                Some("http://[::1]:4318/".to_owned())
            )
        );
        // not loopback, or not plain http: shown, never probed
        assert_eq!(
            endpoint_target("http://otel-collector:4318/v1/traces"),
            (Some("otel-collector:4318".to_owned()), None)
        );
        assert_eq!(
            endpoint_target("https://127.0.0.1/v1/traces"),
            (Some("127.0.0.1:443".to_owned()), None)
        );
        assert_eq!(
            endpoint_target("http://127.0.0.1.example.org:80/"),
            (Some("127.0.0.1.example.org:80".to_owned()), None)
        );
        assert_eq!(
            endpoint_target("otel-collector:4317"),
            (Some("otel-collector:4317".to_owned()), None)
        );
        assert_eq!(
            endpoint_target("127.0.0.1:4317"),
            (
                Some("127.0.0.1:4317".to_owned()),
                Some("http://127.0.0.1:4317/".to_owned())
            )
        );
        // user info, blanks and nonsense give nothing at all
        for bad in [
            "http://user:pw@127.0.0.1:4318/",
            "http://127.0.0.1:4318/ x",
            "",
            "::::",
            "http://",
        ] {
            assert_eq!(endpoint_target(bad), (None, None), "{bad}");
        }
    }

    #[test]
    fn states_follow_the_last_write() {
        let now = 1_000_000.0;
        assert_eq!(state(false, true, Some(now), now), "off");
        assert_eq!(state(true, false, Some(now), now), "unknown");
        assert_eq!(state(true, true, Some(now - 60.0), now), "active");
        assert_eq!(state(true, true, Some(now - 3600.0), now), "idle");
        assert_eq!(state(true, true, Some(now - 30.0 * 86_400.0), now), "stale");
        assert_eq!(state(true, true, None, now), "stale");
    }
}
