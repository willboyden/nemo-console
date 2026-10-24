import test from 'node:test';
import assert from 'node:assert/strict';
import { findPoint, valueOf, sumOf, groupBy, counterRate, keyWith, relativeMinutes } from '../js/metrics.js';
import { niceTicks, scaleLinear, linePath, lineChart, shareBars, legend, sparkline } from '../js/charts.js';
import { escapeHtml } from '../js/escape.js';

// Tokenise: every tag must be one of the chart tags with only allow-listed, double-quoted attributes whose values contain no raw < > ".
const TAGS = /<\/?(svg|g|text|rect|path|line|circle|title|ul|li|i)((?:\s+[A-Za-z0-9-]+="[^"<>]*")*)\s*\/?>/g;
const ATTRS = new Set(['class', 'viewBox', 'role', 'aria-label', 'aria-hidden', 'preserveAspectRatio', 'x', 'y', 'x1', 'x2', 'y1', 'y2', 'cx', 'cy', 'r', 'd', 'width', 'height', 'rx', 'text-anchor']);
function assertOnlyTrustedMarkup(html, payload) {
  for (const m of html.matchAll(TAGS)) for (const a of m[2].matchAll(/\s+([A-Za-z0-9-]+)="[^"]*"/g)) assert.ok(ATTRS.has(a[1]), `attribute ${a[1]} for payload ${payload}`);
  const rest = html.replace(TAGS, '');
  assert.ok(!/[<>]/.test(rest), `raw angle bracket left over for payload ${payload}: ${rest.slice(0, 80)}`);
}
const snap = { points: [{ key: 'sy.tokens', value: 5, labels: { kind: 'prompt' } }, { key: 'sy.tokens', value: 2, labels: { kind: 'completion' } }, { key: 'dyn.ttft_ms', value: 9, labels: { q: 'p95' } }] };
test('findPoint matches labels; valueOf/sumOf/groupBy', () => {
  assert.equal(valueOf(snap, 'dyn.ttft_ms', { q: 'p95' }), 9); assert.equal(valueOf(snap, 'dyn.ttft_ms', { q: 'p50' }), undefined);
  assert.equal(sumOf(snap, 'sy.tokens'), 7); assert.equal(sumOf(snap, 'nope'), undefined); assert.deepEqual(groupBy(snap, 'sy.tokens', 'kind'), { prompt: 5, completion: 2 });
  assert.equal(findPoint(null, 'x'), undefined);
});
test('counterRate: per-second deltas, drops counter resets and zero dt', () => {
  assert.deepEqual(counterRate([[0, 0], [10, 50], [20, 150], [30, 10], [40, 30]]), [[10, 5], [20, 10], [40, 2]]);
  assert.deepEqual(counterRate([[0, 1], [0, 2]]), []); assert.deepEqual(counterRate(undefined), []);
});
test('keyWith and relativeMinutes', () => {
  assert.equal(keyWith('dyn.ttft_ms', { q: 'p95' }), 'dyn.ttft_ms{q=p95}'); assert.equal(keyWith('dyn.up'), 'dyn.up'); assert.equal(keyWith('a', { x: '1', y: '2' }), 'a{x=1,y=2}');
  assert.deepEqual(relativeMinutes([[0, 1], [60, 2], [120, 3]]), [[-2, 1], [-1, 2], [0, 3]]);
});
test('niceTicks / scaleLinear / linePath basics', () => {
  assert.deepEqual(niceTicks(0, 100, 5), [0, 20, 40, 60, 80, 100]); assert.equal(scaleLinear(0, 10, 0, 100)(5), 50); assert.equal(scaleLinear(3, 3, 0, 10)(3), 5); assert.equal(linePath([[0, 1], [2.34, 3]]), 'M0 1L2.3 3');
});
test('lineChart: empty is safe, never NaN, has an aria-label, names are escaped', () => {
  assert.match(lineChart([], { ariaLabel: 'TTFT' }), /aria-label="TTFT"/);
  const svg = lineChart([{ name: '<img src=x onerror=1>', points: [[-1, 1], [0, 2]] }], { yFormat: (v) => `${v}<`, ariaLabel: 'a"b' });
  assert.ok(!/<img/.test(svg)); assert.match(svg, /&lt;img/); assert.match(svg, /aria-label="a&quot;b"/); assert.ok(!/NaN/.test(svg));
});
test('shareBars: every bar has an aria-label with count and percent; labels are escaped; no NaN', () => {
  const svg = shareBars([{ name: '<x>', value: 3, pct: 75 }, { name: 'b', value: 1, pct: 25 }], { unit: 'requests', ariaLabel: 'by route' });
  assert.equal((svg.match(/<rect[^>]*aria-label="/g) || []).length, 2); assert.match(svg, /aria-label="&lt;x&gt;: 3 requests \(75\.0%\)"/); assert.ok(!svg.includes('<x>')); assert.ok(!/NaN/.test(svg));
  assert.match(shareBars([], { ariaLabel: 'e' }), /aria-label="e"/); assert.ok(!/NaN/.test(shareBars([{ name: 'z', value: 0, pct: 0 }])));
});
test('legend and sparkline escape/handle empties', () => { assert.ok(!/<b>/.test(legend([{ name: '<b>x</b>' }]))); assert.match(sparkline([1]), /aria-hidden/); });
test('XSS corpus: escapeHtml neutralises every payload before it can reach setTrustedHtml', () => {
  const corpus = ['<script>alert(1)</script>', '"><img src=x onerror=alert(1)>', "'><svg onload=alert(1)>", '</text><script>1</script>', '&lt;b&gt;', 'javascript:alert(1)', '<a href="x" onclick="y">', '\u0000<b>'];
  for (const p of corpus) {
    const e = escapeHtml(p);
    assert.ok(!/[<>"']/.test(e), p);
    for (const svg of [lineChart([{ name: p, points: [[0, 1], [1, 2]] }], { ariaLabel: p, yFormat: () => p, xFormat: () => p }), shareBars([{ name: p, value: 1, pct: 100 }], { ariaLabel: p }), legend([{ name: p }])]) assertOnlyTrustedMarkup(svg, p);
  }
});
test('lineChart: the top y tick always covers the data maximum (no points drawn above the plot)', () => {
  const svg = lineChart([{ name: 'a', points: [[0, 5], [1, 45]] }], { height: 200 });
  const ys = [...svg.matchAll(/<circle class="hit[^"]*" cx="[\d.]+" cy="([\d.]+)"/g)].map((m) => Number(m[1]));
  assert.ok(ys.length === 2 && ys.every((y) => y >= 10 - 0.01), `points must lie inside the plot, got ${ys}`);
  assert.match(svg, />60</);
});

test('keyWith sorts labels like the backend series_name()', async () => {
  const { keyWith } = await import('../js/metrics.js');
  assert.equal(keyWith('dcgm.gpu_temp_c', { model: 'RTX-PRO-6000', gpu: '0' }), 'dcgm.gpu_temp_c{gpu=0,model=RTX-PRO-6000}');
  assert.equal(keyWith('sy.requests_total', {}), 'sy.requests_total');
});
