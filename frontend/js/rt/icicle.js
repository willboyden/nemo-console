// Icicle view of the span tree: width = duration (children share their parent's width in proportion to their own),
// depth = nesting. Click a span to focus its subtree (it becomes the full-width top row) and select it; the breadcrumb
// walks back up. Spans narrower than ~0.2% are not drawn (their time still counts in the parent's width).
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { btn } from '../components/ui.js';
import { icicleLayout } from '../viz/tree.js';
import { categoryOf, fmtDur } from '../viz/fmt.js';
import { spanLabel } from './waterfall.js';
import { legendList, swap } from './ui.js';

const ROW = 26;
const pct = (f) => `${(f * 100).toFixed(3)}%`;

export function createIcicle({ model, onSelect, selected }) {
  let m = model,
    sel = selected ?? null,
    focus = null,
    els = new Map();
  const crumbs = h('nav', { class: 'ic-crumbs row gap wrap', 'aria-label': t('rt.ic.path') });
  const note = h('p', { class: 'note info small', hidden: true }, t('rt.ic.parallel'));
  const canvas = h('div', { class: 'ic-canvas', role: 'group', 'aria-label': t('rt.ic.canvas') });
  const el = h(
    'div',
    { class: 'ic' },
    crumbs,
    canvas,
    note,
    h('p', { class: 'hint' }, t('rt.ic.hint')),
    legendList([
      { cls: 'cat-agent', label: t('rt.cat.agent') },
      { cls: 'cat-turn', label: t('rt.cat.turn') },
      { cls: 'cat-llm', label: t('rt.cat.llm') },
      { cls: 'cat-tool', label: t('rt.cat.tool') },
      { cls: 'cat-other', label: t('rt.cat.other') },
      { cls: 'lg-err', label: t('rt.wf.lg_error') },
      { cls: 'lg-open', label: t('rt.wf.lg_open') },
    ]),
  );

  function draw() {
    const lay = icicleLayout(m.tree, m.tl, focus && m.tree.nodes.has(focus) ? focus : null, { minFrac: 0.002 });
    els = new Map();
    note.hidden = !lay.parallel;
    swap(
      crumbs,
      btn(t('rt.ic.whole'), { size: 'sm', disabled: !lay.focus, onClick: () => setFocus(null) }),
      ...lay.path.flatMap((n) => [
        h('span', { class: 'muted', 'aria-hidden': 'true' }, '›'),
        btn(n.span.name ?? '–', { size: 'sm', kind: n.id === lay.focus?.id ? 'primary' : '', onClick: () => setFocus(n.id) }),
      ]),
    );
    canvas.style.height = `${(lay.maxDepth + 1) * ROW}px`;
    const nodes = lay.rects.map((r) => {
      const s = r.node.span;
      const w = r.x1 - r.x0;
      const b = h(
        'button',
        {
          type: 'button',
          class: `ic-node cat-${categoryOf(s)}${s.status === 'error' ? ' err' : ''}${s.status === 'open' ? ' open' : ''}${sel === r.id ? ' sel' : ''}`,
          tabindex: w >= 0.012 ? '0' : '-1',
          'aria-label': spanLabel(s),
          'aria-pressed': String(sel === r.id),
          title: `${spanLabel(s)}\n${t('rt.ic.share', { pct: (w * 100).toFixed(1) })}`,
        },
        h('span', { class: 'ic-text' }, s.status === 'error' ? '✕ ' : '', s.name ?? '–'),
        w > 0.08 ? h('span', { class: 'ic-dur num' }, fmtDur(s.duration_ms)) : null,
      );
      b.style.left = pct(r.x0);
      b.style.width = pct(w);
      b.style.top = `${r.depth * ROW}px`;
      b.addEventListener('click', () => {
        setFocus(r.id);
        onSelect(r.id);
      });
      els.set(r.id, b);
      return b;
    });
    swap(canvas, ...nodes);
  }
  function setFocus(id) {
    focus = id;
    draw();
  }
  draw();
  return {
    el,
    setSelected(id) {
      sel = id;
      for (const [rid, b] of els) {
        b.classList.toggle('sel', rid === id);
        b.setAttribute('aria-pressed', String(rid === id));
      }
    },
    update(next) {
      m = next;
      draw();
    },
  };
}
