// Waterfall: one row per span in tree order (indent = depth, collapsible), bars on a shared time axis. Rows are
// virtualised (only the rows in view are in the DOM, fixed row height), so a 5000-span trace stays responsive. Bars are
// positioned with CSSOM percentages, so a resize needs no re-layout. Zoom by dragging on the axis or with the buttons.
import { h } from '../dom.js';
import { svg } from '../viz/svg.js';
import { t } from '../i18n.js';
import { btn } from '../components/ui.js';
import { visibleRows, windowRows, scrollToRow, barGeometry, parentIds, relMs } from '../viz/tree.js';
import { offsetTicks, clamp, fmtOffsetMs } from '../viz/scale.js';
import { categoryOf, fmtDur, isRoutingMark } from '../viz/fmt.js';
import { legendList, swap } from './ui.js';

export const ROW_H = 28;
const MIN_RANGE_MS = 2;
const pctStr = (f) => `${(f * 100).toFixed(3)}%`;

export function spanLabel(span) {
  return t('rt.wf.row_label', {
    name: span.name ?? '–',
    category: t(`rt.cat.${categoryOf(span)}`),
    duration: span.status === 'open' ? t('rt.status.open') : fmtDur(span.duration_ms),
    status: t(`rt.status.${span.status}`),
  });
}

