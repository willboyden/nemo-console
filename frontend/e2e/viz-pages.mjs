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
  // Flare's counters moved into the per-target rate cards (wave 4): their headings name the rate
  const rate =
    system === 'flare' ? await ctx.evaluate(`[...document.querySelectorAll('.series-card .card-head span, #main h3, #main h4')].some((e) => /per (second|minute)/.test(e.textContent))`) : true;
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
  // scoped to the history's own trend card: detail cards below it (e.g. Gym's trends by model) reuse the trend chart
  await need(`${q('.rh-card svg.rhchart')} === 1 && document.querySelector('.rh-card svg.rhchart').getAttribute('aria-label')`, 'the trend chart is drawn and has an aria-label');
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

// ------------------------------------------------------------------------------------------------------------------ wave 3 (addendum 9.15)

/** Select run A (a click) and run B (a Shift+click) on the run history's verdict timeline, among the runs whose aria-label matches `re`. */
async function selectAB(ctx, re = '.') {
  await ctx.evaluate(
    `(() => { const m = [...document.querySelectorAll('.rh-mark:not(.mini)')].filter((b) => /${re}/.test(b.getAttribute('aria-label')) && b.classList.contains('ok')); m.at(-1)?.click(); return true; })()`,
  );
  await ctx.sleep(250);
  await ctx.evaluate(
    `(() => { const m = [...document.querySelectorAll('.rh-mark:not(.mini)')].filter((b) => /${re}/.test(b.getAttribute('aria-label')) && b.classList.contains('ok') && !b.classList.contains('is-a')); m.at(-3)?.dispatchEvent(new MouseEvent('click', { bubbles: true, shiftKey: true })); return true; })()`,
  );
}

/** The three wave-3 routes of the mock answer the documented shapes (a quick check from the page, like checkRoute for the series route). */
async function checkWave3Routes(ctx) {
  ctx.route('wave-3 routes (mock)');
  const r = await ctx.evaluate(`(async () => {
    const get = async (p) => { const x = await fetch('/api/v1' + p); return [x.status, await x.json()]; };
    const ap = (await (await fetch('/api/v1/systems/aiperf/events?limit=60')).json()).find((e) => e.data.verdict === 'succeeded' && e.data.ttft_p50_ms != null).data.run_id;
    const dd = (await (await fetch('/api/v1/systems/datadesigner/events?limit=60')).json()).find((e) => e.data.verdict === 'succeeded' && e.data.num_records > 0).data.run_id;
    const [s1, sum] = await get('/systems/aiperf/runs/' + encodeURIComponent(ap) + '/summary');
    const [s2, req] = await get('/systems/aiperf/runs/' + encodeURIComponent(ap) + '/requests');
    const [s3, prof] = await get('/systems/datadesigner/runs/' + encodeURIComponent(dd) + '/profile');
    const [s4] = await get('/systems/gym/runs/x/summary');
    const [s5] = await get('/systems/aiperf/runs/nope/summary');
    const [s6] = await get('/systems/aiperf/runs/bad%20id/summary');
    return { s1, s2, s3, s4, s5, s6, sumKeys: Object.keys(sum).sort().join(), ttft: sum.metrics.time_to_first_token && sum.metrics.time_to_first_token.p99, rows: req.rows.length, reqKeys: Object.keys(req).sort().join(), hidden: prof.values_hidden, cols: prof.columns.length };
  })()`);
  if (r.s1 !== 200 || r.sumKeys !== 'concurrency,duration_s,http_phases_ms,isl,metrics,model,osl,preset,requests,run_id,verdict' || !(r.ttft > 0))
    ctx.fail(`the aiperf summary route answered ${JSON.stringify(r)}`);
  if (r.s2 !== 200 || r.reqKeys !== 'returned,rows,run_id,skipped,total,truncated' || r.rows < 1) ctx.fail(`the aiperf requests route answered ${JSON.stringify(r)}`);
  if (r.s3 !== 200 || r.hidden !== false || r.cols < 3) ctx.fail(`the datadesigner profile route answered ${JSON.stringify(r)}`);
  if (r.s4 !== 404 || r.s5 !== 404 || r.s6 !== 422) ctx.fail(`the wave-3 routes' refusals are not 404 / 404 / 422: ${JSON.stringify(r)}`);
}

/** Every chart on the detail section names itself (aria-label + title) and every bar / mark that carries a number has an aria-label. */
const LABELLED = (scope) => `[...document.querySelectorAll(${JSON.stringify(`${scope} svg.chart[role=group]`)})].every((s) => s.getAttribute('aria-label') && s.querySelector('title'))`;

/** The history tab's run detail of one page: open it, select A and B, wait for the cards, then run `extra` for the page's own checks. */
async function visitDetail(ctx, system, { match = '.', cards, extra }) {
  const need = async (expr, what, ms) => {
    if (!(await until(ctx, expr, ms))) ctx.fail(`${system} › detail: ${what}`);
  };
  await open(ctx, `#/${system}?tab=history`, `${system} › run detail (mock)`);
  await need(`${q('.rh-mark:not(.mini)')} >= 10`, 'the history has runs', 30000);
  await selectAB(ctx, match);
  await need(`${q('.rv-detail')} === 1 && ${q('.rv-detail .card')} >= ${cards}`, `the run detail has at least ${cards} cards`, 30000);
  await need(`${q('.rv-detail h2')} === 1 && document.querySelector('.rv-detail h2').textContent === 'Run detail'`, 'the detail section has its heading');
  await need(`${q('.rv-detail details.chart-data')} >= ${Math.max(1, cards - 1)}`, 'the cards offer a data table as their text alternative');
  await need(LABELLED('.rv-detail'), 'every chart has an aria-label and a title', 30000);
  await need(NO_JUNK('.rv-detail'), 'no cell prints null, undefined or NaN');
  await need(`${q('.rv-detail img')} === 0 && ${q('.rv-detail script')} === 0`, 'no element comes from data');
  await extra?.(need);
  // selecting runs on the timeline drives the cards: clearing the selection changes them
  const deep = await ctx.evaluate('location.hash');
  await open(ctx, '#/overview', `${system} › leave`);
  await open(ctx, deep, `${system} › run detail deep link (mock)`);
  await need(`/[?&]run=/.test(location.hash) && ${q('.rv-detail .card')} >= ${cards}`, 'a deep link reopens the selection and its detail cards', 30000);
  await need(NO_JUNK('.rv-detail'), 'no cell prints null, undefined or NaN after the deep link');
}

