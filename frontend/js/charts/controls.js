// Controls any chart card can use (moved out of js/rt/controls.js, which re-exports it).
import { h } from '../dom.js';
import { swap } from './parts.js';

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
