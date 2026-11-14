// Pure formatting helpers for trace data (no DOM). Absent data is null and prints as an en dash, never as 0.
import { fmtCompact, fmtInt, fmtMs } from '../format.js';
const isNum = (n) => typeof n === 'number' && Number.isFinite(n);

/** Shorten an identifier for tables: first 8 and last 4 characters around an ellipsis; short ids stay whole. */
export function shortId(id, head = 8, tail = 4) {
  const s = String(id ?? '');
  return s.length > head + tail + 1 ? `${s.slice(0, head)}…${s.slice(-tail)}` : s || '–';
}
/** `MM-DD HH:MM:SS` in UTC from epoch seconds. */
export const fmtStamp = (s) => (isNum(s) ? new Date(s * 1000).toISOString().slice(5, 19).replace('T', ' ') : '–');
/** Duration in ms, null-safe: 230 ms, 1.42 s, and minutes above a minute. */
export function fmtDur(ms) {
  if (!isNum(ms)) return '–';
  if (ms < 60000) return fmtMs(ms);
  const s = Math.round(ms / 1000);
  return s < 3600 ? `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, '0')}s` : `${Math.floor(s / 3600)}h ${String(Math.floor((s % 3600) / 60)).padStart(2, '0')}m`;
}
export const fmtTok = (n) => (isNum(n) ? (Math.abs(n) >= 10000 ? fmtCompact(n) : fmtInt(n)) : '–');
export const fmtRate = (v) => (isNum(v) ? `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} tok/s` : '–');
export const fmtPercent = (r, d = 0) => (isNum(r) ? `${(r * 100).toFixed(d)}%` : '–');

/** Segments of a token bar: cached is a part of input, so the bar is [uncached input, cached input, output]. Fractions of the total. */
export function tokenSegments(input, output, cached) {
  const inp = isNum(input) ? Math.max(0, input) : 0,
    out = isNum(output) ? Math.max(0, output) : 0,
    c = isNum(cached) ? clampTo(cached, 0, inp) : 0;
  const total = inp + out;
  if (total <= 0) return { total: 0, uncached: 0, cached: 0, output: 0 };
  return { total, uncached: (inp - c) / total, cached: c / total, output: out / total };
}
const clampTo = (v, lo, hi) => Math.min(hi, Math.max(lo, v));

/** Share of a value among the maximum of a set (for inline bars): 0..1, 0 for absent data. */
export const shareOf = (v, max) => (isNum(v) && isNum(max) && max > 0 ? Math.min(1, Math.max(0, v / max)) : 0);

/** Sort comparator on a key with nulls always last, whatever the direction. */
export function byKey(key, dir = 'asc') {
  const m = dir === 'desc' ? -1 : 1;
  return (a, b) => {
    const x = typeof key === 'function' ? key(a) : a[key],
      y = typeof key === 'function' ? key(b) : b[key];
    const nx = x == null || Number.isNaN(x),
      ny = y == null || Number.isNaN(y);
    if (nx || ny) return nx && ny ? 0 : nx ? 1 : -1;
    return (x < y ? -1 : x > y ? 1 : 0) * m;
  };
}

/** Bar category for a span: agent | turn | llm | tool | other. `custom` joins `turn` (both are caller-defined scopes). */
export function categoryOf(span) {
  const c = String(span?.category ?? '').toLowerCase();
  if (c === 'agent') return 'agent';
  if (c === 'turn' || c === 'custom' || c === 'function' || c === 'task') return 'turn';
  if (c === 'llm') return 'llm';
  if (c === 'tool') return 'tool';
  return 'other';
}
/** Whether a mark is a routing decision (drawn differently from other marks). */
export const isRoutingMark = (m) => /rout|switchyard/i.test(`${m?.name ?? ''} ${m?.schema ?? ''}`) || !!(m?.data && (m.data.decision || m.data.route));
