// Pure logic behind the Data Designer runs list and records table (views/datadesigner.js). No DOM, no imports:
// node --test covers it directly (tests/dd-records.test.mjs). Shapes follow the contract addendum:
// records endpoint -> {run_id, total, columns, rows, truncated}; run events -> kind "dd.run" with metadata only.
export const RECORDS_LIMIT = 50;   // server-driven page size (contract: limit=50, offset pages)
export const CELL_MAX = 120;       // visible characters per cell; the full text goes into the title attribute

const VERDICT_KINDS = { succeeded: 'ok', passed: 'ok', failed: 'bad', error: 'bad', incomplete: 'warn', interrupted: 'warn', running: 'info' };
export const verdictKind = (v) => VERDICT_KINDS[v] || 'muted';
export const isLiveVerdict = (v) => v === 'running';

/** Events (any order) -> one entry per run, newest first. The latest event per run_id wins; earlier
 *  fields are kept when a later event omits them (a finish event may carry fewer fields than the start). */
export function runsFromEvents(events, { kind = 'dd.run', cap = 200 } = {}) {
  const byId = new Map();
  for (const e of events || []) {
    if (!e || e.kind !== kind) continue;
    const d = e.data || {};
    const id = String(d.run_id || '');
    if (!id) continue;
    const prev = byId.get(id);
    // Input order is not guaranteed (the UI's list is newest-first): only a newer event may overwrite.
    if (prev && e.ts < prev.ts) continue;
    byId.set(id, {
      id, ts: e.ts, summary: e.summary || '',
      verdict: d.verdict || prev?.verdict || 'running',
      mode: d.mode ?? prev?.mode,
      script: d.script ?? prev?.script,
      num_records: d.num_records ?? prev?.num_records,
      dataset_name: d.dataset_name ?? prev?.dataset_name,
      destination: d.destination ?? prev?.destination,
      format: d.format ?? prev?.format,
      duration_ms: d.duration_ms ?? prev?.duration_ms,
      error: d.error ?? prev?.error,
    });
  }
  return [...byId.values()].sort((a, b) => (b.ts - a.ts) || (a.id < b.id ? -1 : 1)).slice(0, cap);
}

/** The subset of runs whose latest verdict is still "running" (the live create banner / progress line). */
export const liveRuns = (runs) => runs.filter((r) => isLiveVerdict(r.verdict));

/** Page window over a server-paginated list: clamps the offset into [0, total-1] and derives the flags
 *  the pager buttons need. total 0 -> offset 0 and an empty window (the UI shows the empty state). */
export function pageMeta(total, limit, offset) {
  const totalN = Math.max(0, Math.floor(Number(total) || 0));
  const limitN = Math.max(1, Math.floor(Number(limit) || 1));
  const off = Math.min(Math.max(0, Math.floor(Number(offset) || 0)), Math.max(0, totalN - 1));
  return {
    offset: off,
    from: totalN ? off + 1 : 0,
    to: Math.min(off + limitN, totalN),
    total: totalN,
    hasPrev: off > 0,
    hasNext: off + limitN < totalN,
    pages: Math.ceil(totalN / limitN),
  };
}

/** Column show/hide: hidden is a Set of column names. */
export const visibleColumns = (columns, hidden) => (columns || []).filter((c) => !hidden.has(c));
export const toggleHidden = (hidden, col) => {
  const next = new Set(hidden);
  if (next.has(col)) next.delete(col); else next.add(col);
  return next;
};

/** A record value as display text: null stays "null", objects/arrays are JSON, everything else String(). */
export function cellText(v) {
  if (v === null) return 'null';
  if (v === undefined) return '';
  if (typeof v === 'object') { try { return JSON.stringify(v); } catch { return String(v); } }
  return String(v);
}

/** Truncate for display (append an ellipsis); the caller puts `full` into the title attribute and the
 *  per-row raw-JSON toggle shows everything anyway. */
export function truncateCell(text, max = CELL_MAX) {
  const s = String(text ?? '');
  return s.length > max ? { text: `${s.slice(0, Math.max(1, max - 1))}…`, full: s } : { text: s, full: s };
}

/** Pretty JSON for the per-row raw toggle. */
export const rowJson = (row) => {
  try { return JSON.stringify(row, null, 2); } catch { return String(row); }
};
