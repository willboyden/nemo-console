//! Observability for the console itself: JSON logs (scrubbed), Prometheus /metrics, optional OTLP.
//! Port of `services/telemetry.py`. Contract (PORTING.md section 5.6): the exposition text is written by hand so it is
//! byte-identical to `prometheus_client.generate_latest` 0.21+ (same `_created` series, label order with `le` first,
//! `floatToGoString` numbers, HELP/TYPE lines). Grafana boards read these names and labels.

use std::sync::Mutex;

use indexmap::IndexMap;

use crate::domain::models::now_ts;
use crate::services::common::RecoverLock as _;
use crate::services::pyfmt::go_float;

/// prometheus_client's default histogram buckets.
pub const BUCKETS: [f64; 15] = [
    0.005,
    0.01,
    0.025,
    0.05,
    0.075,
    0.1,
    0.25,
    0.5,
    0.75,
    1.0,
    2.5,
    5.0,
    7.5,
    10.0,
    f64::INFINITY,
];

fn esc(v: &str) -> String {
    v.replace('\\', r"\\")
        .replace('\n', r"\n")
        .replace('"', "\\\"")
}

fn labels(names: &[&str], values: &[String]) -> String {
    let parts: Vec<String> = names
        .iter()
        .zip(values)
        .map(|(n, v)| format!("{n}=\"{}\"", esc(v)))
        .collect();
    parts.join(",")
}

struct CounterSeries {
    value: f64,
    created: f64,
}

struct HistSeries {
    buckets: [f64; 15],
    sum: f64,
    count: f64,
    created: f64,
}

#[derive(Default)]
struct Inner {
    requests: IndexMap<Vec<String>, CounterSeries>,
    latency: IndexMap<Vec<String>, HistSeries>,
    denied: IndexMap<Vec<String>, CounterSeries>,
    sse: f64,
}

pub struct Telemetry {
    inner: Mutex<Inner>,
}

impl Default for Telemetry {
    fn default() -> Self {
        Telemetry::new()
    }
}

impl Telemetry {
    #[must_use]
    pub fn new() -> Self {
        Telemetry {
            inner: Mutex::new(Inner::default()),
        }
    }

    /// `nemo_console_http_requests_total{method,route,status}` += 1.
    pub fn request(&self, method: &str, route: &str, status: &str) {
        if let Ok(mut g) = self.inner.lock_recovering() {
            let k = vec![method.to_owned(), route.to_owned(), status.to_owned()];
            g.requests
                .entry(k)
                .or_insert_with(|| CounterSeries {
                    value: 0.0,
                    created: now_ts(),
                })
                .value += 1.0;
        }
    }

    /// `nemo_console_http_request_seconds{method,route}` observe.
    pub fn latency(&self, method: &str, route: &str, seconds: f64) {
        if let Ok(mut g) = self.inner.lock_recovering() {
            let k = vec![method.to_owned(), route.to_owned()];
            let h = g.latency.entry(k).or_insert_with(|| HistSeries {
                buckets: [0.0; 15],
                sum: 0.0,
                count: 0.0,
                created: now_ts(),
            });
            for (i, b) in BUCKETS.iter().enumerate() {
                if seconds <= *b {
                    h.buckets[i] += 1.0;
                }
            }
            h.sum += seconds;
            h.count += 1.0;
        }
    }

    /// `nemo_console_denied_requests_total{status}` += 1.
    pub fn denied(&self, status: &str) {
        if let Ok(mut g) = self.inner.lock_recovering() {
            g.denied
                .entry(vec![status.to_owned()])
                .or_insert_with(|| CounterSeries {
                    value: 0.0,
                    created: now_ts(),
                })
                .value += 1.0;
        }
    }

    /// `nemo_console_sse_connections` gauge.
    pub fn set_sse(&self, v: usize) {
        if let Ok(mut g) = self.inner.lock_recovering() {
            #[allow(clippy::cast_precision_loss)]
            {
                g.sse = v as f64;
            }
        }
    }

