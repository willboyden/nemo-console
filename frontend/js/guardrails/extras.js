// The Guardrails Status additions (addendum 9.16): the rail pipeline of each served config (input rails -> main model -> output rails, the streaming settings, the retrieval flows) from
// `GET /systems/guardrails/rails`, and an up / down strip from the `gr.up` history under the page's window picker. Both are text first: the pipeline is a list of named flows (no
// prompt text, no `.co` content is ever served), the strip has a summary sentence and a table of the down periods. Everything from the server goes in as text.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { describeError } from '../errors.js';
import { fmtDur, fmtStamp } from '../viz/fmt.js';
import { badge, btn, emptyBox } from '../components/ui.js';
import { fitted } from '../charts/fit.js';
import { stripChart } from '../charts/strip-chart.js';
import { dataTable, swap, windowPhrase } from '../charts/parts.js';
import { shapeRails, pipelineStages } from '../guardrails-model.js';
import { stripFromAnswer } from '../strip-model.js';
import { stripLegend, stripSummary } from '../charts/strip-parts.js';

const RAILS_EVERY_MS = 60000;

/** The words for one problem the rails route reports ({config, code, count}); a code this console does not know is shown with its fields. */
export function railProblemText(p) {
  const key = `grv.problem_${String(p.code).replace(/\./g, '_')}`;
  const vars = { config: p.config ?? '–', count: p.count ?? '–', code: p.code };
  return t(key) === key ? t('grv.problem_other', vars) : t(key, vars);
}
const GAP_S = { '15m': 10, '1h': 30, '24h': 60, '7d': 120 };

/** One stage of the pipeline: a titled box holding an ordered list of flows (or "no rail here"). */
function stageBox(kind, flows, none) {
  return h(
    'li',
    { class: `gr-stage ${kind}${none ? ' none' : ''}` },
    h('span', { class: 'gr-stage-title' }, t(`grv.stage_${kind}`)),
    none
      ? h('span', { class: 'muted small' }, t('grv.no_rail'))
      : h(
          'ol',
          { class: 'gr-flows', 'aria-label': t(`grv.stage_${kind}`) },
          flows.map((f) => h('li', h('code', f))),
        ),
  );
}

const arrow = () => h('li', { class: 'gr-arrow', 'aria-hidden': 'true' }, '→');
const endpoint = (word) => h('li', { class: 'gr-end' }, h('span', { class: 'gr-end-word' }, word));

/** One config's diagram and the facts under it. */
export function configDiagram(cfg) {
  const [inp, mod, out] = pipelineStages(cfg);
  const s = cfg.streaming;
  const streamText = !s ? t('grv.streaming_none') : s.enabled === false ? t('grv.streaming_off') : t('grv.streaming_on', { chunk: s.chunkSize ?? '–', context: s.contextSize ?? '–' });
  return h(
    'article',
    { class: 'gr-config', 'aria-label': t('grv.config_aria', { id: cfg.id }) },
    h('header', { class: 'row gap wrap between' }, h('h4', { class: 'gr-config-id' }, h('code', cfg.id)), badge(t('grv.rail_count', { count: cfg.rails }), cfg.rails ? 'info' : 'warn')),
    h(
      'ol',
      { class: 'gr-pipe', 'aria-label': t('grv.pipe_aria', { id: cfg.id }) },
      endpoint(t('grv.request')),
      arrow(),
      stageBox('input', inp.flows, inp.none),
      arrow(),
      h(
        'li',
        { class: 'gr-stage model' },
        h('span', { class: 'gr-stage-title' }, t('grv.stage_model')),
        mod.model ? h('code', { class: 'gr-model' }, mod.model) : h('span', { class: 'muted small' }, t('grv.model_unknown')),
      ),
      arrow(),
      stageBox('output', out.flows, out.none),
      arrow(),
      endpoint(t('grv.response')),
    ),
    h(
      'dl',
      { class: 'facts gr-facts' },
      h('div', { class: 'kv' }, h('dt', t('grv.streaming')), h('dd', streamText)),
      cfg.retrieval.length
        ? h(
            'div',
            { class: 'kv' },
            h('dt', t('grv.retrieval')),
            h(
              'dd',
              cfg.retrieval.map((f) => h('code', { class: 'gr-flow-chip' }, f)),
            ),
          )
        : null,
    ),
  );
}