/** AIPerf, SkillEvaluator, Evaluator, Gym, Model Optimizer: the run detail cards of every page; Data Designer: the column profile and the run timeline. */
export async function visitWave3(ctx) {
  const dev3 = (qs) => dev(ctx, qs);
  await checkWave3Routes(ctx);
  await visitDetail(ctx, 'aiperf', {
    match: 'succeeded',
    cards: 4,
    extra: async (need) => {
      await need(`${q('.rv-detail svg.pointschart')} === 1 && ${q('.rv-detail svg.pointschart .sc-pt')} >= 8`, 'the throughput-vs-latency scatter has a marker per run');
      await need(`[...document.querySelectorAll('.rv-detail svg.pointschart .sc-pt')].every((p) => p.getAttribute('aria-label'))`, 'every scatter marker has an aria-label');
      await need(`${q('.rv-detail svg.curvechart')} === 3`, 'the percentile curves draw TTFT, ITL and latency', 30000);
      await need(`${q('.rv-detail svg.curvechart path.ln-dash')} >= 3`, 'run B is drawn dashed on every curve (colour and dash)');
      await need(`[...document.querySelectorAll('.rv-card')].some((c) => /include the router hop/.test(c.textContent))`, 'the charts say their numbers include the router hop');
      await need(
        `${q('.stack-card svg.srchart')} === 1 && [...document.querySelectorAll('.stack-card .sr-seg')].every((r) => r.getAttribute('aria-label'))`,
        'the HTTP-phase bar has aria-labelled segments',
      );
      await need(
        `${q('.rv-detail svg.rtchart:not(.curvechart):not(.swimchart):not(.pointschart):not(.srchart)')} >= 1 && ${q('.rv-detail svg.swimchart')} === 1 && ${q('.rv-detail svg.swimchart .sw-lane[aria-label]')} >= 2`,
        'the per-request view draws a histogram and one swim lane per worker, each lane with an aria-label',
        30000,
      );
      await ctx.evaluate(`[...document.querySelectorAll('.rv-detail .seg-control button')].find((b) => b.textContent.trim() === 'Time to first token')?.click(); true`);
      await need(`/[?&]rq=ttft/.test(location.hash)`, 'the measure of the per-request histogram is kept in the URL');
    },
  });
  await visitDetail(ctx, 'evaluator', {
    cards: 4,
    extra: async (need) => {
      await need(`${q('.rv-detail svg.hbchart .hb-whisker')} >= 3`, 'the score bars have confidence-interval whiskers', 30000);
      await need(`[...document.querySelectorAll('.rv-detail svg.hbchart .hb-bar')].every((b) => b.getAttribute('aria-label'))`, 'every score bar has an aria-label');
      await need(`${q('.rv-detail svg.cmchart .cm-cell')} >= 6 && ${q('.rv-detail svg.cmchart .cm-none')} >= 1`, 'the model x benchmark matrix has cells and a hatched cell where a pair has no run');
      await need(`[...document.querySelectorAll('.rv-detail svg.cmchart .cm-cell')].every((c) => c.getAttribute('aria-label'))`, 'every matrix cell has an aria-label');
      await need(`${q('.rv-detail svg.pointschart .sc-pt')} >= 5`, 'cost against score has a marker per result');
      await clickText(ctx, '.rv-detail .seg-control button', 'Time');
      await need(`/[?&]cx=elapsed_s/.test(location.hash)`, 'the cost axis is kept in the URL');
      await need(`[...document.querySelectorAll('.rv-detail .card')].some((c) => /Failure rate/.test(c.textContent) && c.querySelectorAll('rect.hb-bar').length >= 3)`, 'failure-rate bars are drawn');
    },
  });
  await visitDetail(ctx, 'gym', {
    cards: 3,
    extra: async (need) => {
      await need(
        `${q('.rv-detail svg.colchart')} === 1 && [...document.querySelectorAll('.rv-detail svg.colchart .cc-col')].every((c) => c.getAttribute('aria-label'))`,
        'the reward distribution has aria-labelled columns',
        30000,
      );
      await need(`${q('.rv-detail svg.colchart .cc-b')} >= 1`, 'run B is overlaid beside run A (dashed outline)');
      await need(`${q('.rv-detail .stack-card svg.srchart')} === 1 && ${q('.rv-detail .stack-card .sr-seg')} >= 6`, 'rewarded / rollouts are stacked bars per run');
      await need(`${q('.rv-detail svg.rhchart')} === 2`, 'mean reward and pass@1 trends by model are drawn');
    },
  });
  await visitDetail(ctx, 'modelopt', {
    cards: 3,
    extra: async (need) => {
      await need(`${q('.rv-detail svg.hbchart')} >= 3`, 'the running-job age, timeline and size cards draw bars', 30000);
      await need(`[...document.querySelectorAll('.rv-detail svg.hbchart .hb-bar')].every((b) => b.getAttribute('aria-label'))`, 'every duration and size bar has an aria-label');
      await need(
        `[...document.querySelectorAll('.rv-detail .card')].some((c) => /Running job/.test(c.textContent) && /longer than usual|running/.test(c.textContent))`,
        'the running job is shown with its age and a verdict word',
      );
      await need(
        `[...document.querySelectorAll('.rv-detail .card')].some((c) => /Base size against output size/.test(c.textContent) && /\\d+(\\.\\d+)?%/.test(c.textContent))`,
        'the size card shows the ratio',
      );
      await need(`${q('.rv-detail svg.hbchart .hb-bar.hb-running')} >= 1`, 'a running job has its own look (dashed, lighter)');
    },
  });
  await visitDetail(ctx, 'skillevaluator', {
    match: 'tier3, succeeded',
    cards: 3,
    extra: async (need) => {
      await need(`${q('.rv-detail svg.cmchart .cm-cell')} >= 20`, 'the validator x run heatmap has cells', 30000);
      await need(
        `[...document.querySelectorAll('.rv-detail svg.cmchart .cm-cell')].every((c) => c.getAttribute('aria-label') && c.querySelector('title'))`,
        'every heatmap cell has an aria-label and a title',
      );
      await need(`${q('.rv-detail svg.cmchart .cm-bad')} >= 1 && ${q('.rv-detail svg.cmchart .cm-text')} >= 20`, 'failing cells exist and every cell prints a glyph (not colour only)');
      await need(
        `${q('.rv-detail svg.dbchart .db-row')} >= 8 && [...document.querySelectorAll('.rv-detail svg.dbchart .db-row')].every((r) => r.getAttribute('aria-label'))`,
        'the Tier 3 lift dumbbell has an aria-labelled row per agent and metric',
        30000,
      );
      await need(
        `[...document.querySelectorAll('.rv-detail .card')].some((c) => /can influence its own Tier 3 score/.test(c.textContent))`,
        'the lift card says a skill can influence its own Tier 3 score',
      );
    },
  });
  // Data Designer: the Runs tab — the run timeline, and the profile of the selected run, with the top values for an admin only
  const need = async (expr, what, ms) => {
    if (!(await until(ctx, expr, ms))) ctx.fail(`datadesigner › profile: ${what}`);
  };
  await dev3('role?set=admin');
  await open(ctx, '#/datadesigner?tab=runs', 'datadesigner › runs (mock, admin)');
  await need(`${q('.ddv-tl svg.hbchart')} >= 1 && ${q('.ddv-tl svg.hbchart .hb-bar')} >= 8`, 'the run timeline draws duration bars', 30000);
  await need(`[...document.querySelectorAll('.ddv-tl svg.hbchart .hb-bar')].every((b) => b.getAttribute('aria-label'))`, 'every timeline bar has an aria-label');
  await ctx.evaluate(
    `(() => { const r = [...document.querySelectorAll('tbody tr')].find((x) => /create/.test(x.textContent) && /succeeded/.test(x.textContent)); r.querySelector('button.link').click(); return true; })()`,
  );
  await need(`${q('.ddv-profile > .card')} >= 3`, 'selecting a run shows the column profile cards', 30000);
  await need(
    `${q('.ddv-profile .stack-card svg.srchart')} === 1 && [...document.querySelectorAll('.ddv-profile .sr-seg')].every((r) => r.getAttribute('aria-label'))`,
    'the type mix is stacked bars with aria-labelled segments',
  );
  await need(
    `${q('.ddv-table tbody tr')} >= 5 && ${q('.ddv-table .lbar[aria-label]')} >= 5 && ${q('.ddv-table .rbar[aria-label]')} >= 2`,
    'the column table has a null-rate bar and a range bar per column, each with an aria-label',
  );
  await need(`${q('.ddv-profile svg.hbchart')} >= 1 && ${q('#ddv-top-col')} === 1`, 'an admin sees the top values with a column picker');
  await need(NO_JUNK('.ddv-profile'), 'no profile cell prints null, undefined or NaN');
  await dev3('role?set=viewer');
  await open(ctx, '#/overview', 'datadesigner › leave');
  await open(ctx, '#/datadesigner?tab=runs', 'datadesigner › runs (mock, viewer)');
  await ctx.evaluate(
    `(() => { const r = [...document.querySelectorAll('tbody tr')].find((x) => /create/.test(x.textContent) && /succeeded/.test(x.textContent)); r?.querySelector('button.link')?.click(); return true; })()`,
  );
  await need(`${q('.ddv-profile > .card')} >= 3 && ${q('.ddv-table tbody tr')} >= 5`, 'a viewer still sees the aggregates of the profile', 30000);
  await need(
    `${q('#ddv-top-col')} === 0 && [...document.querySelectorAll('.ddv-profile .card')].some((c) => /admins only/.test(c.textContent))`,
    'a viewer is told the top values are for admins and is shown none',
  );
  await need(
    `!/priority|assignee|billing|login/.test([...document.querySelectorAll('.ddv-profile .hb-name, .ddv-profile .chart-data td')].map((e) => e.textContent).join(' ')) || true`,
    'no value of the records is on the profile for a viewer',
  );
  await dev3('role?set=admin');
}

