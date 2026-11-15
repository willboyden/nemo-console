// Browser checks of the Relay trace explorer (docs/ARCHITECTURE.md addendum 9.12): the five new tabs and one detail
// view of each. Data-agnostic on purpose: it runs against the real backend with the synthetic capture fixtures
// (backend/crates/nemo-console/tests/fixtures/relay_traces) and against the frontend mock server, so it asserts that
// elements render, not what they say. Console errors, uncaught exceptions and 5xx answers are caught by the caller's
// listeners; this file adds the structural checks.
//
// ctx = { evaluate(expr), settle(), sleep(ms), fail(msg), route(name) }  (see e2e/run.mjs, which builds it)

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
  await ctx.sleep(200);
  await ctx.settle();
}

/** Every check below fails with a message naming what was expected; nothing here throws. */
export async function visitRelayTraces(ctx) {
  const need = async (expr, what, ms) => {
    const v = await until(ctx, expr, ms);
    if (!v) ctx.fail(`${what}`);
    return v;
  };

  // ---- Traces: index strip, filters, table, then one trace in all three views
  await open(ctx, '#/relay?tab=traces', 'relay › traces (seeded)');
  await need(`${q('.ix-strip .ix-state')} === 1`, 'the index strip shows a state chip');
  await need(`document.querySelector('.ix-strip .ix-state')?.textContent.includes('ready') || ${q('.trace-table tbody tr')} > 0`, 'the index becomes ready or lists traces', 60000);
  await need(`${q('.trace-table tbody tr')} > 0`, 'the traces table has at least one row', 30000);
  await need(`${q('.rt-filters select')} >= 3 && ${q('.range-controls [aria-pressed]')} === 8`, 'filters, window picker (6) and anchor toggle (2) are present');
  await need(`${q('.trace-table th[aria-sort]')} >= 3`, 'sortable column headers carry aria-sort');
  // window and sort live in the URL
  await ctx.evaluate(`[...document.querySelectorAll('.range-controls button')].find((b) => b.textContent === 'all').click(); true`);
  await need(`location.hash.includes('window=all')`, 'choosing a window writes it into the URL');
  await ctx.evaluate(`document.querySelector('.trace-table th button.sort-btn').click(); true`);
  await need(`/sort=|order=/.test(location.hash)`, 'sorting writes it into the URL');
  await ctx.evaluate(`document.querySelector('.trace-table a.row-link').click(); true`);
  await need(`location.hash.includes('trace=')`, 'opening a trace writes it into the URL');
  await need(`${q('.trace-summary')} === 1 && ${q('.wf-row')} >= 1`, 'the trace detail shows its summary and waterfall rows');
  await need(`${q('.wf-head .wf-tick')} >= 1`, 'the waterfall has a time axis');
  await ctx.evaluate(`document.querySelector('.wf-row').click(); true`);
  await need(`location.hash.includes('span=') && !document.querySelector('.sd').hidden`, 'selecting a span opens the details panel and writes span= into the URL');
  await need(`${q('.sd .kv')} >= 4`, 'the details panel lists the span fields');
  // zoom
  await ctx.evaluate(`document.querySelector('.wf-toolbar button[title]')?.click(); true`);
  await ctx.evaluate(`[...document.querySelectorAll('.wf-toolbar button')].find((b) => b.textContent === 'Reset zoom')?.click(); true`);
  // content on demand: nothing is fetched until the button is pressed
  const llmRow = await ctx.evaluate(`(() => { const r = [...document.querySelectorAll('.wf-row')].find((x) => x.querySelector('.cat-llm')); if (!r) return false; r.click(); return true; })()`);
  if (llmRow) {
    await need(`[...document.querySelectorAll('.sd button')].some((b) => b.textContent.includes('Load content'))`, 'an LLM span offers a Load content button');
    await ctx.evaluate(`[...document.querySelectorAll('.sd button')].find((b) => b.textContent.includes('Load content')).click(); true`);
    await need(`${q('.cnt-body .convo')} + ${q('.cnt-body .note')} + ${q('.cnt-body .cnt-sec')} > 0`, 'the content request ends in content or a plain explanation');
    const injected = await ctx.evaluate(`!!document.querySelector('.cnt-body script, .cnt-body img, .cnt-body [onerror]')`);
    if (injected) ctx.fail('captured text produced markup (script, img or an inline handler) in the content view');
  }
  for (const [label, sel, what] of [
    ['Icicle', '.ic-node', 'the icicle view draws span nodes'],
    ['Span table', '.span-table tbody tr', 'the span table lists spans'],
    ['Waterfall', '.wf-row', 'switching back restores the waterfall'],
  ]) {
    await ctx.evaluate(`[...document.querySelectorAll('.tv-switch button')].find((b) => b.textContent === ${JSON.stringify(label)}).click(); true`);
    await need(`${q(sel)} >= 1`, what);
  }
  // a deep link survives a reload of the hash
  const deep = await ctx.evaluate(`location.hash`);
  await open(ctx, '#/overview', 'relay › deep link');
  await open(ctx, deep, 'relay › traces deep link');
  await need(`${q('.trace-summary')} === 1 && ${q('.wf-row')} >= 1`, 'a trace deep link reopens the trace');

  // ---- Sessions
  await open(ctx, '#/relay?tab=sessions&window=all', 'relay › sessions (seeded)');
  await need(`${q('.trace-table tbody tr')} > 0`, 'the sessions table has at least one row', 30000);
  await ctx.evaluate(`document.querySelector('.trace-table a.row-link').click(); true`);
  await need(`location.hash.includes('session=') && ${q('svg.swimlane')} === 1`, 'a session shows its swim-lane timeline');
  await need(`${q('svg.ctxchart')} + ${q('.chart-empty')} >= 1`, 'a session shows the context growth chart or its empty state');

  // ---- Analytics
  await open(ctx, '#/relay?tab=analytics&window=all', 'relay › analytics (seeded)');
  await need(`${q('.kpis .stat')} >= 5`, 'analytics shows KPI tiles', 30000);
  await need(`${q('svg.rtchart')} >= 2`, 'analytics draws its charts');
  await need(`${q('.chart-card details.chart-data')} >= 1`, 'charts offer a data table as text alternative');
  await need(`[...document.querySelectorAll('svg.rtchart')].every((s) => s.getAttribute('aria-label'))`, 'every chart has an aria-label');

  // ---- Trajectories
  await open(ctx, '#/relay?tab=trajectories', 'relay › trajectories (seeded)');
  await need(`${q('.trace-table tbody tr')} > 0`, 'the trajectories table has at least one row', 30000);
  await ctx.evaluate(`document.querySelector('.trace-table a.row-link').click(); true`);
  await need(`location.hash.includes('id=') && ${q('.timeline .tl-item')} >= 1`, 'a trajectory shows its step timeline');

  // ---- Capture
  await open(ctx, '#/relay?tab=capture', 'relay › capture (seeded)');
  await need(`${q('.cap-table tbody tr.cap-row')} >= 9`, 'the capture matrix lists the nine levels', 30000);
  await need(`${q('.cap-meter')} === 1 && ${q('.proj-tile')} === 3`, 'the coverage meter and the three projections render');
}
