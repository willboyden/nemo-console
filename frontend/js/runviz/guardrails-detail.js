// The Guardrails probe matrix (addendum 9.16), under the run history: for the runs the history shows, a row per probe (benign, jailbreak, harmful), a column per check run, and in each cell
// what the rails answered against what they should have. A match is a check mark, an unexpected answer (a jailbreak that was let through, a benign prompt that was blocked) a cross, a probe
// that errored an exclamation mark, a probe the run never reached (the check stops at the first error) a hatched cell: the glyph and the word carry the meaning as well as the colour.
// Under it a latency strip: the same rows and columns, each cell the probe's time in ms on a colour ramp (darker is slower), a timeout flagged. No prompt text is shown or stored: the check's
// prompts are fixed in the adapter. Clicking a column header selects that run in the history above. Words: i18n keys `grv.*`.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtMs } from '../format.js';
import { levelOf } from '../viz/heat.js';
import { cellMatrix } from '../charts/cell-matrix.js';
import { dataTable, chartEmpty } from '../charts/parts.js';
import { badge } from '../components/ui.js';
import { probeMatrix } from '../guardrails-model.js';
import { vizCard, chartPanel, repaintKeepFocus } from './kit.js';

const LABEL_W = 78; // the row labels of the matrices (the longest probe name) and the right margin, so the columns that fit can be counted before drawing
const MIN_CELL = 40;

const msText = (ms) => (ms == null ? '–' : ms >= 10000 ? `${Math.round(ms / 1000)}s` : fmtMs(ms).replace(' ms', '').replace(' s', 's'));