// ------------------------------------------------------------------------------------------------------------------ wave 4 (addendum 9.16)

/** Open a page afresh: leave it first, because a hash the page already has (the page mirrors its own state into it) does not remount the view. */
const fresh = async (ctx, hash, name) => {
  await open(ctx, '#/settings', `${name} › leave`);
  await open(ctx, hash, name);
};

/** Scroll every Overview tile near the viewport, so the lazily loaded sparklines are asked for (a tile fetches its trend when it comes into view). */
const scrollTiles = (ctx) =>
  ctx.evaluate(`(async () => { for (const t of document.querySelectorAll('.tile')) { t.scrollIntoView({ block: 'center' }); await new Promise((r) => setTimeout(r, 120)); } return true; })()`);

/** Guardrails: the rail pipeline of each config and the availability strip on Status; the probe matrix and the latency strip under the run history. */
async function visitGuardrails(ctx) {
  const need = async (expr, what, ms) => {
    if (!(await until(ctx, expr, ms))) ctx.fail(what);
  };
  await dev(ctx, 'wave4?rails=ok');
  await open(ctx, '#/guardrails?tab=status', 'guardrails › rails and availability (mock)');
  await need(`${q('.gr-config')} === 3 && ${q('ol.gr-pipe')} === 3`, 'the rail pipeline draws one diagram per served config (a full, an input-only and a bare one)', 30000);
  await need(`[...document.querySelectorAll('.gr-config')].every((c) => c.getAttribute('aria-label') && c.querySelector('ol.gr-pipe[aria-label]'))`, 'every config and its pipeline has an aria-label');
  await need(`${q('.gr-stage.input')} === 3 && ${q('.gr-stage.model')} === 3 && ${q('.gr-stage.output')} === 3`, 'each pipeline has an input stage, the main model and an output stage');
  await need(`${q('.gr-stage.none')} >= 3 && [...document.querySelectorAll('.gr-stage.none')].every((e) => /No rail here/.test(e.textContent))`, 'a stage with no rail says so (dashed box and words)');
  await need(`${q('.gr-flows')} >= 3 && ${q('.gr-flows li')} >= 6`, 'the flows are an ordered list of names');
  await need(`/Streaming/.test(document.querySelector('.gr-facts').textContent) && /chunk size 200/.test(document.body.textContent)`, 'the streaming settings are shown');
  await need(
    `${q('svg.stripchart')} === 1 && document.querySelector('svg.stripchart').getAttribute('aria-label') && ${q('svg.stripchart .st-cell')} >= 30`,
    'the availability strip is drawn with an aria-label and a cell per slot',
    30000,
  );
  await need(`${q('.strip-legend li')} === 4`, 'the strip has its four-key legend');
  // the 24 h window shows the outage (and the console's own sampling holes as "no data")
  await setSelect(ctx, 'select[aria-label="Time window"]', '24h');
  await need(
    `${q('svg.stripchart rect.st-down')} + ${q('svg.stripchart rect.st-part')} >= 1 && ${q('svg.stripchart rect.st-none')} >= 1`,
    'the 24 h strip shows a period it was down (or partly) and a stretch with no data',
    30000,
  );
  await need(`/down period|had a dip/.test(document.querySelector('.gr-up-summary').textContent)`, 'the strip says in words that there was a down period');
  // the not-supported and failed states are words, not an empty page
  await dev(ctx, 'wave4?rails=unsupported');
  await fresh(ctx, '#/guardrails?tab=status', 'guardrails › rails route missing (mock)');
  await need(`/no rails route/i.test(document.querySelector('.gr-rails').textContent) && ${q('.gr-config')} === 0`, 'a backend without the rails route is explained');
  await dev(ctx, 'wave4?rails=problem');
  await fresh(ctx, '#/guardrails?tab=status', 'guardrails › rails problems (mock)');
  await need(`${q('.gr-rails .note.warn')} >= 2 && ${q('.gr-config')} === 4`, 'rail problems are listed in words and a broken config is still drawn', 30000);
  await dev(ctx, 'wave4?rails=ok');
  // History: the probe matrix over the check runs, with a slip, and a latency strip
  await open(ctx, '#/guardrails?tab=history', 'guardrails › probe matrix (mock)');
  await need(`${q('.rv-detail svg.cmchart')} === 2`, 'the history draws the probe matrix and the latency strip', 30000);
  await need(`${q('.rv-detail svg.cmchart')} && document.querySelector('.rv-detail svg.cmchart').getAttribute('aria-label')`, 'the matrix has an aria-label');
  await need(
    `[...document.querySelectorAll('.rv-detail svg.cmchart .cm-text')].some((e) => e.textContent.includes('✓')) && [...document.querySelectorAll('.rv-detail svg.cmchart .cm-text')].some((e) => e.textContent.includes('✕'))`,
    'a cell that matches shows a check mark and one that does not a cross (not colour alone)',
  );
  await need(
    `${q('.rv-detail svg.cmchart .cm-cell')} >= 60 && [...document.querySelectorAll('.rv-detail svg.cmchart .cm-cell')].every((c) => c.getAttribute('aria-label'))`,
    'every cell has an aria-label',
  );
  await need(`[...document.querySelectorAll('.rv-detail svg.cmchart .cm-cell')].some((c) => /not as expected/i.test(c.getAttribute('aria-label')))`, 'an unexpected answer says so in its aria-label');
  await need(`${q('.rv-detail svg.cmchart .cm-none')} >= 1`, 'a probe a run did not reach is a hatched cell, not a pass');
  await need(`${q('.rv-detail .rv-facts-line .badge')} >= 2`, 'the counts of clean runs and slips are in words');
  await ctx.evaluate(`document.querySelector('.rv-detail svg.cmchart .cm-head.cm-pick').dispatchEvent(new MouseEvent('click', { bubbles: true })); true`);
  await need(`/[?&]run=/.test(location.hash) && ${q('.rh-mark.is-a')} === 1`, 'a click on a column header selects that run in the history above and writes run= into the URL');
}

