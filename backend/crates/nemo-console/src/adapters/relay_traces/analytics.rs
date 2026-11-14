//! `GET /analytics`: everything the charts need for one window, computed from a snapshot.
//!
//! Definitions (the contract leaves them to the implementation; the tests pin them):
//!   * a span belongs to the window when its first known moment lies in it; a trace when it overlaps it;
//!   * `errors` counts failed LLM and tool spans, `error_rate` is that over all LLM and tool spans;
//!   * `input_tokens` is everything the model read (cached tokens included), so `cache_hit_ratio` is
//!     `cached_tokens / input_tokens`;
//!   * a percentile is the linearly interpolated value at rank `(n - 1) * p` of the sorted values (numpy's default),
//!     rounded to a thousandth; of nothing it is `null`.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::derive::{Labels, decision_label, mark_num, mark_sym, span_status};
use super::ids::{NO_TS, Sym};
use super::query::{Range, Span64};
use super::rows::{Key, bit};
use super::snapshot::{Core, Snapshot, f_json, round3};

#[derive(Clone, Debug)]
pub struct AnalyticsQuery {
    pub range: Range,
    pub client: Option<String>,
    pub model: Option<String>,
    pub kind: Option<String>,
    pub buckets: usize,
}

/// Bucket widths the time axis may have, in seconds.
const STEPS: [i64; 22] = [
    1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 10_800, 21_600, 43_200, 86_400,
    172_800, 604_800, 1_209_600, 2_592_000,
];

const DURATION_EDGES: [f64; 13] = [
    0.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10_000.0, 25_000.0,
    60_000.0,
];
const TTFC_EDGES: [f64; 11] = [
    0.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 15_000.0,
];
const INPUT_EDGES: [f64; 12] = [
    0.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16_000.0, 32_000.0, 64_000.0, 128_000.0,
    256_000.0,
];
const OUTPUT_EDGES: [f64; 11] = [
    0.0, 16.0, 32.0, 64.0, 128.0, 256.0, 512.0, 1024.0, 2048.0, 4096.0, 8192.0,
];
const TPS_EDGES: [f64; 10] = [0.0, 5.0, 10.0, 20.0, 30.0, 40.0, 60.0, 80.0, 120.0, 200.0];
const GAP_EDGES: [f64; 10] = [
    0.0, 50.0, 100.0, 200.0, 400.0, 800.0, 1600.0, 3200.0, 6400.0, 12_800.0,
];

/// The value at rank `(n - 1) * p` of the sorted values, linearly interpolated.
#[must_use]
pub fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    let n = sorted.len();
    if n == 0 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let rank = (n - 1) as f64 * p;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let lo = rank.floor() as usize;
    let hi = (lo + 1).min(n - 1);
    #[allow(clippy::cast_precision_loss)]
    let frac = rank - lo as f64;
    Some(round3(sorted[lo] + (sorted[hi] - sorted[lo]) * frac))
}

fn pct(values: &mut [f64], p: f64) -> Value {
    values.sort_by(f64::total_cmp);
    f_json(percentile(values, p))
}

/// `{edges, counts}`: the fixed edges, extended upwards (doubling) until the largest value fits; a value below the
/// first edge counts in the first bin.
fn histogram(values: &[f64], base: &[f64]) -> Value {
    let mut edges: Vec<f64> = base.to_vec();
    let max = values.iter().copied().fold(0.0f64, f64::max);
    while edges.len() < 24 && edges.last().is_some_and(|e| max >= *e) {
        let next = edges.last().copied().unwrap_or(1.0) * 2.0;
        edges.push(next);
    }
    let bins = edges.len().saturating_sub(1);
    let mut counts = vec![0u64; bins];
    for &v in values {
        let i = edges
            .partition_point(|e| *e <= v)
            .saturating_sub(1)
            .min(bins.saturating_sub(1));
        if let Some(c) = counts.get_mut(i) {
            *c += 1;
        }
    }
    json!({"edges": edges, "counts": counts})
}

/// Sums that stay `null` until something known was added.
#[derive(Default, Clone, Copy)]
struct Sum(Option<i64>);