export function createWaterfall({ model, onSelect, selected, scope }) {
  let m = model,
    sel = selected ?? null,
    view = { from: 0, to: m.tl.durMs },
    full = true,
    collapsed = new Set(),
    rows = visibleRows(m.tree, collapsed),
    rowEls = new Map(),
    lastWin = '';
  const dur = () => m.tl.durMs;

  // ---- toolbar
  const zoomLabel = h('span', { class: 'muted small num wf-zoom-label', role: 'status' });
  const bZoomIn = btn('', { icon: 'plus', size: 'sm', title: t('rt.wf.zoom_in'), onClick: () => zoomBy(0.5) });
  const bZoomOut = btn('', { icon: 'restart', size: 'sm', title: t('rt.wf.zoom_out'), onClick: () => zoomBy(2) });
  swap(bZoomOut, h('span', { 'aria-hidden': 'true', class: 'minus' }, '−'), h('span', { class: 'sr-only' }, t('rt.wf.zoom_out')));
  const bReset = btn(t('rt.wf.reset_zoom'), { size: 'sm', onClick: () => setView(0, dur()) });
  const bSpan = btn(t('rt.wf.zoom_span'), { size: 'sm', title: t('rt.wf.zoom_span_tip'), onClick: () => sel && zoomToSpan(sel) });
  const bCollapse = btn(t('rt.wf.collapse_all'), { size: 'sm', onClick: () => collapseAll(true) });
  const bExpand = btn(t('rt.wf.expand_all'), { size: 'sm', onClick: () => collapseAll(false) });
  const toolbar = h('div', { class: 'row gap wrap wf-toolbar' }, h('div', { class: 'row gap' }, bZoomOut, bZoomIn, bReset, bSpan), h('div', { class: 'row gap' }, bCollapse, bExpand), zoomLabel);
  bZoomIn.setAttribute('aria-label', t('rt.wf.zoom_in'));
  bZoomOut.setAttribute('aria-label', t('rt.wf.zoom_out'));

  // ---- axis (drag to zoom)
  const axisTicks = h('div', { class: 'wf-ticks' });
  const dragBox = h('div', { class: 'wf-drag', hidden: true });
  const axis = h('div', { class: 'wf-axis', title: t('rt.wf.axis_tip'), role: 'presentation' }, axisTicks, dragBox);
  const head = h('div', { class: 'wf-head' }, h('div', { class: 'wf-name wf-name-head' }, t('rt.wf.col_span'), h('span', { class: 'muted small' }, t('rt.wf.col_dur'))), axis);

  // ---- scrolling body
  const padTop = h('div', { class: 'wf-pad' }),
    padBottom = h('div', { class: 'wf-pad' }),
    rowsHost = h('div', { class: 'wf-rows' });
  const grid = svg('svg', { class: 'wf-grid', 'aria-hidden': 'true' });
  const content = h('div', { class: 'wf-content' }, grid, padTop, rowsHost, padBottom);
  const scroller = h('div', { class: 'wf-scroll', role: 'tree', tabindex: '0', 'aria-label': t('rt.wf.tree_label') }, content);
  const empty = h('p', { class: 'hint', hidden: true }, t('rt.wf.no_spans'));
  const el = h(
    'div',
    { class: 'wf' },
    toolbar,
    h('div', { class: 'wf-frame' }, head, scroller, empty),
    legendList([
      { cls: 'cat-agent', label: t('rt.cat.agent') },
      { cls: 'cat-turn', label: t('rt.cat.turn') },
      { cls: 'cat-llm', label: t('rt.cat.llm') },
      { cls: 'cat-tool', label: t('rt.cat.tool') },
      { cls: 'cat-other', label: t('rt.cat.other') },
      { cls: 'lg-err', label: t('rt.wf.lg_error') },
      { cls: 'lg-open', label: t('rt.wf.lg_open') },
      { cls: 'lg-mark', label: t('rt.wf.lg_mark') },
      { cls: 'lg-route', label: t('rt.wf.lg_routing') },
      { cls: 'lg-ttfc', label: t('rt.wf.lg_ttfc') },
      { cls: 'lg-chunk', label: t('rt.wf.lg_chunks') },
    ]),
  );

  // ---- view (zoom)
  function setView(from, to) {
    const range = clamp(to - from, Math.min(MIN_RANGE_MS, dur()), dur());
    from = clamp(from, 0, dur() - range);
    view = { from, to: from + range };
    full = view.from <= 0 && view.to >= dur() - 1e-6;
    lastWin = '';
    drawAxis();
    drawRows(true);
  }
  function zoomBy(factor) {
    const sp = sel && m.tree.nodes.get(sel)?.span;
    const s = sp && relMs(sp.start_ts ?? sp.end_ts, m.tl);
    const range = view.to - view.from;
    const center = s != null && s >= view.from && s <= view.to ? s + (sp.duration_ms ?? 0) / 2 : (view.from + view.to) / 2;
    const next = clamp(range * factor, Math.min(MIN_RANGE_MS, dur()), dur());
    setView(center - next / 2, center + next / 2);
  }
  function zoomToSpan(id) {
    const n = m.tree.nodes.get(id);
    if (!n) return;
    const s = relMs(n.span.start_ts ?? n.span.end_ts, m.tl);
    if (s == null) return;
    const d = Math.max(n.span.duration_ms ?? 0, MIN_RANGE_MS);
    setView(s - d * 0.05, s + d * 1.05);
  }
  function drawAxis() {
    const range = view.to - view.from;
    const ticks = offsetTicks(view.from, view.to, 7);
    swap(
      axisTicks,
      ...ticks.map((tk) => {
        const f = (tk.v - view.from) / range;
        const s = h('span', { class: `wf-tick${f > 0.94 ? ' last' : ''}` }, tk.label);
        s.style.left = pctStr(f);
        return s;
      }),
    );
    swap(grid, ...ticks.map((tk) => svg('line', { class: 'wf-gl', x1: pctStr((tk.v - view.from) / range), x2: pctStr((tk.v - view.from) / range), y1: 0, y2: '100%' })));
    zoomLabel.textContent = full ? t('rt.wf.zoom_full', { total: fmtOffsetMs(dur()) }) : t('rt.wf.zoom_range', { from: fmtOffsetMs(view.from), to: fmtOffsetMs(view.to), total: fmtOffsetMs(dur()) });
    bReset.disabled = full;
    bSpan.disabled = !sel;
  }

  // ---- rows
  function trackFor(node) {
    const s = node.span;
    const range = view.to - view.from;
    const f = (ms) => (ms - view.from) / range;
    const track = h('div', { class: 'wf-track' });
    const g = barGeometry(s, m.tl, view);
    const cat = categoryOf(s);
    if (!g.hidden) {
      const bar = h('div', {
        class: `wf-bar cat-${cat}${s.status === 'error' ? ' err' : ''}${g.open ? ' open' : ''}${g.clipL ? ' clip-l' : ''}${g.clipR && !g.open ? ' clip-r' : ''}${g.noStart ? ' nostart' : ''}`,
        role: 'img',
        'aria-label': spanLabel(s),
        title: `${spanLabel(s)}${g.noStart ? ` (${t('rt.wf.no_start')})` : ''}`,
      });
      bar.style.left = pctStr(g.l);
      bar.style.width = pctStr(g.w);
      track.append(bar);
      if (g.open && g.tailW > 0) {
        const tail = h('div', { class: 'wf-tail', title: t('rt.wf.open_tail') });
        tail.style.left = pctStr(g.tailL);
        tail.style.width = pctStr(g.tailW);
        track.append(tail);
      }
    }
    const s0 = relMs(s.start_ts, m.tl);
    const llm = s.llm;
    if (llm && s0 != null) {
      for (const off of llm.chunk_offsets_ms || []) {
        const x = f(s0 + off);
        if (x < 0 || x > 1) continue;
        const i = h('i', { class: 'wf-chunk' });
        i.style.left = pctStr(x);
        track.append(i);
      }
      if (llm.ttfc_ms != null) {
        const x = f(s0 + llm.ttfc_ms);
        if (x >= 0 && x <= 1) {
          const i = h('i', { class: 'wf-ttfc', title: t('rt.wf.ttfc_tip', { ms: fmtDur(llm.ttfc_ms) }) });
          i.style.left = pctStr(x);
          track.append(i);
        }
      }
    }
    const marks = m.marksMap.get(node.id) || [];
    let shown = 0;
    for (const mk of marks) {
      const x = f(relMs(mk.ts, m.tl));
      if (!(x >= 0 && x <= 1) || shown >= 24) continue;
      shown++;
      const i = h('i', {
        class: `wf-mark${isRoutingMark(mk) ? ' routing' : ''}${mk.severity === 'warn' ? ' warn' : mk.severity === 'error' ? ' err' : ''}`,
        title: t('rt.wf.mark_tip', { name: mk.name, at: fmtOffsetMs(relMs(mk.ts, m.tl)) }),
      });
      i.style.left = pctStr(x);
      track.append(i);
    }
    return track;
  }
  function rowEl(node) {
    const s = node.span;
    const hasKids = node.children.length > 0;
    const open = !collapsed.has(node.id);
    const name = h('div', { class: 'wf-name' });
    name.style.paddingLeft = `${node.depth * 14 + 4}px`;
    if (hasKids) {
      const c = h('button', { type: 'button', class: 'wf-caret', tabindex: '-1', 'aria-label': open ? t('rt.wf.collapse') : t('rt.wf.expand') }, open ? '▾' : '▸');
      c.addEventListener('click', (e) => {
        e.stopPropagation();
        toggle(node.id);
      });
      name.append(c);
    } else name.append(h('span', { class: 'wf-caret ph', 'aria-hidden': 'true' }));
    name.append(
      h('span', { class: `wf-cat cat-${categoryOf(s)}`, 'aria-hidden': 'true', title: t(`rt.cat.${categoryOf(s)}`) }, { agent: 'A', turn: 'T', llm: 'L', tool: 'F', other: '·' }[categoryOf(s)]),
      h('span', { class: 'wf-title', title: s.name }, s.status === 'error' ? h('span', { class: 'bad-text', 'aria-hidden': 'true' }, '✕ ') : null, s.name ?? '–'),
      h('span', { class: 'wf-dur num' }, s.status === 'open' ? t('rt.status.open') : fmtDur(s.duration_ms)),
    );
    const row = h(
      'div',
      {
        class: `wf-row${sel === node.id ? ' sel' : ''}${s.status === 'error' ? ' err' : ''}`,
        id: `wf-${s.id}`,
        role: 'treeitem',
        'aria-level': node.depth + 1,
        'aria-selected': String(sel === node.id),
        'aria-expanded': hasKids ? String(open) : null,
        'aria-label': spanLabel(s),
      },
      name,
      trackFor(node),
    );
    row.addEventListener('click', () => onSelect(node.id));
    row.addEventListener('dblclick', () => zoomToSpan(node.id));
    return row;
  }
  function drawRows(force = false) {
    const vh = scroller.clientHeight || 560;
    const w = windowRows(rows.length, scroller.scrollTop, vh, ROW_H, 6);
    const key = `${w.start}:${w.end}:${rows.length}`;
    if (!force && key === lastWin) return;
    lastWin = key;
    padTop.style.height = `${w.padTop}px`;
    padBottom.style.height = `${w.padBottom}px`;
    rowEls = new Map();
    const els = rows.slice(w.start, w.end).map((n) => {
      const r = rowEl(n);
      rowEls.set(n.id, r);
      return r;
    });
    swap(rowsHost, ...els);
    empty.hidden = rows.length > 0;
    scroller.hidden = rows.length === 0;
    head.hidden = rows.length === 0;
    if (sel && rowEls.has(sel)) scroller.setAttribute('aria-activedescendant', `wf-${sel}`);
    else scroller.removeAttribute('aria-activedescendant');
  }
  let raf = 0;
  scroller.addEventListener('scroll', () => {
    if (raf) return;
    raf = requestAnimationFrame(() => {
      raf = 0;
      drawRows();
    });
  });
  scope?.listen(window, 'resize', () => drawRows(true));
  scope?.own(() => cancelAnimationFrame(raf));

  function recompute() {
    rows = visibleRows(m.tree, collapsed);
    lastWin = '';
    drawRows(true);
  }
  function toggle(id) {
    if (collapsed.has(id)) collapsed.delete(id);
    else collapsed.add(id);
    recompute();
  }
  function collapseAll(on) {
    collapsed = on ? new Set(parentIds(m.tree).filter((id) => m.tree.nodes.get(id)?.parent)) : new Set();
    recompute();
  }
  function setSelected(id, { reveal = false } = {}) {
    sel = id;
    bSpan.disabled = !sel;
    if (reveal && id && m.tree.nodes.has(id)) {
      for (let p = m.tree.nodes.get(id).parent; p; p = p.parent) collapsed.delete(p.id);
      rows = visibleRows(m.tree, collapsed);
      const i = rows.findIndex((n) => n.id === id);
      const top = scrollToRow(i, scroller.scrollTop, scroller.clientHeight || 560, ROW_H);
      if (top != null) scroller.scrollTop = top;
      lastWin = '';
      drawRows(true);
      return;
    }
    for (const [rid, r] of rowEls) {
      const on = rid === id;
      r.classList.toggle('sel', on);
      r.setAttribute('aria-selected', String(on));
    }
    if (id && rowEls.has(id)) scroller.setAttribute('aria-activedescendant', `wf-${id}`);
  }
  scroller.addEventListener('keydown', (e) => {
    const i = rows.findIndex((n) => n.id === sel);
    const go = (j) => {
      const n = rows[clamp(j, 0, rows.length - 1)];
      if (n) {
        onSelect(n.id, { reveal: true });
      }
    };
    const node = i >= 0 ? rows[i] : null;
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        return go(i < 0 ? 0 : i + 1);
      case 'ArrowUp':
        e.preventDefault();
        return go(i < 0 ? 0 : i - 1);
      case 'PageDown':
        e.preventDefault();
        return go(i + 10);
      case 'PageUp':
        e.preventDefault();
        return go(i - 10);
      case 'Home':
        e.preventDefault();
        return go(0);
      case 'End':
        e.preventDefault();
        return go(rows.length - 1);
      case 'ArrowRight':
        if (!node) return;
        e.preventDefault();
        if (node.children.length && collapsed.has(node.id)) toggle(node.id);
        else if (node.children.length) go(i + 1);
        return;
      case 'ArrowLeft':
        if (!node) return;
        e.preventDefault();
        if (node.children.length && !collapsed.has(node.id)) toggle(node.id);
        else if (node.parent) onSelect(node.parent.id, { reveal: true });
        return;
      case '+':
      case '=':
        e.preventDefault();
        return zoomBy(0.5);
      case '-':
        e.preventDefault();
        return zoomBy(2);
      case '0':
        e.preventDefault();
        return setView(0, dur());
      default:
    }
  });

  // drag on the axis = zoom to that range
  let drag = null;
  axis.addEventListener('pointerdown', (e) => {
    if (e.button !== 0) return;
    const r = axis.getBoundingClientRect();
    drag = { x0: clamp((e.clientX - r.left) / r.width, 0, 1), x1: 0, r };
    drag.x1 = drag.x0;
    axis.setPointerCapture?.(e.pointerId);
    e.preventDefault();
  });
  axis.addEventListener('pointermove', (e) => {
    if (!drag) return;
    drag.x1 = clamp((e.clientX - drag.r.left) / drag.r.width, 0, 1);
    const a = Math.min(drag.x0, drag.x1),
      b = Math.max(drag.x0, drag.x1);
    dragBox.hidden = b - a < 0.003;
    dragBox.style.left = pctStr(a);
    dragBox.style.width = pctStr(b - a);
  });
  const endDrag = (e) => {
    if (!drag) return;
    const a = Math.min(drag.x0, drag.x1),
      b = Math.max(drag.x0, drag.x1);
    const range = view.to - view.from;
    const from = view.from + a * range,
      to = view.from + b * range;
    drag = null;
    dragBox.hidden = true;
    axis.releasePointerCapture?.(e.pointerId);
    if (b - a > 0.01) setView(from, to);
  };
  axis.addEventListener('pointerup', endDrag);
  axis.addEventListener('pointercancel', () => {
    drag = null;
    dragBox.hidden = true;
  });

  drawAxis();
  drawRows(true);
  requestAnimationFrame(() => drawRows(true));

  return {
    el,
    setSelected,
    zoomToSpan,
    /** New data (the trace was still open): keep zoom, collapse and selection; follow the end while fully zoomed out. */
    update(next) {
      const wasFull = full;
      m = next;
      if (wasFull) view = { from: 0, to: dur() };
      else view = { from: clamp(view.from, 0, dur()), to: clamp(view.to, 0, dur()) };
      collapsed = new Set([...collapsed].filter((id) => m.tree.nodes.has(id)));
      rows = visibleRows(m.tree, collapsed);
      lastWin = '';
      drawAxis();
      drawRows(true);
    },
  };
}