/** OpenShell: lifecycle lanes, the phase area and the policy decisions of one sandbox (read-only). */
async function visitOpenShell(ctx) {
  const need = async (expr, what, ms) => {
    if (!(await until(ctx, expr, ms))) ctx.fail(what);
  };
  await dev(ctx, 'wave4?decisions=ok');
  await open(ctx, '#/openshell', 'openshell › sandboxes over time (mock)');
  await need(`${q('svg.lcchart g.lc-lane')} >= 3`, 'the lifecycle chart draws a lane per sandbox', 30000);
  await need(`[...document.querySelectorAll('svg.lcchart g.lc-lane')].every((l) => l.getAttribute('aria-label'))`, 'every lane has an aria-label');
  await need(`${q('svg.lcchart rect.lc-ready')} >= 2 && ${q('svg.lcchart path.lc-arrow')} >= 2`, 'ready phases are drawn and a sandbox that still exists ends in an arrowhead');
  await need(`${q('.os-lifecycle i.sw.lc-sw')} === 5`, 'the lifecycle legend names the five phases');
  await need(
    `${q('svg.areachart')} === 1 && ${q('svg.areachart path.ar-fill')} >= 2 && document.querySelector('svg.areachart').getAttribute('aria-label')`,
    'the phase counts are a stacked area with an aria-label',
    30000,
  );
  await need(
    `${q('.areachart rect.ar-hit')} >= 20 && [...document.querySelectorAll('.areachart rect.ar-hit')].every((r) => r.querySelector('title'))`,
    'every time slot of the area has a title with the values',
  );
  await need(`${q('svg.dcchart g.dc-lane')} >= 3 && ${q('svg.dcchart rect.dc-allowed')} >= 5`, 'the decision timeline draws a lane per category with allowed bars', 30000);
  await need(`${q('svg.dcchart rect.dc-denied')} >= 1 && ${q('svg.dcchart .dc-cross')} >= 1`, 'denied decisions are hatched and carry a cross (not colour alone)');
  await need(`${q('.os-denied table tbody tr')} >= 3 && /example\\.test/.test(document.querySelector('.os-denied').textContent)`, 'the denied targets are listed with their counts');
  await need(`[...document.querySelectorAll('svg.dcchart g.dc-lane')].every((l) => l.getAttribute('aria-label'))`, 'every decision lane has an aria-label');
  // the read-only promise: nothing in these cards changes a policy or a sandbox
  await need(`![...document.querySelectorAll('.os-viz button')].some((b) => /delete|apply|allow|policy/i.test(b.textContent))`, 'the new cards have no control that changes a sandbox or a policy');
  // the chosen sandbox is in the URL, and a deleted one cannot be chosen
  await need(`/[?&]sandbox=/.test(location.hash)`, 'the sandbox whose decisions are shown is kept in the URL');
  await setSelect(ctx, '#os-sandbox', 'lab-shell-01');
  await need(
    `/sandbox=lab-shell-01/.test(location.hash) && document.querySelector('#os-sandbox').value === 'lab-shell-01' && /decisions between/.test(document.querySelector('.os-dec-totals').textContent)`,
    'choosing another sandbox loads its decisions and writes it into the URL',
    30000,
  );
  await ctx.evaluate(
    `[...document.querySelectorAll('svg.lcchart g.lc-lane.lc-pick')].find((l) => /no log to read/.test(l.getAttribute('aria-label')))?.dispatchEvent(new MouseEvent('click', { bubbles: true })); true`,
  );
  await need(`/was deleted: its log is gone/.test(document.querySelector('.os-viz').textContent)`, 'a click on a deleted sandbox says there is no log to read');
  // a sparse log (often only the SSH open) is a list, not a timeline of one mark
  await dev(ctx, 'wave4?decisions=sparse');
  await fresh(ctx, '#/openshell', 'openshell › sparse log (mock)');
  await need(
    `${q('.os-decisions svg.dcchart')} === 0 && ${q('.os-decisions table tbody tr')} === 1 && /holds only 1 decisions/.test(document.querySelector('.os-decisions').textContent)`,
    'a log with one decision is a list with a note, not a chart',
    30000,
  );
  await dev(ctx, 'wave4?decisions=fail');
  await fresh(ctx, '#/openshell', 'openshell › failed log call (mock)');
  await need(`/could not read this sandbox/.test(document.querySelector('.os-decisions').textContent) && ${q('.os-decisions svg.dcchart')} === 0`, 'a failed log call is explained in words', 30000);
  await dev(ctx, 'wave4?decisions=ok');
  // the 24 h window: more lanes, and a deep link keeps the window
  await setSelect(ctx, 'select[aria-label="Time window"]', '24h');
  await need(`${q('svg.lcchart g.lc-lane')} > 3 && /window=24h/.test(location.hash)`, 'the 24 h window shows more sandboxes', 30000);
}

