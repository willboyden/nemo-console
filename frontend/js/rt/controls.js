// Controls shared by the Traces, Sessions and Analytics tabs: the window picker, the anchor toggle and filter pieces.
import { swap } from './ui.js';
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { select, btn } from '../components/ui.js';
import { debounce } from '../format.js';
import { WINDOWS, ANCHORS } from './nav.js';
import { normalizeFacet } from './pure.js';

export { normalizeFacet };

/** Segmented picker over `values`; the pressed one is aria-pressed. `labelOf(v)` names a value. */
export function segPicker({ values, current, labelOf, label, onPick, titleOf }) {
  const wrap = h('div', { class: 'seg-control', role: 'group', 'aria-label': label });
  const draw = () => {
    swap(
      wrap,
      ...values.map((v) => {
        const b = h('button', { type: 'button', 'aria-pressed': String(current() === v), title: titleOf?.(v) }, labelOf(v));
        b.addEventListener('click', () => onPick(v));
        return b;
      }),
    );
  };
  draw();
  wrap.redraw = draw;
  return wrap;
}

/** Window (1h .. all) and anchor (latest event / now) controls bound to the tab's nav; onChange re-queries. */
export function rangeControls(nav, onChange) {
  const win = segPicker({
    values: WINDOWS,
    current: () => nav.get('window'),
    labelOf: (v) => t(`rt.window.${v}`),
    label: t('rt.window.label'),
    onPick: (v) => {
      nav.set({ window: v });
      win.redraw();
      onChange();
    },
  });
  const anchor = segPicker({
    values: ANCHORS,
    current: () => nav.get('anchor'),
    labelOf: (v) => t(`rt.anchor.${v}`),
    titleOf: (v) => t(`rt.anchor.${v}_tip`),
    label: t('rt.anchor.label'),
    onPick: (v) => {
      nav.set({ anchor: v });
      anchor.redraw();
      onChange();
    },
  });
  return h('div', { class: 'row gap wrap range-controls' }, win, anchor);
}

/** A labelled <select> of facet values ({value,count} items, plain strings or a {value: count} map); keeps its value while options refresh. */
export function facetSelect({ name, label, nav, onChange }) {
  const sel = select([{ value: '', label: t('rt.filter.all') }], nav.get(name), (v) => {
    nav.set({ [name]: v, offset: null });
    onChange();
  });
  sel.id = `flt-${name}`;
  const wrap = h('div', { class: 'field filter' }, h('label', { for: sel.id }, label), sel);
  wrap.update = (facet) => {
    const items = normalizeFacet(facet);
    const cur = nav.get(name);
    // A value of null is "not set" (counted, but a query cannot ask for it): listed, not selectable.
    const opts = [
      { value: '', label: t('rt.filter.all') },
      ...items.map((i) => ({ value: i.value, label: `${i.value ?? t('rt.filter.unset')}${i.count != null ? ` (${i.count})` : ''}`, disabled: i.value == null })),
    ];
    if (cur && !items.some((i) => i.value === cur)) opts.push({ value: cur, label: cur });
    swap(sel, ...opts.map((o) => h('option', { value: o.value ?? '', disabled: o.disabled }, o.label)));
    sel.value = cur;
  };
  return wrap;
}

/** Debounced search box that writes `name` into the nav and calls onChange. */
export function searchBox({ name, label, nav, onChange, maxlength = 80, placeholder }) {
  const input = h('input', { type: 'search', id: `flt-${name}`, value: nav.get(name), maxlength, placeholder: placeholder ?? '', autocomplete: 'off', spellcheck: 'false' });
  const fire = debounce(() => {
    nav.set({ [name]: input.value.trim(), offset: null });
    onChange();
  }, 300);
  input.addEventListener('input', fire);
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      fire.cancel();
      nav.set({ [name]: input.value.trim(), offset: null });
      onChange();
    }
  });
  const wrap = h('div', { class: 'field filter grow' }, h('label', { for: input.id }, label), input);
  wrap.input = input;
  return wrap;
}

export function numberBox({ name, label, nav, onChange, min = 0, step = 100, placeholder }) {
  const input = h('input', { type: 'number', id: `flt-${name}`, min, step, value: nav.get(name), placeholder: placeholder ?? '', inputmode: 'numeric' });
  const fire = debounce(() => {
    const v = input.value.trim();
    nav.set({ [name]: v && Number(v) >= 0 ? String(Math.floor(Number(v))) : null, offset: null });
    onChange();
  }, 400);
  input.addEventListener('input', fire);
  return h('div', { class: 'field filter narrow' }, h('label', { for: input.id }, label), input);
}

export const resetButton = (onClick) => btn(t('rt.filter.reset'), { size: 'sm', icon: 'close', onClick });
