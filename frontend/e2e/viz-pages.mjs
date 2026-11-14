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

// ------------------------------------------------------------------------------------------------------------------ wave 2 (addendum 9.14)

const NO_JUNK = (sel) =>
  `!/(^|\\s)(null|undefined|NaN)(\\s|$)/.test([...document.querySelectorAll(${JSON.stringify(sel)} + ' *')].filter((e) => !e.children.length).map((e) => e.textContent.trim()).join(' '))`;

const dev = (ctx, qs) => ctx.evaluate(`fetch(${JSON.stringify(`/__dev/${qs}`)}, { headers: { 'X-Console': '1' } }).then(() => true)`);
const clickText = (ctx, sel, text) =>
  ctx.evaluate(
    `(() => { const b = [...document.querySelectorAll(${JSON.stringify(sel)})].find((x) => x.textContent.trim() === ${JSON.stringify(text)}); if (!b) return false; b.click(); return true; })()`,
  );
const setSelect = (ctx, sel, value) =>
  ctx.evaluate(
    `(() => { const s = document.querySelector(${JSON.stringify(sel)}); if (!s) return false; s.value = ${JSON.stringify(value)}; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`,
  );

/** GPU telemetry, Status tab: the really-busy, memory, power, energy, violation, remapped-rows cards and the XID / diagnostics timeline. */
async function visitGpuHealth(ctx) {
  const need = async (expr, what, ms) => {
    if (!(await until(ctx, expr, ms))) ctx.fail(what);
  };
  await dev(ctx, 'seven?dcgm_gpus=3');
  await open(ctx, '#/dcgm?tab=status', 'dcgm › status › GPU health (mock)');
  await need(`${q('.gpu-health')} === 1 && ${q('.gpu-health .gpu-card .busy-bars')} >= 3`, 'the GPU health section has a really-busy card per GPU', 30000);
  await need(
    `[...document.querySelectorAll('.busy-bar')].every((b) => b.getAttribute('aria-label') && /\\d%|–/.test(b.getAttribute('aria-label')))`,
    'every utilisation bar has an aria-label with its figure',
  );
  await need(`${q('.gpu-card .badge')} >= 3`, 'each busy card says what it thinks (a verdict badge: colour and a word)');
  await need(
    `${q('.stack-card svg.srchart')} >= 1 && [...document.querySelectorAll('.stack-card .sr-seg')].every((r) => r.getAttribute('aria-label'))`,
    'the memory card draws stacked bars whose segments carry aria-labels',
    30000,
  );
  await need(`${q('.stack-card svg.chart[role=group]')} >= 4`, 'the memory card draws one stacked chart for now and one over time per GPU');
  await need(
    `[...document.querySelectorAll('.gpu-section')].some((s) => /Power against its limit/i.test(s.textContent) && s.querySelectorAll('svg.tschart').length >= 3)`,
    'power against its limit has a chart per GPU',
  );
  await need(`[...document.querySelectorAll('.gpu-section .stats .stat-value')].some((e) => /k?Wh/.test(e.textContent))`, 'the energy tiles show Wh or kWh');
  await need(
    `[...document.querySelectorAll('.gpu-section')].some((s) => /Throttle violations/i.test(s.textContent) && s.querySelectorAll('table').length >= 3)`,
    'throttle violations have a table of reasons per GPU',
  );
  await need(`[...document.querySelectorAll('.gpu-section')].some((s) => /Remapped rows/i.test(s.textContent) && s.querySelectorAll('tbody tr').length >= 2)`, 'remapped rows list the GPUs');
  await need(NO_JUNK('.gpu-health'), 'no GPU health cell prints null, undefined or NaN');
  // the timeline: marks with names, click = details, again = closed; a longer window shows more events
  await need(`${q('svg.tlchart')} === 1`, 'the timeline is drawn for the 1h window (the mock has a diagnostic in the last hour)', 30000);
  await need(`[...document.querySelectorAll('svg.tlchart .tl-mark')].every((m) => m.getAttribute('aria-label') && m.querySelector('title'))`, 'every timeline mark has an aria-label and a title');
  const n1h = await ctx.evaluate(q('svg.tlchart .tl-mark'));
  await setSelect(ctx, 'select[aria-label="Time window"]', '7d');
  await need(`location.hash.includes('window=7d') && ${q('svg.tlchart .tl-mark')} > ${n1h}`, 'a 7-day window shows more events on the timeline', 30000);
  await ctx.evaluate(`document.querySelector('svg.tlchart .tl-mark').dispatchEvent(new MouseEvent('click', { bubbles: true })); true`);
  await need(`${q('.tl-details')} === 1 && /Time \\(UTC\\)/.test(document.querySelector('.tl-details').textContent)`, 'clicking a mark opens its details');
  await ctx.evaluate(`document.querySelector('svg.tlchart .tl-mark.selected').dispatchEvent(new MouseEvent('click', { bubbles: true })); true`);
  await need(`${q('.tl-details')} === 0`, 'clicking the selected mark closes the details');
  // the generic charts the page keeps, and the window deep link
  await need(`${q('.series-card svg.tschart')} >= 3`, 'the generic series charts are still drawn under the health cards');
  const deep = await ctx.evaluate('location.hash');
  await open(ctx, '#/overview', 'dcgm › leave');
  await open(ctx, deep, 'dcgm › status deep link (mock)');
  await need(`document.querySelector('select[aria-label="Time window"]').value === '7d'`, 'a deep link restores the 7d window on the GPU health cards');
  await dev(ctx, 'seven?dcgm_gpus=2');
}

