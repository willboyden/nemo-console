// A matrix of labelled cells (addendum 9.15): the validator x run heatmap of status and finding severity, and the model x benchmark matrix of the latest
// score. Each cell is a rect AND text: a score prints its number, a validator prints its glyph and the letter of its worst severity, so no cell is colour-only;
// a cell with nothing to show (the run did not run that validator, no run for that pair) is hatched and says so in its aria-label. Every cell has an aria-label
// and a <title>. Columns can be picked (a click or Enter on a column header or a cell calls onPick(col id)). Geometry is plain arithmetic (labels, a fixed cell
// height, a cell width from the available space). Class names (cm-*, heatc fhN) are styled in components.css.
import { svg, titled } from '../viz/svg.js';

let uid = 0;
const clipText = (s, n) => (String(s).length > n ? `${String(s).slice(0, n - 1)}…` : String(s));

/**
 * @param cols  [{id, label (short header), title (the full name: tooltip and aria-label)}]
 * @param rows  [{id, label}]
 * @param cell  (rowIndex, colIndex) => null | {cls (heatc fhN | cm-ok | cm-warn | cm-bad | cm-muted), tone? (ok | warn | bad | muted: colours the glyph), glyph?, text?, label (aria-label and tooltip), level? (the ramp step 0..8 of a heatc cell: the text colour follows it)}
 * @param opts  width, ariaLabel, onPick(colId) (a column header), onCell(rowIndex, colIndex) (a cell that has data), selected {a: colId, b: colId}, noDataLabel
 * Returns an <svg>, or null for no rows or no columns.
 */
export function cellMatrix({ cols, rows, cell, width = 560, ariaLabel, onPick, onCell, selected = {}, noDataLabel, cellH = 24, minCellW = 34 }) {
  if (!rows.length || !cols.length) return null;
  const longest = Math.max(...rows.map((r) => String(r.label).length), 4);
  const labelW = Math.min(width < 420 ? 108 : 190, Math.max(70, longest * 6.4 + 14));
  const widest = Math.max(...cols.map((c) => String(c.label).length));
  const fit = (right) => Math.min(64, Math.max(minCellW, Math.floor(Math.max(60, width - labelW - right) / cols.length)));
  let right = 6,
    cw = fit(right);
  const turned = widest * 6.2 > cw - 4; // a header that does not fit sideways is set at an angle
  if (turned) {
    right = 46; // ... and the last one leans out to the right: leave room for it
    cw = fit(right);
  }
  const w = labelW + cw * cols.length + right;
  const headH = turned ? Math.min(78, widest * 4.6 + 14) : 20;
  const top = headH + 4;
  const height = top + rows.length * cellH + 6;
  const pid = `cm-none-${++uid}`;
  const nodes = [
    svg(
      'defs',
      svg(
        'pattern',
        { id: pid, width: 6, height: 6, patternUnits: 'userSpaceOnUse', patternTransform: 'rotate(45)' },
        svg('rect', { class: 'heat-none-bg', width: 6, height: 6 }),
        svg('line', { class: 'heat-none-line', x1: 0, y1: 0, x2: 0, y2: 6 }),
      ),
    ),
  ];
  cols.forEach((c, ci) => {
    const x = labelW + ci * cw + cw / 2;
    const tag = selected.a === c.id ? 'A' : selected.b === c.id ? 'B' : null;
    const head = svg(
      'text',
      {
        class: `tick cm-head${tag ? ` is-${tag.toLowerCase()}` : ''}${onPick ? ' cm-pick' : ''}`,
        x: turned ? x - 2 : x,
        y: turned ? top - 6 : headH - 5,
        'text-anchor': turned ? 'start' : 'middle',
        transform: turned ? `rotate(-40 ${x - 2} ${top - 6})` : null,
        ...(onPick ? { role: 'button', tabindex: 0, 'aria-label': c.title ?? c.label } : {}),
      },
      `${tag ? `${tag} ` : ''}${clipText(c.label, turned ? 18 : 10)}`,
    );
    titled(head, c.title ?? c.label);
    if (onPick) {
      head.addEventListener('click', () => onPick(c.id));
      head.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onPick(c.id);
        }
      });
    }
    nodes.push(head);
  });
  rows.forEach((r, ri) => {
    const y = top + ri * cellH;
    nodes.push(titled(svg('text', { class: 'tick cm-row', x: labelW - 8, y: y + cellH / 2 + 4, 'text-anchor': 'end' }, clipText(r.label, Math.floor((labelW - 12) / 6.2))), r.label));
    cols.forEach((c, ci) => {
      const d = cell(ri, ci);
      const x = labelW + ci * cw;
      const label = d?.label ?? `${r.label} · ${c.title ?? c.label}: ${noDataLabel}`;
      const pickable = !!(onCell && d);
      const g = svg('g', { class: `cm-cell${d ? '' : ' cm-none'}${pickable ? ' cm-pick' : ''}`, role: pickable ? 'button' : 'img', 'aria-label': label, ...(pickable ? { tabindex: 0 } : {}) });
      titled(g, label);
      g.append(svg('rect', { class: d ? `cm-rect ${d.cls}` : 'cm-rect heat-none', fill: d ? null : `url(#${pid})`, x: x + 1, y: y + 1, width: cw - 2, height: cellH - 2, rx: 3 }));
      if (d && (d.glyph || d.text))
        g.append(
          svg(
            'text',
            { class: `cm-text${d.tone ? ` cmt-${d.tone}` : ''}${d.level != null ? ` lv${d.level}` : ''}`, x: x + cw / 2, y: y + cellH / 2 + 4, 'text-anchor': 'middle' },
            [d.glyph, d.text].filter(Boolean).join(' '),
          ),
        );
      if (pickable) {
        g.addEventListener('click', () => onCell(ri, ci));
        g.addEventListener('keydown', (e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            onCell(ri, ci);
          }
        });
      }
      nodes.push(g);
    });
  });
  const root = svg('svg', { class: 'chart rtchart cmchart', viewBox: `0 0 ${w} ${height}`, role: 'group', 'aria-label': ariaLabel }, svg('title', ariaLabel), ...nodes);
  root.style.maxWidth = `${w}px`; // a matrix of few columns keeps its cell size instead of stretching to the card (CSSOM: no style attribute)
  return root;
}