    /// `generate_latest(registry)`.
    #[must_use]
    pub fn render(&self) -> String {
        let Ok(g) = self.inner.lock_recovering() else {
            return String::new();
        };
        let mut o = String::new();
        let counter = |o: &mut String,
                       name: &str,
                       help: &str,
                       names: &[&str],
                       series: &IndexMap<Vec<String>, CounterSeries>| {
            o.push_str(&format!(
                "# HELP {name}_total {help}\n# TYPE {name}_total counter\n"
            ));
            for (k, s) in series {
                o.push_str(&format!(
                    "{name}_total{{{}}} {}\n",
                    labels(names, k),
                    go_float(s.value)
                ));
            }
            if !series.is_empty() {
                o.push_str(&format!(
                    "# HELP {name}_created {help}\n# TYPE {name}_created gauge\n"
                ));
                for (k, s) in series {
                    o.push_str(&format!(
                        "{name}_created{{{}}} {}\n",
                        labels(names, k),
                        go_float(s.created)
                    ));
                }
            }
        };
        counter(
            &mut o,
            "nemo_console_http_requests",
            "HTTP requests",
            &["method", "route", "status"],
            &g.requests,
        );
        let name = "nemo_console_http_request_seconds";
        o.push_str(&format!(
            "# HELP {name} HTTP latency\n# TYPE {name} histogram\n"
        ));
        for (k, h) in &g.latency {
            for (i, b) in BUCKETS.iter().enumerate() {
                let mut ln = vec![format!("le=\"{}\"", go_float(*b))];
                ln.push(labels(&["method", "route"], k));
                o.push_str(&format!(
                    "{name}_bucket{{{}}} {}\n",
                    ln.join(","),
                    go_float(h.buckets[i])
                ));
            }
            o.push_str(&format!(
                "{name}_count{{{}}} {}\n",
                labels(&["method", "route"], k),
                go_float(h.count)
            ));
            o.push_str(&format!(
                "{name}_sum{{{}}} {}\n",
                labels(&["method", "route"], k),
                go_float(h.sum)
            ));
        }
        if !g.latency.is_empty() {
            o.push_str(&format!(
                "# HELP {name}_created HTTP latency\n# TYPE {name}_created gauge\n"
            ));
            for (k, h) in &g.latency {
                o.push_str(&format!(
                    "{name}_created{{{}}} {}\n",
                    labels(&["method", "route"], k),
                    go_float(h.created)
                ));
            }
        }
        counter(
            &mut o,
            "nemo_console_denied_requests",
            "Requests denied by the middleware/roles (not audited per row)",
            &["status"],
            &g.denied,
        );
        o.push_str("# HELP nemo_console_sse_connections Open SSE streams\n# TYPE nemo_console_sse_connections gauge\n");
        o.push_str(&format!(
            "nemo_console_sse_connections {}\n",
            go_float(g.sse)
        ));
        o
    }
}