/** GPU telemetry, Fleet tab: the heatmap, its legend and readout, the metric / window pickers in the URL, and its down / empty states. */
async function visitFleet(ctx) {
  const need = async (expr, what, ms) => {
    if (!(await until(ctx, expr, ms))) ctx.fail(what);
  };
  await open(ctx, '#/dcgm?tab=fleet', 'dcgm › fleet (mock)');
  await need(`${q('svg.heatchart')} === 1 && ${q('svg.heatchart rect.heatc')} >= 20`, 'the fleet heatmap is drawn with coloured cells', 30000);
  await need(
    `${q('svg.heatchart .heat-row')} >= 6 && [...document.querySelectorAll('svg.heatchart .heat-row')].every((r) => r.getAttribute('aria-label'))`,
    'every heatmap row has an aria-label that sums it up',
  );
  await need(`document.querySelector('svg.heatchart').getAttribute('aria-label') && document.querySelector('svg.heatchart title')`, 'the heatmap has an aria-label and a title');
  await need(`${q('svg.heatchart .heat-none')} >= 1`, 'a gap in the data (the host that is down, an outage) is a hatched cell, never a low value');
  await need(`${q('.heat-legend')} === 1 && /no data/.test(document.querySelector('.heat-legend').textContent) && ${q('.heat-legend i.fh8')} === 1`, 'the legend shows the scale and the no-data key');
  await need(`[...document.querySelectorAll('.fleet-hosts .badge.bad')].some((b) => /host-d/.test(b.textContent))`, 'a host that is down is named in the host strip');
  await need(`document.querySelector('.fleet-card .seg-control [aria-pressed=true]').textContent.trim() === 'Utilisation'`, 'utilisation is the default metric');
  await need(`${q('.fleet-card details.chart-data')} === 1`, 'the heatmap has a collapsed data table as its text alternative');
  // hover: the readout names the row, the time and the value
  await ctx.evaluate(
    `(() => { const s = document.querySelector('svg.heatchart'); const b = s.getBoundingClientRect(); s.dispatchEvent(new PointerEvent('pointermove', { clientX: b.left + b.width * 0.6, clientY: b.top + 30, bubbles: true })); return true; })()`,
  );
  await need(
    `/UTC/.test(document.querySelector('.heat-readout').textContent) && /host-/.test(document.querySelector('.heat-readout').textContent)`,
    'moving over the heatmap reads out the row, the time and the value',
  );
  // the keyboard: focus a row, Left / Right move a cursor
  await ctx.evaluate(
    `(() => { const r = document.querySelector('svg.heatchart .heat-row'); r.dispatchEvent(new FocusEvent('focus')); r.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true })); return true; })()`,
  );
  await need(
    `/UTC/.test(document.querySelector('.heat-readout').textContent) && document.querySelector('svg.heatchart .heat-cursor').getAttribute('visibility') === 'visible'`,
    'Left / Right on a focused row moves a cursor and reads out the cell',
  );
  // pickers live in the URL
  await clickText(ctx, '.fleet-card .seg-control button', 'Temperature');
  await need(`location.hash.includes('metric=temp') && /°C/.test(document.querySelector('.heat-legend').textContent)`, 'choosing Temperature writes it into the URL and re-labels the scale', 30000);
  await clickText(ctx, '.fleet-card .seg-control button', '7d');
  await need(`location.hash.includes('window=7d') && /1\\.5 h|30 min/.test(document.querySelector('.fleet-card').textContent)`, 'choosing 7d writes the window into the URL and the heatmap follows');
  await need(NO_JUNK('.fleet-card'), 'no fleet cell prints null, undefined or NaN');
  const deep = await ctx.evaluate('location.hash');
  await open(ctx, '#/overview', 'dcgm › fleet › leave');
  await open(ctx, deep, 'dcgm › fleet deep link (mock)');
  await need(`document.querySelector('.fleet-card .seg-control [aria-pressed=true]').textContent.trim() === 'Temperature'`, 'a deep link restores the metric and the window');
  // states: Prometheus unreachable (a note, no error state), empty, a backend without the route
  const state = async (mode, expr, what) => {
    await dev(ctx, `wave2?fleet=${mode}`);
    await clickText(ctx, 'button', 'Refresh');
    await need(expr, what, 15000);
    if (await ctx.evaluate(q('#main .state.error'))) ctx.fail(`fleet (${mode}): the page shows an error state`);
  };
  await state(
    'prom_down',
    `${q('.fleet-card .note.warn')} === 1 && /Prometheus/.test(document.querySelector('.fleet-card .note.warn').textContent) && ${q('svg.heatchart')} === 0`,
    'Prometheus unreachable shows a warning note and no heatmap',
  );
  await state('empty', `${q('.fleet-card .state.empty')} === 1 && ${q('svg.heatchart')} === 0`, 'an answer with no rows shows the empty state');
  await state(
    'unsupported',
    `${q('.fleet-card .state.empty')} === 1 && /no fleet history/.test(document.querySelector('.fleet-card .state.empty').textContent)`,
    'a backend that answers 404 gets a plain explanation, not an error',
  );
  await dev(ctx, 'wave2?fleet=ok');
  await clickText(ctx, 'button', 'Refresh');
  await need(`${q('svg.heatchart')} === 1`, 'the heatmap comes back when the route does');
}