impl Sum {
    fn add(&mut self, v: i64) {
        if v >= 0 {
            self.0 = Some(self.0.unwrap_or(0).saturating_add(v));
        }
    }

    fn json(self) -> Value {
        self.0.map_or(Value::Null, |v| json!(v))
    }
}

#[derive(Default)]
struct ModelAgg {
    calls: u64,
    errors: u64,
    input: Sum,
    output: Sum,
    cached: Sum,
    reasoning: Sum,
    dur: Vec<f64>,
    ttfc: Vec<f64>,
    tps: Vec<f64>,
}

impl ModelAgg {
    fn merge(&mut self, o: ModelAgg) {
        self.calls += o.calls;
        self.errors += o.errors;
        for (a, b) in [
            (&mut self.input, o.input),
            (&mut self.output, o.output),
            (&mut self.cached, o.cached),
            (&mut self.reasoning, o.reasoning),
        ] {
            if let Some(v) = b.0 {
                a.add(v);
            }
        }
        self.dur.extend(o.dur);
        self.ttfc.extend(o.ttfc);
        self.tps.extend(o.tps);
    }
}

#[derive(Default)]
struct GroupAgg {
    traces: u64,
    llm_calls: u64,
    tokens: Sum,
    errors: u64,
}

#[derive(Default)]
struct ToolAgg {
    calls: u64,
    errors: u64,
    dur: Vec<f64>,
}

fn top<K: Copy, V>(
    map: HashMap<K, V>,
    n: usize,
    size: impl Fn(&V) -> u64,
    name: impl Fn(K) -> Option<String>,
) -> Vec<(K, V)> {
    let mut rows: Vec<(K, V)> = map.into_iter().collect();
    rows.sort_by(|a, b| {
        size(&b.1)
            .cmp(&size(&a.1))
            .then_with(|| name(a.0).cmp(&name(b.0)))
    });
    rows.truncate(n);
    rows
}

/// The time axis: `(t0_us, step_us, n)`. The step is the smallest allowed width that needs at most `buckets`
/// buckets; buckets are aligned to multiples of the step, so the series of two requests line up.
fn axis(w: Span64, buckets: usize) -> (i64, i64, usize) {
    let buckets = buckets.clamp(1, 240);
    let span_s = ((w.to - w.from) / 1_000_000).max(60);
    for step in STEPS {
        let step_us = step * 1_000_000;
        let t0 = w.from.div_euclid(step_us) * step_us;
        let n = usize::try_from((w.to - t0).div_euclid(step_us) + 1).unwrap_or(usize::MAX);
        if n <= buckets {
            return (t0, step_us, n.max(1));
        }
    }
    // longer than the widest step covers: whole days, as many as it takes
    let mut days = (span_s / 86_400 / i64::try_from(buckets).unwrap_or(1)).max(30);
    loop {
        let step_us = days * 86_400 * 1_000_000;
        let t0 = w.from.div_euclid(step_us) * step_us;
        let n = usize::try_from((w.to - t0).div_euclid(step_us) + 1).unwrap_or(usize::MAX);
        if n <= buckets || days > 1_000_000 {
            return (t0, step_us, n.clamp(1, buckets));
        }
        days += days / 8 + 1;
    }
}

/// `YYYY-MM-DD` of a day number (days since 1970-01-01), UTC.
fn day_text(days: i64) -> String {
    // Howard Hinnant's civil-from-days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn count_rows(
    map: HashMap<Sym, u64>,
    core: &Core,
    key: &str,
    limit: usize,
    keep_null: bool,
) -> Vec<Value> {
    let mut rows: Vec<(u64, Option<&str>)> = map
        .into_iter()
        .map(|(s, n)| (n, core.sym_str(s)))
        .filter(|(_, v)| keep_null || v.is_some())
        .collect();
    rows.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.is_none().cmp(&b.1.is_none()))
            .then(a.1.cmp(&b.1))
    });
    rows.truncate(limit);
    rows.into_iter()
        .map(|(n, v)| json!({key: v, "count": n}))
        .collect()
}

