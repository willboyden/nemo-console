// Pure formatting helpers (no DOM).
export const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
const isNum = (n) => typeof n === 'number' && Number.isFinite(n);

export function fmtBytes(n, digits = 1) {
  if (!isNum(n)) return '–';
  const u = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let i = 0, v = Math.abs(n);
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return `${n < 0 ? '-' : ''}${i === 0 ? v.toFixed(0) : v.toFixed(digits)} ${u[i]}`;
}
export const fmtInt = (n) => (isNum(n) ? Math.round(n).toLocaleString('en-US') : '–');
export function fmtCompact(n) {
  if (!isNum(n)) return '–';
  const a = Math.abs(n);
  if (a >= 1e9) return `${(n / 1e9).toFixed(a >= 1e10 ? 0 : 1)}B`;
  if (a >= 1e6) return `${(n / 1e6).toFixed(a >= 1e7 ? 0 : 1)}M`;
  if (a >= 1e3) return `${(n / 1e3).toFixed(a >= 1e4 ? 0 : 1)}k`;
  return String(Math.round(n));
}
export const fmtPct = (p, d = 1) => (isNum(p) ? `${p.toFixed(d)}%` : '–');
export const fmtRatio = (r, d = 0) => (isNum(r) ? `${(r * 100).toFixed(d)}%` : '–');
/** Milliseconds -> "230 ms" / "1.42 s". */
export const fmtMs = (ms) => (isNum(ms) ? (ms < 1000 ? `${ms < 10 ? ms.toFixed(1) : ms.toFixed(0)} ms` : `${(ms / 1000).toFixed(2)} s`) : '–');
export function fmtDuration(sec) {
  if (!isNum(sec) || sec < 0) return '–';
  sec = Math.round(sec);
  const d = Math.floor(sec / 86400), h = Math.floor((sec % 86400) / 3600), m = Math.floor((sec % 3600) / 60), s = sec % 60;
  if (d) return `${d}d ${h}h`;
  if (h) return `${h}h ${String(m).padStart(2, '0')}m`;
  if (m) return `${m}m ${String(s).padStart(2, '0')}s`;
  return `${s}s`;
}
const toMs = (ts) => (typeof ts === 'number' ? (ts < 1e12 ? ts * 1000 : ts) : Date.parse(ts));
export function timeAgo(ts, now = Date.now()) {
  const t = toMs(ts);
  if (!isNum(t)) return '–';
  const s = Math.max(0, Math.round((now - t) / 1000));
  if (s < 5) return 'just now';
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}
export function fmtDate(ts) { const t = toMs(ts); return isNum(t) ? new Date(t).toISOString().replace('T', ' ').slice(0, 19) : '–'; }
export function fmtTime(ts) { const t = toMs(ts); return isNum(t) ? new Date(t).toISOString().slice(11, 19) : '–'; }
export function fmtClock(ts) {
  const d = new Date(toMs(ts));
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}
/** First 8 hex chars of a sha256, for compact display. */
export const shortSha = (s) => (typeof s === 'string' && s ? s.slice(0, 8) : '–');
export function debounce(fn, ms) {
  let id;
  const d = (...a) => { clearTimeout(id); id = setTimeout(() => fn(...a), ms); };
  d.cancel = () => clearTimeout(id);
  return d;
}
/** Clamp a user-typed max_tokens into the contract's 1..4096 range; NaN -> fallback. */
export function clampMaxTokens(v, fallback = 256) {
  const n = parseInt(v, 10);
  return Number.isFinite(n) ? clamp(n, 1, 4096) : fallback;
}

/** Axis tick: whole numbers stay whole, fractions get at most 2 decimals with trailing zeros trimmed (0.25, not 0.3). */
export const fmtTick = (v) => (Number.isFinite(v) ? (Number.isInteger(v) ? String(v) : String(+v.toFixed(2))) : '–');
