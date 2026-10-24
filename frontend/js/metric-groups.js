// Pure helpers for the generic system view (no DOM, unit-tested).
import { t } from './i18n.js';

/** Human label for a metric key: i18n `metric.<key>` when present, else the key itself. */
export function metricLabel(key) { const k = `metric.${key}`; const s = t(k); return s === k ? key : s; }
export const labelText = (labels) => Object.values(labels || {}).join(' / ');
/** Distinct metric keys in a snapshot, in first-seen order, each with its points. */
export function groupPoints(snapshot) {
  const m = new Map();
  for (const p of snapshot?.points || []) { if (!m.has(p.key)) m.set(p.key, []); m.get(p.key).push(p); }
  return m;
}
/** The chart keys to draw: the descriptor's preferred keys that exist; when none exist, the first few non-table keys. */
export function pickChartKeys(desc, grouped) {
  const wanted = (desc.charts || []).filter((k) => grouped.has(k));
  if (wanted.length || Array.isArray(desc.charts)) return wanted;   // an explicit list (even empty) is respected
  return [...grouped.keys()].filter((k) => !(desc.tables || []).includes(k)).slice(0, 3);
}
