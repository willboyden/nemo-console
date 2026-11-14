// Browser checks of the wave-1 visualizations (docs/ARCHITECTURE.md addendum 9.13): the series charts on every generic page
// that has them (rates, the 7d window, "show max", the line picker kept in the URL hash) and the run history of the eight
// run-based pages (verdict timeline, trend, one A/B compare, a filter, the selection in the URL hash).
//
// It runs against the FRONTEND MOCK SERVER (dev/mock-server.mjs, started by e2e/run.mjs): the hermetic real backend has no runs and
// no metrics, so on it these pages only show their empty states (which the first pass of run.mjs already opens). Like
// e2e/relay-traces.mjs it asserts that things render and respond, not what they say.
//
// ctx = { base, evaluate(expr), settle(), sleep(ms), fail(msg), route(name) }   (see e2e/run.mjs, which builds it)

const q = (sel) => `document.querySelectorAll(${JSON.stringify(sel)}).length`;

/** Poll `expr` (a JS expression) until it is truthy; resolves its last value, null on timeout. */
async function until(ctx, expr, ms = 20000) {
  const t0 = Date.now();
  for (;;) {
    const v = await ctx.evaluate(`(() => { try { return (${expr}) || null; } catch { return null; } })()`);
    if (v) return v;
    if (Date.now() - t0 > ms) return null;
    await ctx.sleep(150);
  }
}

async function open(ctx, hash, name) {
  ctx.route(name);
  await ctx.evaluate(`location.hash = ${JSON.stringify(hash)}; true`);
  await ctx.sleep(250);
  await ctx.settle();
}

/** The mock's metric-series route answers the documented shape, and refuses what the contract refuses. */
async function checkRoute(ctx) {
  ctx.route('series route (mock)');
  const r = await ctx.evaluate(`(async () => {
    const get = async (qs) => { const x = await fetch('/api/v1/systems/dcgm/metrics/series?' + qs); return [x.status, await x.json()]; };
    const [s7, a7] = await get('keys=dcgm.gpu_util&window=7d&split=1');
    const [s0, a0] = await get('keys=dcgm.gpu_util&window=30d');
    return { s7, shape: Object.keys(a7).sort().join(), step: a7.step_s, lines: a7.series.length, s0, code: a0.error && a0.error.code };
  })()`);
  if (r.s7 !== 200 || r.shape !== 'from,series,step_s,to,truncated,window' || r.step !== 600 || r.lines < 1) ctx.fail(`the series route answered ${JSON.stringify(r)}`);
  if (r.s0 !== 422 || r.code !== 'invalid_request') ctx.fail(`an unknown window is not a 422 invalid_request: ${JSON.stringify(r)}`);
}