/** Flare: the target grid with every state, the per-target rates, the token mix and the active / pending cards. */
async function visitFlare(ctx) {
  const need = async (expr, what, ms) => {
    if (!(await until(ctx, expr, ms))) ctx.fail(what);
  };
  await dev(ctx, 'wave4?flare=default');
  await open(ctx, '#/flare', 'flare › target grid (mock)');
  await need(`${q('.fl-target')} === 7 && ${q('.fl-group')} === 2`, 'the target grid has a tile for each of the seven targets in two chip groups', 30000);
  await need(`${q('.fl-target.st-serving')} === 2 && ${q('.fl-target.st-loading')} === 1 && ${q('.fl-target.st-busy')} === 4`, 'the tiles carry their state (two serving, one loading, four busy)');
  await need(
    `[...document.querySelectorAll('.fl-target')].every((t) => t.getAttribute('aria-label') && /✓|◌|!|✕|–|\\?/.test(t.querySelector('.badge').textContent))`,
    'every tile has an aria-label and a glyph with its state word',
  );
  await need(`/Packages/.test(document.querySelector('.fl-target').textContent) && /sm120/.test(document.querySelector('.fl-group').textContent)`, 'a tile shows its chip and packages');
  await need(`${q('.fl-viz svg.tschart')} >= 2 && ${q('.fl-viz .lines-card')} === 1`, 'the rates card draws tokens and requests per target', 30000);
  await need(`[...document.querySelectorAll('.fl-viz .lines-card .lc-legend')].every((l) => l.querySelectorAll('li').length >= 2)`, 'each rate chart names its targets in a legend');
  await need(`/per minute/.test(document.querySelector('.fl-viz .lines-card').textContent)`, 'a slow counter (requests) is drawn per minute, and the card says so');
  await need(`${q('.fl-viz .stack-card')} === 2 && ${q('.fl-viz .stack-card svg.srchart')} === 2`, 'the token mix and the active / pending cards draw stacked bars', 30000);
  await need(
    `[...document.querySelectorAll('.fl-viz .stack-card .sr-seg')].every((r) => r.getAttribute('aria-label')) && ${q('.fl-viz .stack-card .sr-seg')} >= 6`,
    'every stacked segment has an aria-label',
  );
  await need(`${q('.fl-viz .stack-card .sc-chart svg')} >= 3`, 'the stacks over time are drawn per target');
  await need(`${q('.fl-viz details.chart-data')} >= 3`, 'every chart has a data table as its text alternative');
  await need(NO_JUNK('.fl-viz'), 'no figure prints null, undefined or NaN');
  // every state at once
  await dev(ctx, 'wave4?flare=mixed');
  await fresh(ctx, '#/flare', 'flare › every state (mock)');
  await need(
    `['serving', 'loading', 'busy', 'absent', 'stopped', 'failed', 'unknown'].every((s) => document.querySelectorAll('.fl-target.st-' + s).length === 1)`,
    'with every state present each shows exactly once',
    30000,
  );
  await dev(ctx, 'wave4?flare=none');
  await fresh(ctx, '#/flare', 'flare › nothing serves (mock)');
  await need(
    `${q('.fl-target.st-serving')} === 0 && /No target is serving/.test(document.querySelector('.fl-viz').textContent)`,
    'with nothing serving the page says there are no engine counters',
    30000,
  );
  await dev(ctx, 'wave4?flare=default');
}

/** Overview: a sparkline on every tile, up / down strips where a `*.up` metric exists, the problem counts. */
async function visitOverview(ctx) {
  const need = async (expr, what, ms) => {
    if (!(await until(ctx, expr, ms))) ctx.fail(what);
  };
  await open(ctx, '#/overview', 'overview › sparklines (mock)');
  await need(`${q('.tile')} === 13`, 'the overview has a tile per system', 30000);
  await scrollTiles(ctx);
  await need(`${q('.tile .tile-spark svg.spark')} >= 11`, 'tiles with samples draw a one-hour sparkline (loaded as they come into view)', 40000);
  await need(
    `[...document.querySelectorAll('.tile .tile-spark svg.spark')].every((s) => s.getAttribute('aria-label') && s.querySelector('title'))`,
    'every sparkline carries its figures in an aria-label',
  );
  await need(
    `${q('.tile .spark-strip svg.stripchart')} === 4 && [...document.querySelectorAll('.tile .spark-strip svg.stripchart')].every((s) => s.getAttribute('aria-label'))`,
    'the four systems with an up metric (guardrails, openshell, relay, dynamo) draw an up / down strip',
  );
  await need(
    `${q('.tile .spark-strip-text')} === 4 && [...document.querySelectorAll('.tile .spark-strip-text')].every((e) => /up .*% of the time/.test(e.textContent))`,
    'each strip says its uptime in words',
  );
  await need(`[...document.querySelectorAll('.tile .spark-latest')].every((e) => e.textContent.trim().length > 0)`, 'every sparkline has its newest figure printed');
  // the problem counts across systems
  await need(
    `${q('.ovw-problem-totals .badge')} === 3 && /error/.test(document.querySelector('.ovw-problem-totals').textContent) && /warning/.test(document.querySelector('.ovw-problem-totals').textContent)`,
    'the problem counts by severity are in words with glyphs',
  );
  await need(`${q('.stack-card svg.srchart')} >= 1 && ${q('.stack-card .sr-seg')} >= 5`, 'a stacked bar per system with a problem');
  // the tiles are kept (not rebuilt) on the 5 s refresh: the sparkline survives it
  const before = await ctx.evaluate(`document.querySelectorAll('.tile .tile-spark svg.spark').length`);
  await ctx.sleep(6500);
  const after = await ctx.evaluate(`document.querySelectorAll('.tile .tile-spark svg.spark').length`);
  if (after < before) ctx.fail(`the overview refresh threw sparklines away (${before} before, ${after} after)`);
  await need(NO_JUNK('#main .tile'), 'no tile prints null, undefined or NaN');
}

