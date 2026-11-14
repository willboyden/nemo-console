// Flat, sortable list of the trace's spans: name, category, start, duration, tokens, status. Paginated (100 per page)
// so a 5000-span trace never renders thousands of rows; selecting a span elsewhere jumps to its page.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { relMs } from '../viz/tree.js';
import { byKey, fmtDur, fmtTok, categoryOf } from '../viz/fmt.js';
import { fmtOffsetMs } from '../viz/scale.js';
import { catChip, statusChip, pager, sortTh, dash, swap } from './ui.js';

const PAGE = 100;
const KEYS = {
  name: (s) => String(s.name ?? '').toLowerCase(),
  category: (s) => categoryOf(s),
  start: (s, m) => relMs(s.start_ts ?? s.end_ts, m.tl),
  duration: (s) => s.duration_ms,
  tokens: (s) => s.llm?.total_tokens ?? null,
  status: (s) => s.status,
};

export function createSpanTable({ model, onSelect, selected }) {
  let m = model,
    sel = selected ?? null,
    sort = { sort: 'start', order: 'asc' },
    page = 0,
    text = '',
    cat = '';
  const host = h('div', { class: 'tbl-host' });
  const input = h('input', { type: 'search', 'aria-label': t('rt.st.filter'), placeholder: t('rt.st.filter_ph'), autocomplete: 'off', maxlength: 80 });
  input.addEventListener('input', () => {
    text = input.value.trim().toLowerCase();
    page = 0;
    draw();
  });
  const catSel = h(
    'select',
    { 'aria-label': t('rt.st.category') },
    h('option', { value: '' }, t('rt.filter.all')),
    ['agent', 'turn', 'llm', 'tool', 'other'].map((c) => h('option', { value: c }, t(`rt.cat.${c}`))),
  );
  catSel.addEventListener('change', () => {
    cat = catSel.value;
    page = 0;
    draw();
  });
  const el = h(
    'div',
    { class: 'stack-v' },
    h(
      'div',
      { class: 'row gap wrap' },
      h('div', { class: 'field filter grow' }, h('label', {}, t('rt.st.filter')), input),
      h('div', { class: 'field filter' }, h('label', {}, t('rt.st.category')), catSel),
    ),
    host,
  );

  const sorted = () => {
    const list = m.spans.filter((s) => (!cat || categoryOf(s) === cat) && (!text || `${s.name} ${s.llm?.model ?? ''} ${s.tool?.name ?? ''}`.toLowerCase().includes(text)));
    const key = KEYS[sort.sort];
    return list.sort(byKey((s) => key(s, m), sort.order));
  };
  function draw() {
    const list = sorted();
    const pages = Math.max(1, Math.ceil(list.length / PAGE));
    page = Math.min(page, pages - 1);
    const slice = list.slice(page * PAGE, page * PAGE + PAGE);
    const onSort = (k, o) => {
      sort = { sort: k, order: o };
      draw();
    };
    swap(
      host,
      h(
        'div',
        { class: 'tablewrap tall' },
        h(
          'table',
          { class: 'compact span-table', 'aria-label': t('rt.st.table') },
          h(
            'thead',
            h(
              'tr',
              sortTh(t('rt.st.name'), 'name', sort, onSort),
              sortTh(t('rt.st.category'), 'category', sort, onSort),
              sortTh(t('rt.st.start'), 'start', sort, onSort, 'al-right'),
              sortTh(t('rt.st.duration'), 'duration', sort, onSort, 'al-right'),
              sortTh(t('rt.st.tokens'), 'tokens', sort, onSort, 'al-right'),
              sortTh(t('rt.st.status'), 'status', sort, onSort),
            ),
          ),
          h(
            'tbody',
            slice.map((s) => {
              const tr = h(
                'tr',
                { class: `${sel === s.id ? 'sel' : ''}${s.status === 'error' ? ' err' : ''}`, 'aria-selected': String(sel === s.id), dataset: { id: s.id } },
                h(
                  'td',
                  h('button', { type: 'button', class: 'link', onClick: () => onSelect(s.id) }, s.status === 'error' ? '✕ ' : '', s.name ?? '–'),
                  s.llm ? h('div', { class: 'muted small' }, s.llm.model, s.llm.served_model ? ` → ${s.llm.served_model}` : '') : null,
                ),
                h('td', catChip(s)),
                h('td', { class: 'num al-right' }, fmtOffsetMs(relMs(s.start_ts ?? s.end_ts, m.tl))),
                h('td', { class: 'num al-right' }, s.status === 'open' ? t('rt.status.open') : fmtDur(s.duration_ms)),
                h('td', { class: 'num al-right' }, s.llm ? t('rt.tokens.pair', { input: fmtTok(s.llm.input_tokens), output: fmtTok(s.llm.output_tokens) }) : dash()),
                h('td', statusChip(s.status)),
              );
              tr.addEventListener('click', (e) => {
                if (!e.target.closest('button')) onSelect(s.id);
              });
              return tr;
            }),
          ),
        ),
      ),
      pager({ offset: page * PAGE, limit: PAGE, total: list.length, onPage: (off) => ((page = Math.floor(off / PAGE)), draw()) }),
    );
  }
  draw();
  return {
    el,
    setSelected(id, { reveal = false } = {}) {
      sel = id;
      if (reveal && id) {
        const i = sorted().findIndex((s) => s.id === id);
        if (i >= 0) page = Math.floor(i / PAGE);
      }
      draw();
    },
    update(next) {
      m = next;
      draw();
    },
  };
}
