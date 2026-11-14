// A time-series chart card (addendum 9.13): one chart per metric key, fed by ONE `GET /systems/{id}/metrics/series` answer.
// Counters are drawn as rates, a split chart has one line per label (a picker when there are more than 4: the busiest 4 by
// default, the user's choice kept in the URL hash), "show max" adds the bucket max as a thin line and a band, every chart has
// a text alternative (a table of latest/min/avg/max per line). The card is updated in place when a newer answer arrives, so
// an open picker or data table survives the 10-second refresh.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { buildLines, normalizeAnswer, resolveSelection, encodeSelection, decodeSelection, lineStats, labelName, DEFAULT_SHOWN } from '../series-model.js';
import { seriesChart, lineSwatch } from './series-chart.js';
import { chartEmpty } from './parts.js';

const PICK = (key) => `s.${key}`;
/** 2 -> "2 s", 60 -> "1 min", 600 -> "10 min". */
const fmtStep = (s) => (s < 60 ? `${+s.toFixed(1)} s` : `${+(s / 60).toFixed(1)} min`);

/**
 * @param opts.title       card title
 * @param opts.key         the metric key (the picker's hash key is `s.<key>`)
 * @param opts.rate        draw the counter as a rate
 * @param opts.yFmt        (v) => axis tick text;  opts.valueFmt (v) => value text (tooltips, table)
 * @param opts.hash        a createHashState() (the picker's choice lives in it)
 * @param opts.win         () => the current window;  opts.showMax () => whether the max is drawn
 * Returns {el, update(answer), fail(error), setWindow, redraw}.
 */