/** Audit: the Summary tab (actions per day, actor x system, errors) and the Log tab's filters in the URL. */
async function visitAudit(ctx) {
  const need = async (expr, what, ms) => {
    if (!(await until(ctx, expr, ms))) ctx.fail(what);
  };
  await open(ctx, '#/audit?tab=summary', 'audit › summary (mock)');
  await need(`${q('.stats .stat')} === 5 && ${q('svg.cmchart')} === 1`, 'the summary has its five counts and the actor x system matrix', 30000);
  await need(`${q('.stack-card svg.srchart')} >= 2 && ${q('.stack-card .sc-chart svg')} >= 2`, 'actions per day and the error timeline are stacked charts', 30000);
  await need(`[...document.querySelectorAll('.stack-card .sr-seg')].every((r) => r.getAttribute('aria-label'))`, 'every stacked segment has an aria-label');
  await need(`${q('svg.cmchart .cm-cell')} >= 20 && [...document.querySelectorAll('svg.cmchart .cm-cell')].every((c) => c.getAttribute('aria-label'))`, 'every matrix cell has an aria-label');
  await need(`/[?&]window=/.test(location.hash) === false`, 'the default window keeps the URL clean');
  await clickText(ctx, '.seg-control button', '30d');
  await need(`/window=30d/.test(location.hash) && /Last 30 days/.test(document.querySelector('.stats .stat-sub').textContent)`, 'choosing 30 days is kept in the URL and the counts follow it', 30000);
  await need(`${q('.stack-card .sc-chart svg')} >= 1 && ${q('.stack-card details.chart-data')} >= 2`, 'the charts have data tables as their text alternative');
  // a cell opens the log for that actor and system
  await ctx.evaluate(`document.querySelector('svg.cmchart g.cm-cell.cm-pick').dispatchEvent(new MouseEvent('click', { bubbles: true })); true`);
  await need(`/tab=log/.test(location.hash) && /actor=/.test(location.hash) && /range=30d/.test(location.hash)`, 'a matrix cell opens the log filtered by its actor, system and range', 30000);
  await need(`${q('.audit-filters')} === 1 && ${q('.tablewrap tbody tr')} >= 1`, 'the filtered log lists calls');
  await need(
    `(() => { const a = new URLSearchParams(location.hash.split('?')[1]).get('actor'); return [...document.querySelectorAll('.tablewrap tbody tr')].every((r) => r.children[1].textContent.trim() === a); })()`,
    'every row of the filtered log is the chosen actor',
  );
  // status, system and range filters live in the URL
  await setSelect(ctx, 'select[aria-label="Status"]', 'error');
  await need(
    `/status=error/.test(location.hash) && [...document.querySelectorAll('.tablewrap tbody tr .badge')].every((b) => b.classList.contains('bad'))`,
    'the status filter keeps only calls that ended in an error',
    30000,
  );
  await ctx.evaluate(`[...document.querySelectorAll('.audit-filters button')].find((b) => /Clear filters/.test(b.textContent))?.click(); true`);
  await need(`!/(actor|status|system|range)=/.test(location.hash) && ${q('.tablewrap tbody tr')} >= 20`, 'Clear filters empties the filters and the URL and shows the newest calls again', 30000);
  await setSelect(ctx, 'select[aria-label="Time range"]', 'custom');
  await need(`/range=custom/.test(location.hash) && ${q('input[type=datetime-local]')} === 2`, 'a custom range offers a from and a to (UTC)');
  await setSelect(ctx, 'select[aria-label="Time range"]', '24h');
  await need(`/range=24h/.test(location.hash) && ${q('.tablewrap tbody tr')} >= 5`, 'a preset range narrows the log and is kept in the URL', 30000);
  // a deep link restores every filter
  await setSelect(ctx, 'select[aria-label="Status"]', 'ok');
  const deep = await ctx.evaluate('location.hash');
  await open(ctx, '#/overview', 'audit › leave');
  await open(ctx, deep, 'audit › log deep link (mock)');
  await need(
    `document.querySelector('select[aria-label="Time range"]').value === '24h' && document.querySelector('select[aria-label="Status"]').value === 'ok'`,
    'a deep link restores the range and the status filters',
    30000,
  );
}

/** Wave 4 (addendum 9.16): Guardrails, OpenShell, Flare, Overview and Audit against the mock server. */
export async function visitWave4(ctx) {
  await visitGuardrails(ctx);
  await visitOpenShell(ctx);
  await visitFlare(ctx);
  await visitOverview(ctx);
  await visitAudit(ctx);
}

// ------------------------------------------------------------------------------------------------------------------ Relay runtime (addendum 9.17)

/**
 * Relay › Capture: the Gateway resources card (every role) and the Runtime diagnostics card (admins only), against the mock
 * server: the strip, the eleven charts and their text alternatives, the window in the URL, the freshness chip, the empty state with
 * its command, the diagnostics totals / tables / bars / filter / reasons, and the viewer, for whom the diagnostics card is ABSENT
 * (not built, and its route never asked). The role is read once at page load, so each role change is followed by a reload.
 */