export function guardrailsDetail(scope) {
  const el = h('div', { class: 'stack-v rv-detail' });
  let ctx = null,
    rs = null;

  /** How many of the newest runs a chart of this width can hold without shrinking its cells below MIN_CELL. */
  const fit = (w) => Math.max(6, Math.min(24, Math.floor((w - LABEL_W - 12) / MIN_CELL)));

  function cellOf(model, p, ci, showWord) {
    const c = model.cells[p.name][ci];
    if (!c) return null; // not reached: a hatched cell that says so in its aria-label
    const col = model.cols[ci];
    const base = t('grv.cell', { probe: p.name, when: col.title, got: c.word ?? '–', want: c.expectedWord ?? '–' });
    if (c.error) return { cls: 'cm-warn', tone: 'warn', glyph: '!', text: showWord ? t('grv.word_error') : null, label: `${base}. ${t('grv.cell_error')}` };
    if (c.match) return { cls: 'cm-ok', tone: 'ok', glyph: '✓', text: showWord ? c.word : null, label: `${base}. ${t('grv.cell_match')}` };
    if (c.match === false) return { cls: 'cm-bad', tone: 'bad', glyph: '✕', text: showWord ? c.word : null, label: `${base}. ${t('grv.cell_slip')}` };
    return { cls: 'cm-muted', tone: 'muted', glyph: '?', label: `${base}. ${t('grv.cell_unknown')}` };
  }

  function latencyCell(model, p, ci) {
    const c = model.cells[p.name][ci];
    if (!c || c.ms == null) return null;
    const col = model.cols[ci];
    const base = t('grv.lat_cell', { probe: p.name, when: col.title, ms: c.timeout ? t('grv.timeout') : fmtMs(c.ms) });
    if (c.timeout) return { cls: 'cm-warn', tone: 'warn', glyph: '!', label: base }; // a timeout is a glyph: the label says "timed out"
    const level = levelOf(c.ms, { min: 0, max: model.maxMs ?? c.ms, curve: 'sqrt' });
    return { cls: `heatc fh${level}`, level, text: msText(c.ms), label: base };
  }

  function matrixCard(rows) {
    const legend = h(
      'ul',
      { class: 'legend' },
      h('li', h('i', { class: 'sw cm-sw cm-ok', 'aria-hidden': 'true' }), h('span', `✓ ${t('grv.key_match')}`)),
      h('li', h('i', { class: 'sw cm-sw cm-bad', 'aria-hidden': 'true' }), h('span', `✕ ${t('grv.key_slip')}`)),
      h('li', h('i', { class: 'sw cm-sw cm-warn', 'aria-hidden': 'true' }), h('span', `! ${t('grv.key_error')}`)),
      h('li', h('i', { class: 'sw heat-sw heat-none-sw', 'aria-hidden': 'true' }), h('span', t('grv.key_none'))),
    );
    const full = probeMatrix(rows, { max: 24 }); // for the facts and the table (the chart itself draws as many of the newest runs as its width holds)
    const hiddenNote = h('p', { class: 'muted small' }); // says how many runs the chart left out at THIS width; set by the draw below
    // (declared above the body so the draw callbacks can set it)
    const body = rows.length
      ? [
          chartPanel(rs, {
            title: t('grv.matrix_title'),
            build: (w) => {
              const m = probeMatrix(rows, { max: fit(w) });
              hiddenNote.textContent = m.hidden ? t('grv.hidden', { count: m.hidden, shown: m.cols.length }) : '';
              if (!m.cols.length) return null;
              const showWord = (w - LABEL_W - 12) / m.cols.length >= 54;
              return cellMatrix({
                cols: m.cols.map((c) => ({ id: c.id, label: c.label, title: c.title })),
                rows: m.probes.map((p) => ({ id: p.name, label: `${p.name} → ${p.expectedWord ?? '?'}` })),
                cell: (ri, ci) => cellOf(m, m.probes[ri], ci, showWord),
                width: w,
                minCellW: MIN_CELL,
                ariaLabel: t('grv.matrix_alt', { runs: m.cols.length, probes: m.probes.length, slips: m.stats.slips, errors: m.stats.errors }),
                noDataLabel: t('grv.not_reached'),
                onPick: (id) => ctx.pick(id, false),
                selected: { a: ctx.A?.id, b: ctx.B?.id },
              });
            },
            empty: chartEmpty(t('grv.matrix_none')),
            legend,
          }),
          chartPanel(rs, {
            title: t('grv.latency_title'),
            build: (w) => {
              const m = probeMatrix(rows, { max: fit(w) });
              if (!m.cols.length || m.maxMs == null) return null;
              return cellMatrix({
                cols: m.cols.map((c) => ({ id: c.id, label: c.label, title: c.title })),
                rows: m.probes.map((p) => ({ id: p.name, label: p.name })),
                cell: (ri, ci) => latencyCell(m, m.probes[ri], ci),
                width: w,
                minCellW: MIN_CELL,
                ariaLabel: t('grv.latency_alt', { runs: m.cols.length, max: fmtMs(m.maxMs) }),
                noDataLabel: t('grv.no_latency'),
                onPick: (id) => ctx.pick(id, false),
                selected: { a: ctx.A?.id, b: ctx.B?.id },
              });
            },
            empty: chartEmpty(t('grv.latency_none')),
            sub: h('p', { class: 'muted small' }, t('grv.latency_hint')),
          }),
          hiddenNote,
          h(
            'p',
            { class: 'rv-facts-line' },
            badge(`✓ ${t('grv.facts_clean', { count: full.stats.clean })}`, 'ok'),
            ' ',
            full.stats.slips ? badge(`✕ ${t('grv.facts_slips', { count: full.stats.slips })}`, 'bad') : null,
            full.stats.slips ? ' ' : null,
            full.stats.errors ? badge(`! ${t('grv.facts_errors', { count: full.stats.errors })}`, 'warn') : null,
          ),
          dataTable(t('chart.data_table'), [t('grv.col_probe'), t('grv.col_expected'), t('grv.col_match'), t('grv.col_slip'), t('grv.col_error'), t('grv.col_p50'), t('grv.col_max')], () =>
            full.probes.map((p) => {
              const s = full.stats.perProbe[p.name];
              return [p.name, p.expectedWord ?? '–', `${s.match} / ${s.n}`, s.mismatch, s.errors, s.p50 == null ? '–' : fmtMs(s.p50), s.max == null ? '–' : fmtMs(s.max)];
            }),
          ),
        ]
      : [chartEmpty(t('grv.matrix_none'))];
    return vizCard({ title: t('grv.probes_title'), hint: t('grv.probes_hint'), wide: true }, ...body);
  }

  function paint() {
    rs?.dispose();
    rs = scope.child();
    repaintKeepFocus(el, () => [h('h2', { class: 'rv-h' }, t('rv.detail')), matrixCard(ctx.view)]);
  }
  return {
    el,
    update(c) {
      ctx = c;
      paint();
    },
  };
}