/** Switchyard: the decisions-and-models tab (flow, per-model rows, call-rate heatmap, fallbacks, outcomes, filters) and the Routing tab's filters. */
async function visitSwitchyard(ctx) {
  const need = async (expr, what, ms) => {
    if (!(await until(ctx, expr, ms))) ctx.fail(what);
  };
  await open(ctx, '#/switchyard?tab=flow', 'switchyard › decisions and models (mock)');
  await need(`${q('svg.flowchart')} === 1 && document.querySelector('svg.flowchart').getAttribute('aria-label')`, 'the decision flow is drawn and has an aria-label', 30000);
  await need(`${q('.sy-models tbody tr')} >= 3`, 'the models card has a row per model');
  await need(
    `${q('.sy-models .lbar')} >= 12 && [...document.querySelectorAll('.sy-models .lbar')].every((b) => b.getAttribute('aria-label'))`,
    'every latency / error / cache / context bar has an aria-label',
  );
  await need(`${q('svg.heatchart')} === 1 && ${q('svg.heatchart .heat-row')} >= 3`, 'the call-rate heatmap has a row per model');
  await need(
    `[...document.querySelectorAll('#main .card')].some((c) => /Fallbacks and routing overhead/.test(c.textContent)) && ${q('.lines-card svg.tschart')} >= 2`,
    'fallbacks and routing overhead are drawn',
  );
  await need(`[...document.querySelectorAll('#main .card')].some((c) => /What clients got/.test(c.textContent) && c.querySelector('rect.hbar'))`, 'the client outcomes card has share bars');
  await need(
    `[...document.querySelectorAll('#main .card')].some((c) => /^Routes/.test(c.querySelector('h2')?.textContent ?? '') && c.querySelector('rect.hbar'))`,
    'the routes are listed on their own (the decision counters carry no route)',
  );
  await need(NO_JUNK('#main'), 'no cell on the tab prints null, undefined or NaN');
  // filters
  const models = await ctx.evaluate(`[...document.querySelector('select[aria-label="Filter by model"]').options].map((o) => o.value).filter(Boolean)`);
  if (models.length < 2) ctx.fail(`the model filter offers ${models.length} models`);
  else {
    await setSelect(ctx, 'select[aria-label="Filter by model"]', models[0]);
    await need(`location.hash.includes('model=') && ${q('.sy-models tbody tr')} === 1`, 'the model filter narrows the models card to one row and is kept in the URL');
    await setSelect(ctx, 'select[aria-label="Filter by model"]', '');
  }
  await setSelect(ctx, 'select[aria-label="Time window"]', '24h');
  await need(`location.hash.includes('window=24h') && ${q('svg.flowchart')} === 1`, 'a 24-hour window keeps the flow and writes the window into the URL', 30000);
  const deep = await ctx.evaluate('location.hash');
  await open(ctx, '#/overview', 'switchyard › leave');
  await open(ctx, deep, 'switchyard › flow deep link (mock)');
  await need(`document.querySelector('select[aria-label="Time window"]').value === '24h'`, 'a deep link restores the window');
  // the Routing tab keeps working, with the route filter on its share cards
  await open(ctx, '#/switchyard?tab=routing', 'switchyard › routing (mock)');
  await need(`${q('.grid.three > .card')} === 3 && ${q('.grid.three rect.hbar')} >= 6`, 'the Routing tab still shows its three share cards', 30000);
  const routes = await ctx.evaluate(`[...document.querySelector('select[aria-label="Filter by route"]').options].map((o) => o.value).filter(Boolean)`);
  if (routes.length < 2) ctx.fail(`the route filter offers ${routes.length} routes`);
  else {
    await setSelect(ctx, 'select[aria-label="Filter by route"]', routes[0]);
    await need(
      `location.hash.includes('route=') && document.querySelectorAll('.grid.three > .card')[1].querySelectorAll('rect.hbar').length === 1`,
      'the route filter keeps one bar on the by-route card',
    );
  }
}