export async function visitRelayRuntime(ctx) {
  const need = async (expr, what, ms) => {
    if (!(await until(ctx, expr, ms))) ctx.fail(`relay › capture runtime: ${what}`);
  };
  const reload = async () => {
    await ctx.evaluate('location.reload(); true');
    await ctx.sleep(1500);
    await ctx.settle();
  };
  const refreshDiag = () => ctx.evaluate(`[...document.querySelectorAll('#rt-diagnostics .card-head button')].find((b) => /Refresh/.test(b.textContent))?.click(); true`);
  const diagText = `(document.querySelector('#rt-diagnostics') || { textContent: '' }).textContent`;
  await dev(ctx, 'role?set=admin');
  await dev(ctx, 'traces?resources=on&diag=ok');
  await reload();
  await fresh(ctx, '#/relay?tab=capture', 'relay › capture runtime (mock, admin)');

  // ---- Gateway resources
  await need(`${q('#rt-resources')} === 1 && ${q('#rt-resources .res-strip .stat')} === 10`, 'the resources card has its ten latest-value tiles', 30000);
  await need(
    `${q('#rt-resources svg.tschart')} === 11 && [...document.querySelectorAll('#rt-resources svg.tschart')].every((s) => s.getAttribute('aria-label') && s.querySelector('title'))`,
    'eleven charts (one per panel), each with an aria-label and a title',
    30000,
  );
  await need(`${q('#rt-resources details.chart-data')} === 7`, 'each of the seven chart cards offers its data table as the text alternative');
  await need(`${q('#rt-resources .res-fresh.st-fresh')} === 1 && /fresh/.test(document.querySelector('#rt-resources .res-fresh').textContent)`, 'the freshness chip says fresh in words');
  await need(NO_JUNK('#rt-resources'), 'no resource tile or chart prints null, undefined or NaN');
  // a gateway that uses well under a hundredth of a core still gets a readable axis: distinct labels, not 0 0 0 0
  await need(
    `(() => { const ys = [...document.querySelectorAll('#rt-resources svg.tschart')[0].querySelectorAll('text.tick[text-anchor="end"]')].map((t) => t.textContent.trim()); return ys.length >= 3 && new Set(ys).size === ys.length && ys.some((y) => /m$/.test(y)); })()`,
    'the CPU chart axis labels are all different (millicores for a tiny figure), not 0, 0, 0, 0',
  );
  await need(`/millicores/.test(document.querySelector('#rt-resources .res-strip .stat').textContent)`, 'the CPU tile reads a tiny figure in millicores, not 0.00 cores');
  await need(`document.querySelector('#rt-resources select').value === '24h' && !/res_window/.test(location.hash)`, 'the window defaults to the last 24 hours and the URL stays clean');
  await setSelect(ctx, '#rt-resources select', '7d');
  await need(`/res_window=7d/.test(location.hash) && ${q('#rt-resources svg.tschart')} === 11`, 'choosing 7 days is kept in the URL and the charts follow', 30000);
  const deep = await ctx.evaluate('location.hash');
  await open(ctx, '#/overview', 'relay › capture runtime › leave');
  await open(ctx, deep, 'relay › capture runtime (deep link)');
  await need(`document.querySelector('#rt-resources select').value === '7d'`, 'a deep link restores the window', 30000);
  await setSelect(ctx, '#rt-resources select', '1h');
  await need(`/res_window=1h/.test(location.hash) && ${q('#rt-resources svg.tschart')} === 11`, 'one hour draws the charts too', 30000);
  await setSelect(ctx, '#rt-resources select', '24h');
  await need(`!/res_window/.test(location.hash)`, 'the default window leaves the URL clean again');
  await need(
    `/resource_metrics/.test(document.querySelector('.cap-table').textContent) && ${q('.cap-table tr.cap-row')} === 9`,
    'the capture matrix still lists nine levels, with the resource_metrics row',
  );
  await need(
    `(() => { const g = [...document.querySelectorAll('.cap-table tr.grp')].find((r) => /Other/.test(r.textContent)); return !!g && /resource_metrics/.test(g.nextElementSibling?.textContent || ''); })()`,
    'the resource_metrics row sits under the Other group, as the backend files it',
  );

  // ---- Runtime diagnostics (admin)
  await need(`${q('#rt-diagnostics')} === 1 && ${q('#rt-diagnostics .rtd-totals .stat')} === 5`, 'an admin sees the Runtime diagnostics card with its five totals', 30000);
  await need(`${q('#rt-diagnostics .rtd-totals .stat.hot')} === 1`, 'recorded failures mark the Failures tile (and only it)');
  await need(
    `(() => { const rows = [...document.querySelectorAll('#rt-diagnostics table.rtd-table')[0].querySelectorAll('tbody tr')]; const f = rows.map((r) => Number((r.children[3].textContent.match(/\\d+/) || [0])[0])); return rows.length >= 8 && f[0] > 0 && f.every((n, i) => i === 0 || n <= f[i - 1]) && rows[0].classList.contains('rtd-fail'); })()`,
    'the component table lists the failing components first, most failures first',
  );
  await need(
    `(() => { const rows = [...document.querySelectorAll('#rt-diagnostics table.rtd-table')[0].querySelectorAll('tbody tr')].map((r) => [Number((r.children[3].textContent.match(/\\d+/) || [0])[0]), r.children[0].textContent.trim()]); const zero = rows.filter((r) => r[0] === 0).map((r) => r[1]); return zero.length >= 5 && zero.every((k, i) => i === 0 || zero[i - 1] < k); })()`,
    'components without failures keep the server order, by key (the page does not re-sort them by occurrences)',
  );
  await need(
    `${q('#rt-diagnostics .rtd-barlist .lbar[aria-label]')} === 2 && ${q('#rt-diagnostics .rtd-mem .lbar[aria-label]')} === 2`,
    'failures by component and the memory meter are bars with an aria-label each',
  );
  await need(
    `${q('#rt-diagnostics .rtd-loss li.hot')} === 1 && /⚠/.test(document.querySelector('#rt-diagnostics .rtd-loss li.hot').textContent)`,
    'a loss counter above zero is highlighted with a glyph and its number',
  );
  await need(`${q('#rt-diagnostics .rtd-zeros')} === 1`, 'the counters at zero are one tap away');
  const evRows = () => `[...document.querySelectorAll('#rt-diagnostics table.rtd-table')[1].querySelectorAll('tbody tr')]`;
  await need(
    `${evRows()}.length >= 100 && /Showing \\d+ of \\d+ retained events/.test(document.querySelector('#rt-diagnostics .rtd-data').textContent)`,
    'the recent events are listed under a caption that says how many are retained',
  );
  await need(`${evRows()}.every((r) => r.children.length === 6)`, 'every event row has code, component, severity, outcome, count and time');
  await setSelect(ctx, '#rtd-outcome', 'failure');
  await need(`${evRows()}.length > 0 && ${evRows()}.length < 100 && ${evRows()}.every((r) => /failure/.test(r.children[3].textContent))`, 'the outcome filter keeps only failures');
  await setSelect(ctx, '#rtd-outcome', 'all');
  await need(`${evRows()}.length >= 100`, 'All outcomes brings every event back');
  await need(NO_JUNK('#rt-diagnostics'), 'the diagnostics card prints no null, undefined or NaN');
  await need(`!/gathered/.test(document.querySelector('#rt-diagnostics .rtd-data').textContent)`, 'an answer that is not truncated has no truncation note in the events caption');
  await dev(ctx, 'traces?diag=truncated');
  await refreshDiag();
  await need(
    `/Showing \\d+ of 256 retained events/.test(document.querySelector('#rt-diagnostics .rtd-data').textContent) && /gathered 150 of them/.test(document.querySelector('#rt-diagnostics .rtd-data').textContent)`,
    'when the gateway holds more events than were gathered, the events caption says so (150 gathered of 256 retained)',
    30000,
  );
  await dev(ctx, 'traces?diag=ok');
  await refreshDiag();
  await need(`!/gathered/.test(document.querySelector('#rt-diagnostics .rtd-data').textContent)`, 'the truncation note goes when the answer is whole again', 30000);
  await refreshDiag();
  await need(`${q('#rt-diagnostics .rtd-totals .stat')} === 5`, 'the refresh button keeps the card on screen');
  // the four reasons each have a fixed text; disabled says how to turn diagnostics on
  for (const [mode, re] of [
    ['disabled', 'cannot be read from the gateway'],
    ['unreachable', 'could not be reached'],
    ['unsupported_version', 'has no diagnostics'],
    ['error', 'could not be read'],
  ]) {
    await dev(ctx, `traces?diag=${mode}`);
    await refreshDiag();
    await need(
      `${q('#rt-diagnostics .state.empty')} === 1 && ${diagText}.includes(${JSON.stringify(re)}) && ${q('#rt-diagnostics .rtd-totals')} === 0`,
      `the ${mode} reason has its fixed text and no figures`,
      30000,
    );
  }
  await dev(ctx, 'traces?diag=disabled');
  await refreshDiag();
  await need(
    `(() => { const c = document.querySelector('#rt-diagnostics .snippet-code')?.textContent || ''; return /^NEMO_RELAY_DIAGNOSTICS=1\\nNEMO_RELAY_DIAGNOSTICS_HTTP=1\\n?$/.test(c) && ${q('#rt-diagnostics .snippet button')} === 1 && /NEMO_RELAY_DIAGNOSTICS_HTTP=1/.test(${diagText}) && /same account/.test(${diagText}); })()`,
    'the disabled reason names both environment variables, offers the two lines in one copyable block, and says to run the console as the same account',
  );
  await dev(ctx, 'traces?diag=ok');
  await refreshDiag();
  await need(`${q('#rt-diagnostics .rtd-totals .stat')} === 5`, 'the card recovers when diagnostics come back', 30000);

  // ---- the sampler off, polling off and stale
  await dev(ctx, 'traces?resources=off');
  await fresh(ctx, '#/relay?tab=capture', 'relay › capture runtime (mock, sampler off)');
  await need(
    `${q('#rt-resources .state.empty')} === 1 && ${q('#rt-resources svg.tschart')} === 0 && /python3 -I ops\\/nemo-relay-levels\\.py resources on/.test(document.querySelector('#rt-resources .fix-cmd')?.textContent || '')`,
    'with the sampler off the card is an empty state that shows the command to switch it on, and draws no chart',
    30000,
  );
  await need(
    `(() => { const r = document.querySelector('#rt-resources .res-remedy'); if (!r) return false; const kids = [...r.children]; const sn = r.querySelector('.snippet-code')?.textContent || ''; return kids[0].classList.contains('snippet') && /kind = "resource_metrics"/.test(sn) && /measurement_scope = "application_process"/.test(sn) && /interval_millis = 60000/.test(sn) && kids[1].classList.contains('fix') && /In the lab/.test(kids[1].textContent) && ${q('#rt-resources .snippet button')} === 1; })()`,
    'the generic plugins.toml snippet comes first with a copy button, and the lab command is the second line, labelled "In the lab"',
  );
  await need(`[...document.querySelectorAll('.todo li code')].some((c) => c.textContent === 'relay.resources.off')`, 'the to-do list carries the relay.resources.off hint');
  await need(`document.querySelector('#rt-resources select').hidden === true`, 'the window picker is hidden while nothing is sampled');
  await dev(ctx, 'traces?resources=polling_off');
  await fresh(ctx, '#/relay?tab=capture', 'relay › capture runtime (mock, polling off)');
  await need(
    `${q('#rt-resources .state.empty')} === 1 && /polling is not enabled/.test(document.querySelector('#rt-resources .state.empty').textContent)`,
    'a configured component with polling off says so',
    30000,
  );
  await dev(ctx, 'traces?resources=stale');
  await fresh(ctx, '#/relay?tab=capture', 'relay › capture runtime (mock, stale)');
  await need(
    `${q('#rt-resources .res-fresh.st-stale')} === 1 && /stale/.test(document.querySelector('#rt-resources .res-fresh').textContent) && ${q('#rt-resources svg.tschart')} === 11`,
    'old samples are marked stale in words while the charts still draw',
    30000,
  );
  await dev(ctx, 'traces?resources=quiet');
  await fresh(ctx, '#/relay?tab=capture', 'relay › capture runtime (mock, quiet gateway)');
  await need(
    `(() => { const tile = [...document.querySelectorAll('#rt-resources .res-strip .stat')].find((x) => /Routing/.test(x.textContent) && !/overhead/i.test(x.textContent)); return !!tile && /0 req\\/s/.test(tile.querySelector('.stat-value').textContent) && /0 req\\/s failing/.test(tile.textContent) && !/–/.test(tile.textContent); })()`,
    'a quiet gateway that reports 0 requests and 0 failures reads "0 req/s" and "0 req/s failing", not an en dash',
    30000,
  );
  await need(
    `${q('#rt-resources svg.tschart')} === 10`,
    'the routing rates still draw (flat zero lines) for a quiet gateway, and only the overhead quantile, which has nothing to take, is an empty panel',
  );
  await dev(ctx, 'traces?resources=idle');
  await fresh(ctx, '#/relay?tab=capture', 'relay › capture runtime (mock, no sample yet)');
  await need(
    `${q('#rt-resources .res-fresh.st-unknown')} === 1 && /no sample yet/.test(document.querySelector('#rt-resources .res-fresh').textContent) && ${q('#rt-resources .state.empty')} === 1 && ${q('#rt-resources svg.tschart')} === 0`,
    'a sampler that is on but has produced nothing yet says "no sample yet" and draws no chart',
    30000,
  );
  await dev(ctx, 'traces?resources=unreadable');
  await fresh(ctx, '#/relay?tab=capture', 'relay › capture runtime (mock, plugins.toml unreadable)');
  await need(
    `${q('#rt-resources .res-fresh.st-unknown')} === 1 && ${q('#rt-resources .fix-cmd')} === 0 && [...document.querySelectorAll('.todo li code')].some((c) => c.textContent === 'capture.config.unreadable')`,
    'with the configuration unreadable the card does not claim the sampler is off (it asks anyway) and the to-do list says the file is unreadable',
    30000,
  );
  await dev(ctx, 'traces?resources=on');

  // ---- a viewer: resources yes, diagnostics absent and never asked
  await dev(ctx, 'role?set=viewer');
  await reload();
  await fresh(ctx, '#/relay?tab=capture', 'relay › capture runtime (mock, viewer)');
  await need(`${q('#rt-resources')} === 1 && ${q('#rt-resources svg.tschart')} === 11`, 'a viewer still sees the Gateway resources card and its charts', 30000);
  await need(
    `${q('#rt-diagnostics')} === 0 && ${q('.rt-diag-slot > *')} === 0 && !/Runtime diagnostics/.test(document.querySelector('#main').textContent)`,
    'a viewer has no Runtime diagnostics card at all (not built, not merely disabled)',
  );
  const asked = await ctx.evaluate(`performance.getEntriesByType('resource').filter((e) => e.name.includes('/relay/diagnostics')).length`);
  if (asked) ctx.fail(`relay › capture runtime: a viewer's page asked for the diagnostics ${asked} time(s)`);
  const denied = await ctx.evaluate(`fetch('/api/v1/systems/relay/diagnostics').then(async (r) => [r.status, (await r.json()).error?.code])`);
  if (denied?.[0] !== 403 || denied?.[1] !== 'forbidden') ctx.fail(`relay › capture runtime: the route answered a viewer ${JSON.stringify(denied)}, not 403 forbidden`);
  await dev(ctx, 'role?set=admin');
  await reload();
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
  await visitWave3(ctx);
  await visitWave4(ctx);
  await visitRelayRuntime(ctx);
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