/** The rail pipeline card: a diagram per config, notes for a refused or absent route. Returns {el, load()}. */
function railsCard(scope) {
  const body = h('div', { class: 'stack-v' }, h('p', { class: 'muted' }, t('common.loading')));
  const el = h(
    'section',
    { class: 'card chart-card gr-rails', 'aria-label': t('grv.rails_title') },
    h('header', { class: 'card-head' }, h('h3', t('grv.rails_title')), btn(t('common.refresh'), { size: 'sm', icon: 'restart', onClick: () => load() })),
    h('div', { class: 'card-body stack-v' }, h('p', { class: 'hint' }, t('grv.rails_hint')), body),
  );
  let sig = '',
    seq = 0;
  async function load() {
    const my = ++seq;
    try {
      const a = await scope.api.rails();
      if (!scope.alive || my !== seq) return;
      const raw = JSON.stringify(a);
      if (raw === sig) return;
      sig = raw;
      const m = shapeRails(a);
      swap(
        body,
        ...m.problems.map((p) => h('p', { class: 'note warn', role: 'status' }, railProblemText(p), h('code', { class: 'muted small' }, ` ${p.code}`))),
        m.configs.length ? m.configs.map(configDiagram) : emptyBox(t('grv.rails_empty'), t('grv.rails_empty_hint')),
      );
    } catch (e) {
      if (!scope.alive || my !== seq) return;
      sig = '';
      swap(body, e?.status === 404 ? emptyBox(t('grv.rails_unsupported'), t('grv.rails_unsupported_hint')) : h('p', { class: 'note warn', role: 'status' }, describeError(e)));
    }
  }
  return { el, load };
}

/** The availability card: the up / down strip of `gr.up` for the page's window. Returns {el, load(force)}. */
function availabilityCard(scope, win) {
  const summaryHost = h('p', { class: 'gr-up-summary', role: 'status' });
  const stripHost = h('div');
  const tableHost = h('div');
  const legendHost = h('div');
  const el = h(
    'section',
    { class: 'card chart-card gr-up', 'aria-label': t('grv.up_title') },
    h('header', { class: 'card-head' }, h('h3', t('grv.up_title'))),
    h('div', { class: 'card-body stack-v' }, h('p', { class: 'hint' }, t('grv.up_hint')), summaryHost, stripHost, legendHost, tableHost),
  );
  let model = null,
    last = 0,
    busy = false;
  const draw = () => {
    if (!model) return;
    const w = win();
    if (model.empty) {
      swap(summaryHost, t('grv.up_none', { window: windowPhrase(w) }));
      swap(stripHost);
      swap(legendHost);
      swap(tableHost);
      return;
    }
    swap(summaryHost, stripSummary(model, windowPhrase(w), t('grv.up_noun')));
    const label = t('grv.up_alt', { summary: stripSummary(model, windowPhrase(w), t('grv.up_noun')) });
    swap(
      stripHost,
      fitted(scope, (width) => stripChart({ cells: model.cells, width, ariaLabel: label }), null),
    );
    swap(legendHost, stripLegend());
    swap(
      tableHost,
      model.downRuns.length
        ? dataTable(t('grv.up_table'), [t('grv.up_col_from'), t('grv.up_col_to'), t('grv.up_col_len')], () =>
            model.downRuns.map((r) => [`${fmtStamp(r.from)} UTC`, `${fmtStamp(r.to)} UTC`, fmtDur((r.to - r.from) * 1000)]),
          )
        : null,
    );
  };
  async function load(force = false) {
    const w = win();
    if (busy || (!force && Date.now() - last < (GAP_S[w] ?? 30) * 1000)) return;
    busy = true;
    last = Date.now();
    try {
      const a = await scope.api.series('guardrails', { keys: ['gr.up'], window: w });
      if (!scope.alive || w !== win()) return;
      model = stripFromAnswer(a, 'gr.up', { maxCells: 96 }) ?? { empty: true };
      draw();
    } catch (e) {
      if (scope.alive && !model) swap(summaryHost, describeError(e));
    } finally {
      busy = false;
    }
  }
  return { el, load, draw };
}

/** `desc.extras(scope, shared)` of the Guardrails page: returns {el}. */
export function guardrailsExtras(scope, shared) {
  const rails = railsCard(scope);
  const up = availabilityCard(scope, shared.win);
  shared.onWindow(() => up.load(true));
  rails.load();
  up.load(true);
  scope.every(RAILS_EVERY_MS, () => rails.load());
  scope.every(10000, () => up.load(false));
  const el = h('div', { class: 'stack-v gr-viz' }, h('h2', t('grv.title')), rails.el, up.el);
  return { el };
}