/** Series charts on a generic page: cards, text alternatives, the 7d window, show max, and (dcgm) the line picker. */
async function visitCharts(ctx, system, { picker = false } = {}) {
  const need = async (expr, what, ms) => {
    const v = await until(ctx, expr, ms);
    if (!v) ctx.fail(`${what}`);
    return v;
  };
  await open(ctx, `#/${system}`, `${system} › charts (mock)`);
  await need(`${q('.series-card svg.tschart')} >= 1`, 'the page draws at least one series chart', 30000);
  await need(`[...document.querySelectorAll('svg.tschart')].every((s) => s.getAttribute('aria-label') && s.querySelector('title'))`, 'every series chart has an aria-label and a title');
  await need(`${q('.series-card details.chart-data')} === ${q('.series-card')}`, 'every chart card offers a data table as its text alternative');
  await need(`[...document.querySelectorAll('select[aria-label="Time window"] option')].map((o) => o.value).join() === '15m,1h,24h,7d'`, 'the window picker offers 15m, 1h, 24h and 7d');
  if (picker) {
    // six GPUs: more than four lines, so a picker with the busiest four shown
    await need(`${q('.series-picker')} >= 1 && document.querySelector('.series-picker [role=status]').textContent.includes('4 of 6')`, 'six lines show a picker with the busiest 4 on');
    await ctx.evaluate(`[...document.querySelector('.series-picker').querySelectorAll('.series-chip')].find((b) => b.getAttribute('aria-pressed') === 'false').click(); true`);
    await need(`/[?&]s\\.dcgm\\.gpu_util=/.test(location.hash)`, 'choosing a line writes the choice into the URL hash');
    await need(`document.querySelector('.series-picker [role=status]').textContent.includes('5 of 6')`, 'the chosen line is added');
  }
  // 7d window and show max live in the URL
  await ctx.evaluate(`(() => { const s = document.querySelector('select[aria-label="Time window"]'); s.value = '7d'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
  await need(`location.hash.includes('window=7d')`, 'choosing 7d writes the window into the URL');
  await need(`[...document.querySelectorAll('.series-card .card-head span')].some((e) => /10 min per point/.test(e.textContent))`, 'the 7d window draws ten-minute points', 30000);
  await ctx.evaluate(`document.querySelector('#main .check input').click(); true`);
  await need(`location.hash.includes('max=1')`, 'ticking Show max writes it into the URL');
  await need(`${q('svg.tschart path.ts-band')} >= 1`, 'with Show max on, a gauge chart draws its band', 20000);
  const rate = system === 'flare' ? await ctx.evaluate(`[...document.querySelectorAll('.series-card .card-head span')].some((e) => /per (second|minute)/.test(e.textContent))`) : true;
  if (!rate) ctx.fail('a counter (a key ending in _total) is not drawn as a rate: no "per second" / "per minute" caption');
  // a deep link restores the choices
  const deep = await ctx.evaluate('location.hash');
  await open(ctx, '#/overview', `${system} › leave`);
  await open(ctx, deep, `${system} › charts deep link (mock)`);
  await need(`document.querySelector('select[aria-label="Time window"]').value === '7d' && document.querySelector('#main .check input').checked`, 'a deep link restores the 7d window and Show max');
  if (picker) await need(`document.querySelector('.series-picker [role=status]').textContent.includes('5 of 6')`, 'a deep link restores the picked lines');
}

/** The run history: timeline, trend, one A/B compare, a filter, and the selection kept in the URL. */
async function visitHistory(ctx, system) {
  const need = async (expr, what, ms) => {
    const v = await until(ctx, expr, ms);
    if (!v) ctx.fail(`${what}`);
    return v;
  };
  await open(ctx, `#/${system}?tab=history`, `${system} › history (mock)`);
  await need(`${q('.rh-mark:not(.mini)')} >= 10`, 'the verdict timeline has at least 10 runs', 30000);
  await need(`${q('.rh-card')} === 3`, 'the history has its three cards: timeline, trend and compare');
  await need(`${q('svg.rhchart')} === 1 && document.querySelector('svg.rhchart').getAttribute('aria-label')`, 'the trend chart is drawn and has an aria-label');
  await need(`${q('.rh-mark.bad')} >= 1 && ${q('.rh-mark.ok')} >= 1`, 'the timeline shows good and failed runs (colour and glyph)');
  await need(`[...document.querySelectorAll('.rh-mark:not(.mini)')].every((b) => b.getAttribute('aria-label') && b.textContent.trim())`, 'every mark has an aria-label and a glyph');
  // select run A (click), then run B (shift+click): one A/B compare
  await ctx.evaluate(`(() => { const ok = [...document.querySelectorAll('.rh-mark.ok:not(.mini)')]; ok.at(-1).click(); return true; })()`);
  await need(`/[?&]run=/.test(location.hash) && ${q('.rh-mark.is-a')} === 1`, 'a click selects run A and writes run= into the URL');
  await ctx.evaluate(
    `(() => { const ok = [...document.querySelectorAll('.rh-mark.ok:not(.mini)')].filter((b) => !b.classList.contains('is-a')); ok[0].dispatchEvent(new MouseEvent('click', { bubbles: true, shiftKey: true })); return true; })()`,
  );
  await need(`/[?&]vs=/.test(location.hash) && ${q('.rh-mark.is-b')} === 1`, 'a Shift+click selects run B and writes vs= into the URL');
  await need(`${q('.rh-table tbody tr.rh-num')} >= 1 && ${q('.rh-table thead th')} >= 5`, 'the compare table lists the numbers with a difference column');
  // Two runs of a system whose numbers all lack a direction (a Skill Lift-only pair, say) show differences without marks: only the
  // systems whose every good run carries a number with a direction are held to a mark.
  if (['aiperf', 'evaluator', 'gym', 'modelopt', 'dcgm', 'guardrails'].includes(system)) await need(`${q('.rh-table .badge')} >= 1`, 'the compare table marks numbers better / worse / same');
  await need(
    `!/(^|\\s)(null|undefined|NaN)(\\s|$)/.test([...document.querySelectorAll('.rh *')].filter((e) => !e.children.length).map((e) => e.textContent.trim()).join(' '))`,
    'no cell prints null, undefined or NaN',
  );
  // a filter narrows the runs and is kept in the URL
  const before = await ctx.evaluate(`document.querySelector('.rh-count').textContent`);
  await ctx.evaluate(`(() => { const s = document.querySelector('.rh-filters select'); s.value = s.options[1].value; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
  await need(`/[?&]f\\./.test(location.hash) && document.querySelector('.rh-count').textContent !== ${JSON.stringify(before)}`, 'a filter changes the run count and is kept in the URL');
  // the trend metric / group are selectable
  await ctx.evaluate(
    `(() => { const s = document.querySelector('#rh-metric'); if (s && s.options.length > 1) { s.value = s.options[1].value; s.dispatchEvent(new Event('change', { bubbles: true })); } return true; })()`,
  );
  await need(`document.querySelector('#rh-metric') !== null`, 'the metric picker survives a change');
  // deep link: away and back
  const deep = await ctx.evaluate('location.hash');
  await open(ctx, '#/overview', `${system} › leave`);
  await open(ctx, deep, `${system} › history deep link (mock)`);
  // (the filter may hide both selected runs from the timeline, so the compare table, which lists every run, is what is checked)
  await need(`/[?&]vs=/.test(location.hash) && ${q('.rh-table tbody tr')} >= 3`, 'a deep link reopens the selection and its compare table');
  // the Status tab of the same page still works after the History tab
  await open(ctx, `#/${system}?tab=status`, `${system} › status (mock)`);
}

/** Every check below fails with a message naming what was expected; nothing here throws. */
export async function visitVizPages(ctx) {
  await checkRoute(ctx);
  // six GPUs on the DCGM page, so the line picker has something to pick
  await ctx.evaluate(`fetch('/__dev/seven?dcgm_gpus=6', { headers: { 'X-Console': '1' } }).then(() => true)`);
  await visitCharts(ctx, 'dcgm', { picker: true });
  for (const system of ['aiperf', 'gym', 'modelopt', 'flare']) await visitCharts(ctx, system);
  for (const system of ['aiperf', 'evaluator', 'gym', 'modelopt', 'skillevaluator', 'datadesigner', 'dcgm', 'guardrails']) await visitHistory(ctx, system);
}

/**
 * The REAL backend's series route (addendum 9.13), checked from Node with the admin key: the documented shape for every window
 * (an empty `series` is fine on the hermetic home: a key with no data is a series with no points, never an error), and the
 * 422 invalid_request answers the contract lists. `fail(msg)` is called for each difference.
 */
export async function checkRealSeriesRoute({ base, key }, fail) {
  const get = async (qs) => {
    const r = await fetch(`${base}/api/v1/systems/dcgm/metrics/series?${qs}`, { headers: { Authorization: `Bearer ${key}` } });
    return [r.status, await r.json().catch(() => null)];
  };
  const steps = { '1h': 60, '24h': 60, '7d': 600 };
  for (const [w, step] of Object.entries(steps)) {
    const [st, a] = await get(`keys=dcgm.gpu_util&window=${w}&split=1`);
    if (st !== 200 || !a || a.window !== w || a.step_s !== step || !Array.isArray(a.series) || typeof a.from !== 'number' || typeof a.to !== 'number' || typeof a.truncated !== 'boolean')
      fail(`real backend: GET metrics/series window=${w} answered ${st} ${JSON.stringify(a)?.slice(0, 200)}`);
  }
  const [s15, a15] = await get('keys=dcgm.gpu_util&window=15m');
  if (s15 !== 200 || !a15 || a15.window !== '15m') fail(`real backend: window=15m answered ${s15}`);
  const bad = ['keys=', 'keys=Bad.Key', 'keys=dcgm.gpu_util&window=30d', 'keys=dcgm.gpu_util&split=2', `keys=${Array.from({ length: 13 }, (_, i) => `k.m${i}`).join(',')}`];
  for (const qs of bad) {
    const [st, a] = await get(qs);
    if (st !== 422 || a?.error?.code !== 'invalid_request') fail(`real backend: metrics/series?${qs.slice(0, 60)} answered ${st} ${a?.error?.code} (want 422 invalid_request)`);
  }
}
