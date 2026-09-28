// Share-percent math for the routing charts. Accepts {name: n}, [{name, value|calls|count}] or [[name, n]].
export function normalizeCounts(input) {
  let rows = [];
  if (Array.isArray(input)) {
    rows = input.map((r) => (Array.isArray(r) ? { name: String(r[0]), value: Number(r[1]) }
      : { name: String(r.name ?? r.route ?? r.model ?? r.tier ?? r.key ?? ''), value: Number(r.value ?? r.calls ?? r.count ?? r.requests ?? 0) }));
  } else if (input && typeof input === 'object') {
    rows = Object.entries(input).map(([name, v]) => ({ name, value: Number(typeof v === 'object' && v !== null ? (v.calls ?? v.count ?? v.requests ?? v.value ?? 0) : v) }));
  }
  return rows.filter((r) => Number.isFinite(r.value) && r.value >= 0).map((r) => ({ ...r, name: r.name === '' ? '(none)' : r.name }));
}

/** rows -> [{name, value, pct}] sorted descending; pct is exact (0..100), 0 when the total is 0. */
export function sharePercents(input) {
  const rows = normalizeCounts(input);
  const total = rows.reduce((s, r) => s + r.value, 0);
  return rows.map((r) => ({ ...r, pct: total > 0 ? (r.value / total) * 100 : 0 })).sort((a, b) => b.value - a.value || a.name.localeCompare(b.name));
}

/** Displayed percentages that sum to exactly 100.0 (largest-remainder on tenths). */
export function roundedShares(input, decimals = 1) {
  const rows = sharePercents(input);
  if (!rows.length || rows.every((r) => r.value === 0)) return rows.map((r) => ({ ...r, label: 0 }));
  const f = 10 ** decimals, target = 100 * f;
  const scaled = rows.map((r) => r.pct * f), floors = scaled.map(Math.floor);
  let rest = target - floors.reduce((a, b) => a + b, 0);
  const order = scaled.map((s, i) => ({ i, frac: s - floors[i] })).sort((a, b) => b.frac - a.frac);
  for (const o of order) { if (rest <= 0) break; floors[o.i]++; rest--; }
  return rows.map((r, i) => ({ ...r, label: floors[i] / f }));
}

/** Keep the top n rows, folding the rest into one "other" row so the chart stays legible. */
export function topN(input, n, otherName = 'other') {
  const rows = sharePercents(input);
  if (rows.length <= n) return rows;
  const head = rows.slice(0, n), tail = rows.slice(n);
  const value = tail.reduce((s, r) => s + r.value, 0), total = rows.reduce((s, r) => s + r.value, 0);
  return [...head, { name: otherName, value, pct: total ? (value / total) * 100 : 0, other: true }];
}

export const errorRate = (errors, requests) => (requests > 0 ? (errors / requests) * 100 : 0);