export function seriesCard(scope, { title, key, rate = false, yFmt, valueFmt = yFmt, hash, win, showMax }) {
  let answer = null,
    error = null,
    lines = [],
    scale = null,
    shown = [],
    drawnNow = [];
  const wrap = h('div', { class: 'chart-wrap-lg' });
  const legendHost = h('div', { class: 'series-legend' });
  const noteHost = h('div');
  const caption = h('span', { class: 'muted small' });
  const tableBody = h('div', { class: 'tablewrap tall' });
  const details = h('details', { class: 'chart-data' }, h('summary', t('chart.data_table')), tableBody);
  const summary = h('p', { class: 'sr-only' });
  const el = h(
    'section',
    { class: 'card chart-card rt-chart series-card', 'aria-label': title },
    h('header', { class: 'card-head' }, h('h3', title), caption),
    h('div', { class: 'card-body' }, noteHost, wrap, summary, legendHost, details),
  );

  const perSuffix = () => (scale?.per === 'min' ? '/min' : '/s');
  const val = (v) => (v == null ? '–' : rate ? `${valueFmt(v)}${perSuffix()}` : valueFmt(v));
  const nameOf = (l) => labelName(l.label, title);

  function describe() {
    const bits = [];
    if (rate) bits.push(scale?.per === 'min' ? t('series.per_min') : t('series.per_s'));
    if (answer?.step_s) bits.push(t('series.step', { step: fmtStep(answer.step_s) }));
    caption.textContent = bits.join(' · ');
  }

  function draw() {
    const width = Math.max(260, Math.round(wrap.clientWidth || 560));
    const sel = shown.map((id) => lines.find((l) => l.id === id)).filter(Boolean);
    const drawn = sel.map((l) => ({ key: l.key, name: nameOf(l), idx: lines.indexOf(l), segments: l.segments }));
    const label = t('chart.summary', { title: `${title}, ${t(`window.${win()}`)}`, latest: drawn.map((d) => `${d.name} ${val(lineStats(d.segments).latest)}`).join(', ') || t('common.no_data') });
    const chart = drawn.length
      ? seriesChart({ lines: drawn, win: win(), from: answer.from, to: answer.to, width, height: width < 420 ? 190 : 210, yFmt, valueFmt: val, showMax: showMax() && !rate, ariaLabel: label })
      : null;
    wrap.replaceChildren(chart ?? chartEmpty(lines.length ? t('series.none_shown') : t('series.empty')));
    summary.textContent = chart ? label : '';
    if (details.open) fillTable(drawn);
    drawnNow = drawn;
  }

  function fillTable(drawn = drawnNow) {
    const head = [t('chart.series'), t('series.latest'), t('series.min'), t('series.avg'), t('series.max')];
    const rows = drawn.map((d) => {
      const s = lineStats(d.segments);
      return [d.name, val(s.latest), val(s.min), val(s.avg), val(s.max)];
    });
    tableBody.replaceChildren(
      h(
        'table',
        { class: 'compact', 'aria-label': t('chart.data_table') },
        h(
          'thead',
          h(
            'tr',
            head.map((c, i) => h('th', { scope: 'col', class: i ? 'al-right' : '' }, c)),
          ),
        ),
        h(
          'tbody',
          rows.map((r) =>
            h(
              'tr',
              r.map((c, i) => h('td', { class: i ? 'num al-right' : '' }, c)),
            ),
          ),
        ),
      ),
    );
  }
  details.addEventListener('toggle', () => details.open && fillTable());

  function setPick(ids) {
    hash?.set({ [PICK(key)]: ids == null ? null : encodeSelection(ids) });
    shown = resolveSelection(lines, ids, DEFAULT_SHOWN);
    drawLegend();
    draw();
  }

  function drawLegend() {
    if (!lines.length) {
      legendHost.replaceChildren();
      return;
    }
    // Up to 4 lines: a plain legend. More: a picker (toggle buttons), the busiest 4 shown until the user chooses.
    if (lines.length <= DEFAULT_SHOWN) {
      legendHost.replaceChildren(
        h(
          'ul',
          { class: 'legend' },
          lines.map((l, i) => h('li', lineSwatch(i), h('span', nameOf(l)))),
        ),
      );
      return;
    }
    const chips = lines.map((l, i) => {
      const on = shown.includes(l.id);
      const b = h('button', { type: 'button', class: 'series-chip', 'aria-pressed': String(on), title: nameOf(l) }, lineSwatch(i), h('span', { class: 'series-chip-name' }, nameOf(l)));
      b.addEventListener('click', () => setPick(on ? shown.filter((x) => x !== l.id) : [...shown, l.id]));
      return b;
    });
    const busiestBtn = h('button', { type: 'button', class: 'btn sm' }, h('span', t('series.busiest', { n: DEFAULT_SHOWN })));
    busiestBtn.addEventListener('click', () => setPick(null));
    const allBtn = h('button', { type: 'button', class: 'btn sm' }, h('span', t('series.all')));
    allBtn.addEventListener('click', () => setPick(lines.map((l) => l.id)));
    legendHost.replaceChildren(
      h(
        'div',
        { class: 'series-picker', role: 'group', 'aria-label': t('series.picker', { shown: shown.length, total: lines.length }) },
        h(
          'div',
          { class: 'row gap wrap between' },
          h('span', { class: 'muted small', role: 'status' }, t('series.picker', { shown: shown.length, total: lines.length })),
          h('div', { class: 'row gap' }, busiestBtn, allBtn),
        ),
        h('div', { class: 'series-chips' }, chips),
      ),
    );
  }

  function rebuild() {
    noteHost.replaceChildren();
    if (error) {
      noteHost.append(h('p', { class: 'note warn', role: 'status' }, error));
      if (!answer) {
        wrap.replaceChildren();
        return;
      }
    }
    const built = buildLines(normalizeAnswer(answer), { rate, stepS: answer.step_s });
    lines = built.lines;
    scale = built.rate;
    const saved = lines.length > DEFAULT_SHOWN ? decodeSelection(hash?.get(PICK(key), '') || '') : null;
    shown = resolveSelection(lines, saved, DEFAULT_SHOWN);
    if (answer.truncated) noteHost.append(h('p', { class: 'note info', role: 'status' }, t('series.truncated')));
    describe();
    drawLegend();
    draw();
  }

  let lastW = 0;
  if (typeof ResizeObserver !== 'undefined') {
    const ro = new ResizeObserver(() => {
      const w = Math.round(wrap.clientWidth);
      if (!answer || w < 80 || Math.abs(w - lastW) < 6) return;
      lastW = w;
      draw();
    });
    ro.observe(wrap);
    scope.own(() => ro.disconnect());
  }

  return {
    el,
    update(a) {
      answer = a;
      error = null;
      rebuild();
    },
    fail(msg) {
      error = msg;
      if (answer) rebuild();
      else {
        noteHost.replaceChildren(h('p', { class: 'note warn', role: 'status' }, msg));
        wrap.replaceChildren();
      }
    },
    /** Redraw with a changed "show max" without asking the server again. */
    redraw() {
      if (answer) draw();
    },
  };
}