/// Optional OTLP export (`otlp_endpoint`). Python needs optional opentelemetry packages that the console does not
/// install, so it only logs one warning; Rust does the same (PORTING.md deviation D-5: no OTLP export).
pub fn setup_otlp(endpoint: Option<&str>) {
    if endpoint.is_some_and(|e| !e.is_empty()) {
        tracing::warn!("OTLP endpoint configured but opentelemetry packages are not installed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from `prometheus_client.generate_latest` (Python backend venv, 2026-10-27) after the same calls;
    /// only the `_created` sample values (wall-clock) are normalised.
    const PYTHON: &str = r#"# HELP nemo_console_http_requests_total HTTP requests
# TYPE nemo_console_http_requests_total counter
nemo_console_http_requests_total{method="GET",route="/api/v1/systems",status="200"} 1.0
# HELP nemo_console_http_requests_created HTTP requests
# TYPE nemo_console_http_requests_created gauge
nemo_console_http_requests_created{method="GET",route="/api/v1/systems",status="200"} <ts>
# HELP nemo_console_http_request_seconds HTTP latency
# TYPE nemo_console_http_request_seconds histogram
nemo_console_http_request_seconds_bucket{le="0.005",method="GET",route="/api/v1/systems"} 0.0
nemo_console_http_request_seconds_bucket{le="0.01",method="GET",route="/api/v1/systems"} 0.0
nemo_console_http_request_seconds_bucket{le="0.025",method="GET",route="/api/v1/systems"} 1.0
nemo_console_http_request_seconds_bucket{le="0.05",method="GET",route="/api/v1/systems"} 1.0
nemo_console_http_request_seconds_bucket{le="0.075",method="GET",route="/api/v1/systems"} 1.0
nemo_console_http_request_seconds_bucket{le="0.1",method="GET",route="/api/v1/systems"} 1.0
nemo_console_http_request_seconds_bucket{le="0.25",method="GET",route="/api/v1/systems"} 1.0
nemo_console_http_request_seconds_bucket{le="0.5",method="GET",route="/api/v1/systems"} 1.0
nemo_console_http_request_seconds_bucket{le="0.75",method="GET",route="/api/v1/systems"} 1.0
nemo_console_http_request_seconds_bucket{le="1.0",method="GET",route="/api/v1/systems"} 1.0
nemo_console_http_request_seconds_bucket{le="2.5",method="GET",route="/api/v1/systems"} 1.0
nemo_console_http_request_seconds_bucket{le="5.0",method="GET",route="/api/v1/systems"} 1.0
nemo_console_http_request_seconds_bucket{le="7.5",method="GET",route="/api/v1/systems"} 1.0
nemo_console_http_request_seconds_bucket{le="10.0",method="GET",route="/api/v1/systems"} 1.0
nemo_console_http_request_seconds_bucket{le="+Inf",method="GET",route="/api/v1/systems"} 1.0
nemo_console_http_request_seconds_count{method="GET",route="/api/v1/systems"} 1.0
nemo_console_http_request_seconds_sum{method="GET",route="/api/v1/systems"} 0.0123
# HELP nemo_console_http_request_seconds_created HTTP latency
# TYPE nemo_console_http_request_seconds_created gauge
nemo_console_http_request_seconds_created{method="GET",route="/api/v1/systems"} <ts>
# HELP nemo_console_denied_requests_total Requests denied by the middleware/roles (not audited per row)
# TYPE nemo_console_denied_requests_total counter
nemo_console_denied_requests_total{status="401"} 5.0
# HELP nemo_console_denied_requests_created Requests denied by the middleware/roles (not audited per row)
# TYPE nemo_console_denied_requests_created gauge
nemo_console_denied_requests_created{status="401"} <ts>
# HELP nemo_console_sse_connections Open SSE streams
# TYPE nemo_console_sse_connections gauge
nemo_console_sse_connections 2.0
"#;

    #[test]
    fn exposition_is_byte_identical_to_prometheus_client() {
        let t = Telemetry::new();
        t.request("GET", "/api/v1/systems", "200");
        t.latency("GET", "/api/v1/systems", 0.0123);
        for _ in 0..5 {
            t.denied("401");
        }
        t.set_sse(2);
        let out: Vec<String> = t
            .render()
            .lines()
            .map(|l| match l.split_once("_created{") {
                Some((head, rest)) if !l.starts_with('#') => {
                    let labels = rest.rsplit_once(' ').map_or(rest, |(a, _)| a);
                    format!("{head}_created{{{labels} <ts>")
                }
                _ => l.to_owned(),
            })
            .collect();
        assert_eq!(out.join("\n") + "\n", PYTHON);
    }

    #[test]
    fn empty_registry_matches_prometheus_client() {
        let out = Telemetry::new().render();
        assert_eq!(
            out,
            "# HELP nemo_console_http_requests_total HTTP requests\n# TYPE nemo_console_http_requests_total counter\n\
             # HELP nemo_console_http_request_seconds HTTP latency\n# TYPE nemo_console_http_request_seconds histogram\n\
             # HELP nemo_console_denied_requests_total Requests denied by the middleware/roles (not audited per row)\n\
             # TYPE nemo_console_denied_requests_total counter\n\
             # HELP nemo_console_sse_connections Open SSE streams\n# TYPE nemo_console_sse_connections gauge\n\
             nemo_console_sse_connections 0.0\n"
        );
    }
}