/// `Analytics`.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn analytics(snap: &Snapshot, q: &AnalyticsQuery, lb: &Labels, now_us: i64) -> Value {
    let core = &*snap.core;
    let store = &core.store;
    let w = core.resolve(q.range, now_us);
    let (t0, step_us, n) = axis(w, q.buckets);
    let bucket = |ts: i64| -> usize {
        usize::try_from((ts - t0).div_euclid(step_us))
            .unwrap_or(0)
            .min(n - 1)
    };
    let in_window = |ts: i64| ts != NO_TS && ts >= w.from && ts <= w.to;
    let want =
        |name: &Option<String>, s: Sym| name.as_deref().is_none_or(|x| core.sym_str(s) == Some(x));
    #[allow(clippy::cast_precision_loss)]
    let secs = |us: i64| f_json(Some(us as f64 / 1e6));

    let mut s_traces = vec![0u64; n];
    let mut s_calls = vec![0u64; n];
    let mut s_errors = vec![0u64; n];
    let mut s_in = vec![0i64; n];
    let mut s_out = vec![0i64; n];
    let mut s_cached = vec![0i64; n];
    let mut b_dur: Vec<Vec<f64>> = vec![Vec::new(); n];
    let mut b_ttfc: Vec<Vec<f64>> = vec![Vec::new(); n];

    let (mut n_traces, mut n_tools, mut n_errors) = (0u64, 0u64, 0u64);
    let mut sessions: HashSet<u32> = HashSet::new();
    let mut models: HashMap<Sym, ModelAgg> = HashMap::new();
    // every LLM span of the window, whatever its model (the totals)
    let mut all = ModelAgg::default();
    let mut by_client: HashMap<Sym, GroupAgg> = HashMap::new();
    let mut by_kind: HashMap<Sym, GroupAgg> = HashMap::new();
    let mut tools: HashMap<Sym, ToolAgg> = HashMap::new();
    let mut tool_requests: HashMap<&str, u64> = HashMap::new();
    let mut err_types: HashMap<Sym, u64> = HashMap::new();
    let mut err_status: HashMap<i64, u64> = HashMap::new();
    let mut finish: HashMap<Sym, u64> = HashMap::new();
    let mut recent: Vec<(i64, Value)> = Vec::new();
    // one row per LLM span: (model, bucket, total tokens) for the per-model series, and what the scatter shows
    let mut calls: Vec<(Sym, usize, i64)> = Vec::new();
    let mut scatter: Vec<(u32, u32)> = Vec::new();
    let (mut in_all, mut out_all, mut gaps): (Vec<f64>, Vec<f64>, Vec<f64>) =
        (Vec::new(), Vec::new(), Vec::new());
    let mut activity: HashMap<(i64, usize), u64> = HashMap::new();
    // routing
    let mut r_requests = 0u64;
    let mut r_algorithms: HashMap<Sym, u64> = HashMap::new();
    let mut r_decisions: HashMap<(Sym, Sym, Sym), u64> = HashMap::new();
    let (mut judge_calls, mut judge_errors) = (0u64, 0u64);
    let mut judge_ms: Vec<f64> = Vec::new();
    let mut overhead_ms: Vec<f64> = Vec::new();
    let mut r_failures: HashMap<(Sym, Sym), u64> = HashMap::new();

    for (ti, t) in core.traces_in(w) {
        if !want(&q.client, t.client) || !want(&q.kind, t.kind) {
            continue;
        }
        let rows = core
            .d
            .order
            .get(t.span_at as usize..(t.span_at + t.span_n) as usize)
            .unwrap_or_default();
        let model_ok = |row: u32| -> bool {
            let Some(m) = q.model.as_deref() else {
                return true;
            };
            core.span(row)
                .and_then(|s| core.llm_of(s))
                .is_some_and(|l| core.sym_str(l.model) == Some(m))
        };
        if q.model.is_some()
            && !rows.iter().any(|&r| {
                core.span(r)
                    .is_some_and(|s| s.has(bit::IS_LLM) && in_window(s.first_us()))
                    && model_ok(r)
            })
        {
            continue;
        }
        n_traces += 1;
        sessions.insert(t.session);
        if in_window(t.t0_us) {
            s_traces[bucket(t.t0_us)] += 1;
        }
        let (gc, gk) = (
            by_client.entry(t.client).or_default(),
            by_kind.entry(t.kind).or_default(),
        );
        gc.traces += 1;
        gk.traces += 1;
        let trace_id = core.id(core.trace_id(t));
        for &r in rows {
            let Some(s) = core.span(r) else { continue };
            let ts = s.first_us();
            if !in_window(ts) {
                continue;
            }
            let failed = s.has(bit::ERROR);
            let b = bucket(ts);
            let dur = super::ids::ms_between(s.start_us, s.end_us);
            if s.has(bit::IS_TOOL) {
                n_tools += 1;
                let a = tools.entry(s.name).or_default();
                a.calls += 1;
                if failed {
                    a.errors += 1;
                }
                if let Some(d) = dur {
                    a.dur.push(d);
                }
            } else if s.has(bit::IS_LLM) {
                if !model_ok(r) {
                    continue;
                }
                let Some(l) = core.llm_of(s) else { continue };
                s_calls[b] += 1;
                s_in[b] += l.input.max(0);
                s_out[b] += l.output.max(0);
                s_cached[b] += l.cached.max(0);
                #[allow(clippy::cast_precision_loss)]
                let ttfc = (l.ttfc_us >= 0).then(|| l.ttfc_us as f64 / 1e3);
                let tps = Core::tokens_per_s(s, l);
                for m in [models.entry(l.model).or_default(), &mut all] {
                    m.calls += 1;
                    if failed {
                        m.errors += 1;
                    }
                    m.input.add(l.input);
                    m.output.add(l.output);
                    m.cached.add(l.cached);
                    m.reasoning.add(l.reasoning);
                    if let Some(d) = dur {
                        m.dur.push(d);
                    }
                    if let Some(v) = ttfc {
                        m.ttfc.push(v);
                    }
                    if let Some(v) = tps {
                        m.tps.push(v);
                    }
                }
                if let Some(d) = dur {
                    b_dur[b].push(d);
                    scatter.push((ti, r));
                }
                if let Some(v) = ttfc {
                    b_ttfc[b].push(v);
                }
                #[allow(clippy::cast_precision_loss)]
                {
                    if l.input >= 0 {
                        in_all.push(l.input as f64);
                    }
                    if l.output >= 0 {
                        out_all.push(l.output as f64);
                    }
                    if let Some(g) = l.max_chunk_gap_us() {
                        gaps.push(g as f64 / 1e3);
                    }
                }
                calls.push((l.model, b, l.total.max(0)));
                for g in [&mut *gc, &mut *gk] {
                    g.llm_calls += 1;
                    g.tokens.add(l.total);
                }
                if l.finish_reason.is_some() {
                    *finish.entry(l.finish_reason).or_insert(0) += 1;
                }
                if let Some(names) = core.sym_str(l.requested_tools) {
                    for name in names.split(',').filter(|x| !x.is_empty()) {
                        *tool_requests.entry(name).or_insert(0) += 1;
                    }
                }
                *activity
                    .entry((
                        ts.div_euclid(86_400_000_000),
                        usize::try_from(ts.rem_euclid(86_400_000_000) / 3_600_000_000)
                            .unwrap_or(0)
                            .min(23),
                    ))
                    .or_insert(0) += 1;
            } else {
                continue;
            }
            if failed {
                n_errors += 1;
                s_errors[b] += 1;
                gc.errors += 1;
                gk.errors += 1;
                *err_types.entry(s.error_type).or_insert(0) += 1;
                if s.http_status != 0 {
                    *err_status.entry(i64::from(s.http_status)).or_insert(0) += 1;
                }
                let model = core.llm_of(s).map_or(Value::Null, |l| core.sym(l.model));
                recent.push((
                    ts,
                    json!({"ts": secs(ts), "trace_id": trace_id, "span_id": core.id(s.id), "model": model, "type": core.sym(s.error_type)}),
                ));
            }
        }
        // the routing marks of this trace
        let marks = core
            .d
            .marks
            .get(t.mark_at as usize..(t.mark_at + t.mark_n) as usize)
            .unwrap_or_default();
        for &mi in marks {
            let Some(m) = store.marks.get(mi as usize) else {
                continue;
            };
            // a metric mark repeats its plain twin as a measurement: count the plain one
            if !in_window(m.ts_us) || m.nmeas > 0 {
                continue;
            }
            if m.name == lb.requested {
                r_requests += 1;
                *r_algorithms
                    .entry(mark_sym(store, m, Key::Algorithm))
                    .or_insert(0) += 1;
            } else if m.name == lb.decision {
                let route = {
                    let own = mark_sym(store, m, Key::Route);
                    if own.is_some() {
                        own
                    } else {
                        t.models.first().copied().unwrap_or(Sym::NONE)
                    }
                };
                let served = {
                    let s = mark_sym(store, m, Key::ServedModel);
                    if s.is_some() {
                        s
                    } else {
                        mark_sym(store, m, Key::SelectedModel)
                    }
                };
                *r_decisions
                    .entry((route, decision_label(store, m, lb), served))
                    .or_insert(0) += 1;
            } else if m.name == lb.llm_call {
                // the call that decides the route is the judge call
                if mark_sym(store, m, Key::CallRole) == lb.routing {
                    judge_calls += 1;
                    let outcome = mark_sym(store, m, Key::Outcome);
                    if outcome.is_some() && outcome != lb.ok {
                        judge_errors += 1;
                    }
                    if let Some(ms) = mark_num(store, m, Key::LatencyMs) {
                        judge_ms.push(ms);
                    }
                }
            } else if m.name == lb.overhead {
                if let Some(ms) = mark_num(store, m, Key::LatencyMs) {
                    overhead_ms.push(ms);
                }
            } else if m.name == lb.error {
                *r_failures
                    .entry((
                        mark_sym(store, m, Key::FailureKind),
                        mark_sym(store, m, Key::Phase),
                    ))
                    .or_insert(0) += 1;
                if let Some(st) = mark_num(store, m, Key::UpstreamStatus) {
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        *err_status.entry(st as i64).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    let n_calls = all.calls;
    let model_rows = top(
        models,
        usize::MAX,
        |m| m.calls,
        |s| core.sym_str(s).map(str::to_owned),
    );
    let model_json = |name: Value, m: &mut ModelAgg| -> Value {
        json!({
            "model": name,
            "llm_calls": m.calls,
            "errors": m.errors,
            "input_tokens": m.input.json(),
            "output_tokens": m.output.json(),
            "cached_tokens": m.cached.json(),
            "reasoning_tokens": m.reasoning.json(),
            "p50_ms": pct(&mut m.dur, 0.5),
            "p95_ms": pct(&mut m.dur, 0.95),
            "ttfc_p50_ms": pct(&mut m.ttfc, 0.5),
            "ttfc_p95_ms": pct(&mut m.ttfc, 0.95),
            "tokens_per_s_p50": pct(&mut m.tps, 0.5),
            "tokens_per_s_p95": pct(&mut m.tps, 0.95),
        })
    };
    // the 8 busiest models by name, everything else as "other"
    let named: Vec<Sym> = model_rows.iter().take(8).map(|(s, _)| *s).collect();
    let mut by_model: Vec<Value> = Vec::new();
    let mut other = ModelAgg::default();
    let has_other = model_rows.len() > 8;
    for (i, (sym, mut m)) in model_rows.into_iter().enumerate() {
        if i < 8 {
            by_model.push(model_json(core.sym(sym), &mut m));
        } else {
            other.merge(m);
        }
    }
    if has_other {
        by_model.push(model_json(json!("other"), &mut other));
    }
    let mut series_models: Vec<Value> = Vec::new();
    {
        let mut per: Vec<(Vec<u64>, Vec<i64>)> = vec![(vec![0; n], vec![0; n]); named.len() + 1];
        for &(model, b, tokens) in &calls {
            let slot = named
                .iter()
                .position(|s| *s == model)
                .unwrap_or(named.len());
            if let Some((c, tk)) = per.get_mut(slot) {
                c[b] += 1;
                tk[b] += tokens;
            }
        }
        for (i, (c, tk)) in per.into_iter().enumerate() {
            let name = match named.get(i) {
                Some(s) => core.sym(*s),
                None if has_other => json!("other"),
                None => continue,
            };
            series_models.push(json!({"model": name, "llm_calls": c, "tokens": tk}));
        }
    }

    let (mut all_dur, mut all_ttfc, mut all_tps) = (
        std::mem::take(&mut all.dur),
        std::mem::take(&mut all.ttfc),
        std::mem::take(&mut all.tps),
    );
    let spans_seen = n_calls + n_tools;
    #[allow(clippy::cast_precision_loss)]
    let totals = json!({
        "traces": n_traces,
        "sessions": sessions.len(),
        "llm_calls": n_calls,
        "tool_calls": n_tools,
        "errors": n_errors,
        "error_rate": if spans_seen == 0 { Value::Null } else { f_json(Some(((n_errors as f64 / spans_seen as f64) * 10_000.0).round() / 10_000.0)) },
        "input_tokens": all.input.json(),
        "output_tokens": all.output.json(),
        "cached_tokens": all.cached.json(),
        "reasoning_tokens": all.reasoning.json(),
        "cache_hit_ratio": match (all.cached.0, all.input.0) {
            (Some(c), Some(i)) if i > 0 => f_json(Some(((c as f64 / i as f64) * 10_000.0).round() / 10_000.0)),
            _ => Value::Null,
        },
        "p50_ms": pct(&mut all_dur, 0.5),
        "p95_ms": pct(&mut all_dur, 0.95),
        "p99_ms": pct(&mut all_dur, 0.99),
        "ttfc_p50_ms": pct(&mut all_ttfc, 0.5),
        "ttfc_p95_ms": pct(&mut all_ttfc, 0.95),
        "tokens_per_s_p50": pct(&mut all_tps, 0.5),
    });

    let ts: Vec<Value> = (0..n)
        .map(|i| secs(t0 + i64::try_from(i).unwrap_or(0) * step_us))
        .collect();
    let series = json!({
        "ts": ts,
        "traces": s_traces,
        "llm_calls": s_calls,
        "errors": s_errors,
        "input_tokens": s_in,
        "output_tokens": s_out,
        "cached_tokens": s_cached,
        "p50_ms": b_dur.iter_mut().map(|v| pct(v, 0.5)).collect::<Vec<_>>(),
        "p95_ms": b_dur.iter_mut().map(|v| pct(v, 0.95)).collect::<Vec<_>>(),
        "ttfc_p50_ms": b_ttfc.iter_mut().map(|v| pct(v, 0.5)).collect::<Vec<_>>(),
        "by_model": series_models,
    });

    let group_rows = |map: HashMap<Sym, GroupAgg>, key: &str| -> Vec<Value> {
        top(map, 50, |g| g.traces, |s| core.sym_str(s).map(str::to_owned))
            .into_iter()
            .map(|(s, g)| json!({key: core.sym(s), "traces": g.traces, "llm_calls": g.llm_calls, "tokens": g.tokens.json(), "errors": g.errors}))
            .collect()
    };

    // heat maps
    let duration_hist = histogram(&all_dur, &DURATION_EDGES);
    let edges_ms: Vec<f64> = duration_hist["edges"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    let group = n.div_ceil(48).max(1);
    let cols = n.div_ceil(group);
    let bins = edges_ms.len().saturating_sub(1);
    let mut lat = vec![vec![0u64; bins]; cols];
    for (i, durs) in b_dur.iter().enumerate() {
        for &d in durs {
            let b = edges_ms
                .partition_point(|e| *e <= d)
                .saturating_sub(1)
                .min(bins.saturating_sub(1));
            if let Some(c) = lat.get_mut(i / group).and_then(|row| row.get_mut(b)) {
                *c += 1;
            }
        }
    }
    let lat_ts: Vec<Value> = (0..cols)
        .map(|c| secs(t0 + i64::try_from(c * group).unwrap_or(0) * step_us))
        .collect();
    let (days, act) = {
        let last = activity.keys().map(|k| k.0).max();
        let first = activity.keys().map(|k| k.0).min();
        match (first, last) {
            (Some(first), Some(last)) => {
                // every day from the first with a call to the last, at most the newest 31
                let first = first.max(last - 30);
                let days: Vec<String> = (first..=last).map(day_text).collect();
                let counts: Vec<Vec<u64>> = (first..=last)
                    .map(|d| {
                        (0..24)
                            .map(|h| activity.get(&(d, h)).copied().unwrap_or(0))
                            .collect()
                    })
                    .collect();
                (days, counts)
            }
            _ => (Vec::new(), Vec::new()),
        }
    };

    // scatter: at most 600 spans, evenly spread over the list
    let stride = scatter.len().div_ceil(600).max(1);
    let scatter: Vec<Value> = scatter
        .iter()
        .step_by(stride)
        .filter_map(|&(ti, r)| {
            let s = core.span(r)?;
            let l = core.llm_of(s)?;
            let t = core.d.traces.get(ti as usize)?;
            Some(json!({
                "trace_id": core.id(core.trace_id(t)),
                "span_id": core.id(s.id),
                "ts": secs(s.first_us()),
                "duration_ms": f_json(super::ids::ms_between(s.start_us, s.end_us)),
                "input_tokens": super::snapshot::count_json(l.input),
                "output_tokens": super::snapshot::count_json(l.output),
                "model": core.sym(l.model),
                "status": span_status(s).as_str(),
            }))
        })
        .collect();

    let tools_json: Vec<Value> = top(tools, 30, |a| a.calls, |s| core.sym_str(s).map(str::to_owned))
        .into_iter()
        .map(|(s, mut a)| json!({"name": core.sym(s), "calls": a.calls, "errors": a.errors, "p50_ms": pct(&mut a.dur, 0.5), "p95_ms": pct(&mut a.dur, 0.95)}))
        .collect();

    let tool_request_rows: Vec<Value> = {
        let mut rows: Vec<(u64, &str)> = tool_requests.into_iter().map(|(k, n)| (n, k)).collect();
        rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        rows.truncate(30);
        rows.into_iter()
            .map(|(n, name)| json!({"name": name, "count": n}))
            .collect()
    };

    // routing: route -> decision -> served model
    let mut flows: HashMap<(Sym, Sym), u64> = HashMap::new();
    for (&(route, decision, served), &c) in &r_decisions {
        *flows.entry((route, decision)).or_insert(0) += c;
        *flows.entry((decision, served)).or_insert(0) += c;
    }
    let mut decisions: Vec<(u64, Value)> = r_decisions
        .iter()
        .map(|(&(route, decision, served), &c)| {
            (c, json!({"route": core.sym(route), "decision": core.sym(decision), "served_model": core.sym(served), "count": c}))
        })
        .collect();
    decisions.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.to_string().cmp(&b.1.to_string()))
    });
    let mut flow_rows: Vec<(u64, Value)> = flows
        .iter()
        .filter(|((a, b), _)| a.is_some() && b.is_some())
        .map(|(&(a, b), &c)| {
            (
                c,
                json!({"source": core.sym(a), "target": core.sym(b), "value": c}),
            )
        })
        .collect();
    flow_rows.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.to_string().cmp(&b.1.to_string()))
    });
    let mut failures: Vec<(u64, Value)> = r_failures
        .iter()
        .map(|(&(kind, phase), &c)| {
            (
                c,
                json!({"kind": core.sym(kind), "phase": core.sym(phase), "count": c}),
            )
        })
        .collect();
    failures.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.to_string().cmp(&b.1.to_string()))
    });
    let mut status_rows: Vec<(i64, u64)> = err_status.into_iter().collect();
    status_rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    recent.sort_by_key(|a| std::cmp::Reverse(a.0));
    recent.truncate(20);

    #[allow(clippy::cast_precision_loss)]
    let bucket_s = step_us as f64 / 1e6;
    json!({
        "range": w.json(),
        "bucket_s": bucket_s,
        "totals": totals,
        "series": series,
        "by_model": by_model,
        "by_client": group_rows(by_client, "client"),
        "by_kind": group_rows(by_kind, "kind"),
        "histograms": {
            "duration_ms": duration_hist,
            "ttfc_ms": histogram(&all_ttfc, &TTFC_EDGES),
            "input_tokens": histogram(&in_all, &INPUT_EDGES),
            "output_tokens": histogram(&out_all, &OUTPUT_EDGES),
            "tokens_per_s": histogram(&all_tps, &TPS_EDGES),
            "chunk_gap_ms": histogram(&gaps, &GAP_EDGES),
        },
        "heatmap": {
            "activity": {"days": days, "hours": (0..24).collect::<Vec<u8>>(), "counts": act},
            "latency": {"ts": lat_ts, "edges_ms": edges_ms, "counts": lat},
        },
        "tools": tools_json,
        "tool_requests": tool_request_rows,
        "routing": {
            "requests": r_requests,
            "by_algorithm": count_rows(r_algorithms, core, "algorithm", 50, true),
            "decisions": decisions.into_iter().map(|d| d.1).collect::<Vec<_>>(),
            "judge": {
                "calls": judge_calls,
                "errors": judge_errors,
                "p50_ms": pct(&mut judge_ms, 0.5),
                "p95_ms": pct(&mut judge_ms, 0.95),
            },
            "overhead_p50_ms": pct(&mut overhead_ms, 0.5),
            "overhead_p95_ms": pct(&mut overhead_ms, 0.95),
            "failures": failures.into_iter().map(|d| d.1).collect::<Vec<_>>(),
            "flows": flow_rows.into_iter().map(|d| d.1).collect::<Vec<_>>(),
        },
        "errors": {
            "by_type": count_rows(err_types, core, "type", 50, true),
            "by_status": status_rows.into_iter().map(|(s, c)| json!({"status": s, "count": c})).collect::<Vec<_>>(),
            "finish_reasons": count_rows(finish, core, "reason", 50, false),
            "recent": recent.into_iter().map(|r| r.1).collect::<Vec<_>>(),
        },
        "scatter": scatter,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_interpolate() {
        assert_eq!(percentile(&[], 0.5), None);
        assert_eq!(percentile(&[7.0], 0.95), Some(7.0));
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 0.5), Some(2.5));
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 0.95), Some(3.85));
        assert_eq!(percentile(&[10.0, 20.0], 0.99), Some(19.9));
    }

    #[test]
    fn histograms_cover_every_value() {
        let h = histogram(
            &[5.0, 10.0, 59_999.0, 60_000.0, 500_000.0, -3.0],
            &DURATION_EDGES,
        );
        let edges: Vec<f64> = h["edges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        let counts: Vec<u64> = h["counts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap())
            .collect();
        assert_eq!(edges.len(), counts.len() + 1);
        assert!(*edges.last().unwrap() > 500_000.0);
        assert_eq!(counts.iter().sum::<u64>(), 6);
        assert_eq!(counts[0], 2); // 5 and the negative one
        assert_eq!(counts[1], 1); // 10 belongs to [10, 25)
        let empty = histogram(&[], &TPS_EDGES);
        assert_eq!(empty["edges"].as_array().unwrap().len(), TPS_EDGES.len());
        assert_eq!(
            empty["counts"].as_array().unwrap().len(),
            TPS_EDGES.len() - 1
        );
    }

    #[test]
    fn the_time_axis_is_aligned_and_bounded() {
        let w = Span64 {
            from: 1_793_534_400_000_000 + 17_000_000,
            to: 1_793_534_400_000_000 + 86_400_000_000 + 17_000_000,
        };
        let (t0, step, n) = axis(w, 60);
        assert_eq!(step, 1800 * 1_000_000);
        assert_eq!(t0 % step, 0);
        assert!(t0 <= w.from && t0 + (n as i64) * step > w.to && n <= 60);
        for buckets in [12, 60, 240] {
            for span_s in [1i64, 60, 3600, 86_400, 30 * 86_400, 3 * 365 * 86_400] {
                let w = Span64 {
                    from: 1_793_534_400_000_000,
                    to: 1_793_534_400_000_000 + span_s * 1_000_000,
                };
                let (t0, step, n) = axis(w, buckets);
                assert!(n >= 1 && n <= buckets.max(1), "{buckets} {span_s} -> {n}");
                assert!(t0 <= w.from && t0 + (n as i64) * step > w.to);
            }
        }
        assert_eq!(day_text(0), "1970-01-01");
        assert_eq!(day_text(20_758), "2026-11-01");
        assert_eq!(day_text(19_782), "2024-02-29");
    }
}
