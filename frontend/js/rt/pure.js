// Pure helpers of the explorer tabs (no DOM, so Node tests import them): facet shapes, index progress, capture coverage,
// the explanation of a failed content request and the pretty-printing of tool arguments.
import { t } from '../i18n.js';
import { describeError } from '../errors.js';

/**
 * Facets are pinned as [{value, count}] (value null = not set); plain lists and {value: count} maps are read too.
 * Returns [{value: string|null, count: number|null}].
 */
export function normalizeFacet(f) {
  if (!f) return [];
  if (Array.isArray(f)) return f.map((x) => (typeof x === 'object' && x ? { value: x.value ?? x.name ?? x.key ?? null, count: x.count ?? null } : { value: String(x), count: null }));
  if (typeof f === 'object') return Object.entries(f).map(([value, count]) => ({ value, count: typeof count === 'number' ? count : null }));
  return [];
}

/** Progress of an index build: bytes read over bytes present across the files, 0..1 (null when nothing is known). */
export function buildProgress(index) {
  const files = index?.files || [];
  const total = files.reduce((a, f) => a + (f.bytes || 0), 0);
  if (!total) return null;
  return Math.min(1, files.reduce((a, f) => a + Math.min(f.bytes || 0, f.scanned_bytes || 0), 0) / total);
}

/** How many capture levels are switched on, and how many are in each state. */
export function coverage(levels) {
  const on = levels.filter((l) => l.enabled).length;
  const by = {};
  for (const l of levels) by[l.state] = (by[l.state] ?? 0) + 1;
  return { on, total: levels.length, by };
}

/** A plain-language explanation for a failed content request (the contract's: 403 content_disabled / other 403, 404, 409). */
export function contentProblem(e) {
  if (e?.status === 403 && e.code === 'content_disabled') return { kind: 'warn', title: t('rt.content.err_disabled_title'), text: t('rt.content.err_disabled') };
  if (e?.status === 403) return { kind: 'warn', title: t('rt.content.err_admin_title'), text: t('rt.content.err_admin') };
  if (e?.status === 409) return { kind: 'info', title: t('rt.content.err_unavailable_title'), text: t('rt.content.err_unavailable') };
  if (e?.status === 404) return { kind: 'info', title: t('rt.content.err_missing_title'), text: t('rt.content.err_missing') };
  return { kind: 'bad', title: t('common.error'), text: describeError(e) };
}

/** Pretty-print a tool argument when it is JSON; any other text is returned as it is. */
export function prettyArgs(text) {
  if (typeof text !== 'string' || !/^\s*[{[]/.test(text)) return text ?? '';
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text;
  }
}

const isObj = (v) => v && typeof v === 'object' && !Array.isArray(v);
const present = (v) => v != null && !(Array.isArray(v) && !v.length);

/** The trace endpoints of a capture document (the OTLP trace exporters), [] when none. */
export const traceEndpoints = (levels) => (levels || []).find((l) => l.id === 'otel_traces')?.detail?.endpoints ?? [];

/**
 * Turn one capture level into typed facts the Capture tab draws (pinned shapes: docs/ARCHITECTURE.md 9.12). Known keys get
 * a type (sinks, endpoints, derived, mode, chips, ms, bool, text, ts, count, bytes); any other key falls back to 'generic'
 * so a newer backend's keys still show. A null value is skipped. Warnings: logs or metrics that are `derived` (no endpoint of
 * their own, so exported once per trace endpoint) while there is more than one trace endpoint are a duplicate export.
 * Returns {configured: Fact[], observed: Fact[], warnings: [{code, n}]}.
 */
export function describeLevel(level, levels = []) {
  const detail = isObj(level?.detail) ? level.detail : {},
    seen = isObj(level?.observed) ? level.observed : {};
  const configured = [],
    observed = [],
    warnings = [];
  const nTrace = traceEndpoints(levels).length;
  const known = new Set();
  const take = (key, make) => {
    known.add(key);
    if (present(detail[key])) configured.push(make(detail[key]));
  };
  switch (level?.id) {
    case 'atof':
      take('sinks', (items) => ({ type: 'sinks', items }));
      break;
    case 'atif':
      take('agent_name', (value) => ({ type: 'text', key: 'agent_name', value }));
      break;
    case 'otel_traces':
      take('endpoints', (items) => ({ type: 'endpoints', items }));
      break;
    case 'otel_logs':
    case 'otel_metrics': {
      take(level.id === 'otel_logs' ? 'minimum_severity' : 'export_interval_ms', (value) =>
        level.id === 'otel_logs' ? { type: 'text', key: 'minimum_severity', value } : { type: 'ms', key: 'export_interval_ms', value },
      );
      known.add('derived');
      known.add('endpoints');
      if (detail.derived === true) {
        configured.push({ type: 'derived', endpoints: nTrace, targets: detail.endpoints ?? [] });
        if (nTrace > 1) warnings.push({ code: 'dup_export', n: nTrace });
      } else if (present(detail.endpoints)) configured.push({ type: 'endpoints', items: detail.endpoints });
      break;
    }
    case 'payloads':
      take('mode', (value) => ({ type: 'mode', value }));
      break;
    case 'redaction':
      take('detectors', (items) => ({ type: 'chips', key: 'detectors', items }));
      break;
    case 'resource_metrics':
      take('polling', (value) => ({ type: 'bool', key: 'polling', value }));
      take('interval_ms', (value) => ({ type: 'ms', key: 'interval_ms', value }));
      break;
    default:
  }
  for (const [key, value] of Object.entries(detail)) if (!known.has(key) && present(value)) configured.push({ type: 'generic', key, value });
  const OBS = { last_ts: 'ts', events: 'count', files: 'count', trajectories: 'count', bytes: 'bytes' };
  for (const [key, value] of Object.entries(seen)) if (present(value)) observed.push({ type: OBS[key] ?? 'generic', key, value });
  return { configured, observed, warnings };
}