/** Dynamo: the Status tab's windowed latency, breakdown, load, tokens, cache, capacity and spec-decode cards, and the Events tab's filters. */
async function visitDynamo(ctx) {
  const need = async (expr, what, ms) => {
    if (!(await until(ctx, expr, ms))) ctx.fail(what);
  };
  await open(ctx, '#/dynamo?tab=status', 'dynamo › status (mock)');
  await need(`${q('.series-card svg.tschart')} >= 5`, 'the latency, tokens and prefix-cache cards draw series charts', 30000);
  await need(
    `[...document.querySelectorAll('.series-card h3')].some((h) => /for the window/.test(h.textContent)) && [...document.querySelectorAll('.series-card h3')].some((h) => /since start/.test(h.textContent))`,
    'windowed and since-start latency are labelled as such',
  );
  await need(
    `${q('.stack-card svg.srchart')} === 1 && [...document.querySelectorAll('.stack-card .sr-seg')].every((r) => r.getAttribute('aria-label')) && ${q('.stack-card svg.chart')} >= 2`,
    'the latency breakdown has stacked bars (aria-labelled) for p50 / p95 and a stack over time',
  );
  await need(`${q('.lines-card svg.tschart')} >= 3`, 'the load card draws its three panels');
  await need(`${q('table[aria-label="Capacity"] tbody tr')} >= 4`, 'the capacity panel lists the four settings');
  await need(
    `${q('.spec-rows .busy-row')} >= 3 && [...document.querySelectorAll('.spec-rows .lbar')].every((b) => b.getAttribute('aria-label'))`,
    'spec-decode acceptance is shown by position with aria-labelled bars',
  );
  await need(NO_JUNK('#main'), 'no Dynamo cell prints null, undefined or NaN');
  await setSelect(ctx, 'select[aria-label="Time window"]', '1h');
  await need(`location.hash.includes('window=1h')`, 'the window is kept in the URL');
  await clickText(ctx, '.seg-control button', 'p95');
  await need(`location.hash.includes('q=p95')`, 'the breakdown quantile is kept in the URL');
  // the Events tab
  await open(ctx, '#/dynamo?tab=events', 'dynamo › events (mock)');
  await need(`${q('.dyn-log tbody tr')} >= 50 && ${q('.series-chip[aria-pressed]')} === 5`, 'the events tab lists log lines with five level chips', 30000);
  const all = await ctx.evaluate(q('.dyn-log tbody tr'));
  await ctx.evaluate(`[...document.querySelectorAll('.series-chip')].find((b) => b.textContent.startsWith('INFO')).click(); true`);
  await need(`location.hash.includes('level=') && ${q('.dyn-log tbody tr')} < ${all}`, 'turning a level off narrows the lines and is kept in the URL');
  await ctx.evaluate(
    `document.querySelector('select[aria-label="Component"]').value = 'worker'; document.querySelector('select[aria-label="Component"]').dispatchEvent(new Event('change', { bubbles: true })); true`,
  );
  await need(
    `location.hash.includes('component=worker') && [...document.querySelectorAll('.dyn-log tbody tr td:nth-child(3)')].every((c) => c.textContent === 'worker')`,
    'the component filter keeps one component',
  );
  await ctx.evaluate(`[...document.querySelectorAll('button')].find((b) => b.textContent.trim() === 'Clear filters').click(); true`);
  await need(`!location.hash.includes('level=') && ${q('.dyn-log tbody tr')} >= 50`, 'Clear filters restores every line');
  // text only: a log line with markup is shown as characters, never as an element
  await ctx.evaluate(`(() => { const i = document.querySelector('input[type=search]'); i.value = '<img'; i.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
  await need(
    `${q('.dyn-log tbody tr')} >= 1 && /<img src=x/.test(document.querySelector('.dyn-log tbody').textContent) && ${q('.dyn-log img')} === 0 && ${q('#main img')} === 0`,
    'a log line that contains markup is shown as text, not as an element',
  );
  await ctx.evaluate(`[...document.querySelectorAll('button')].find((b) => b.textContent.trim() === 'Clear filters').click(); true`);
}

/** Wave 2 (addendum 9.14): GPU telemetry, Switchyard and Dynamo against the mock server. */
export async function visitWave2(ctx) {
  await visitGpuHealth(ctx);
  await visitFleet(ctx);
  await visitSwitchyard(ctx);
  await visitDynamo(ctx);
}

/** Every check below fails with a message naming what was expected; nothing here throws. */
export async function visitVizPages(ctx) {
  await checkRoute(ctx);
  // six GPUs on the DCGM page, so the line picker has something to pick
  await ctx.evaluate(`fetch('/__dev/seven?dcgm_gpus=6', { headers: { 'X-Console': '1' } }).then(() => true)`);
  await visitCharts(ctx, 'dcgm', { picker: true });
  for (const system of ['aiperf', 'gym', 'modelopt', 'flare']) await visitCharts(ctx, system);
  for (const system of ['aiperf', 'evaluator', 'gym', 'modelopt', 'skillevaluator', 'datadesigner', 'dcgm', 'guardrails']) await visitHistory(ctx, system);
  await visitWave2(ctx);
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

/**
 * The REAL backend's fleet route (addendum 9.14), checked from Node with the admin key: the documented shape for every window and metric (on the
 * hermetic home Prometheus is a closed port, so `rows` is empty and a `problem` says why: either is fine), and the 422 / 404 answers the contract lists.
 */
export async function checkRealFleetRoute({ base, key }, fail) {
  const get = async (path) => {
    const r = await fetch(`${base}/api/v1/systems/${path}`, { headers: { Authorization: `Bearer ${key}` } });
    return [r.status, await r.json().catch(() => null)];
  };
  const steps = { '1h': 60, '24h': 300, '7d': 1800 };
  for (const metric of ['util', 'temp', 'power', 'fb_used']) {
    for (const [w, step] of Object.entries(steps)) {
      const [st, a] = await get(`dcgm/fleet?metric=${metric}&window=${w}`);
      const ok =
        st === 200 &&
        a &&
        a.metric === metric &&
        a.window === w &&
        a.step_s === step &&
        Array.isArray(a.rows) &&
        typeof a.from === 'number' &&
        typeof a.to === 'number' &&
        typeof a.truncated === 'boolean' &&
        typeof a.unit === 'string';
      if (!ok) fail(`real backend: GET dcgm/fleet?metric=${metric}&window=${w} answered ${st} ${JSON.stringify(a)?.slice(0, 200)}`);
      else if (a.problem && !(typeof a.problem.code === 'string' && typeof a.problem.message === 'string'))
        fail(`real backend: the fleet problem field is not {code, message}: ${JSON.stringify(a.problem)}`);
    }
  }
  for (const [path, want] of [
    ['dcgm/fleet', 422],
    ['dcgm/fleet?metric=nope&window=1h', 422],
    ['dcgm/fleet?metric=util&window=15m', 422],
    ['switchyard/fleet?metric=util&window=1h', 404],
  ]) {
    const [st, a] = await get(path);
    if (st !== want || (want === 422 && a?.error?.code !== 'invalid_request')) fail(`real backend: GET ${path} answered ${st} ${a?.error?.code} (want ${want})`);
  }
}
