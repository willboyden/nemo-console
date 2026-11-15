// The sandbox lifecycle swim lanes (addendum 9.16): a lane per sandbox, coloured phase segments from create to delete. A phase is told apart by its fill AND its edge and its name
// (Provisioning dashed, Ready solid, Error hatched, Deleting dotted, Other grey) and a segment wide enough prints the phase name; an open segment (the sandbox still exists) ends in an
// arrowhead, a deleted one in a cross. Each lane is one element with an aria-label that sums it up (created, ready after, deleted or still present); a click or Enter on a lane selects it
// (the decision timeline below follows). Geometry: viz/lifecycle.js (pure, unit-tested).
import { svg, titled } from '../viz/svg.js';
import { lifecycleLayout } from '../viz/lifecycle.js';
import { fmtStamp, fmtDur } from '../viz/fmt.js';
import { t } from '../i18n.js';

let uid = 0;
const clipText = (s, n) => (String(s).length > n ? `${String(s).slice(0, n - 1)}…` : String(s));
const css = (bucket) => String(bucket).toLowerCase();

/** The sentence a lane's aria-label and tooltip carry. */
export function laneSummary(ln) {
  const bits = [t('osv.lane_name', { name: ln.name })];
  bits.push(ln.createdTs != null ? t('osv.lane_created', { when: `${fmtStamp(ln.createdTs)} UTC` }) : t('osv.lane_created_unknown'));
  if (ln.provisionS != null) bits.push(t('osv.lane_ready_after', { after: fmtDur(ln.provisionS * 1000) }));
  bits.push(ln.deletedTs != null ? t('osv.lane_deleted', { when: `${fmtStamp(ln.deletedTs)} UTC` }) : ln.present ? t('osv.lane_present') : t('osv.lane_gone'));
  return bits.join(' · ');
}

/**
 * @param opts.lanes     openshell-model.lifecycleLanes(...).lanes;  @param opts.from,to the window;  @param opts.selected a lane name;  @param opts.onPick(name)
 * Returns an <svg>, or null when there is no lane.
 */
export function lifecycleChart({ lanes, from, to, width = 640, selected = null, onPick, ariaLabel }) {
  if (!lanes.length) return null;
  const longest = Math.max(...lanes.map((l) => l.name.length));
  const labelW = Math.min(width < 480 ? 96 : 170, Math.max(84, longest * 6.2 + 14));
  const g = lifecycleLayout({ lanes, from, to, width, labelW });
  const pid = `lc-hatch-${++uid}`;
  const nodes = [
    svg(
      'defs',
      svg(
        'pattern',
        { id: pid, width: 6, height: 6, patternUnits: 'userSpaceOnUse', patternTransform: 'rotate(45)' },
        svg('rect', { class: 'lc-hatch-bg', width: 6, height: 6 }),
        svg('line', { class: 'lc-hatch-line', x1: 0, y1: 0, x2: 0, y2: 6 }),
      ),
    ),
  ];
  for (const tk of g.ticks)
    nodes.push(svg('line', { class: 'gl v', x1: tk.x, x2: tk.x, y1: g.laneY(0), y2: g.height - 20 }), svg('text', { class: 'tick', x: tk.x, y: g.height - 6, 'text-anchor': 'middle' }, tk.label));
  const segsOf = new Map(),
    marksOf = new Map();
  for (const s of g.segs) (segsOf.get(s.lane) ?? segsOf.set(s.lane, []).get(s.lane)).push(s);
  for (const m of g.marks) (marksOf.get(m.lane) ?? marksOf.set(m.lane, []).get(m.lane)).push(m);
  lanes.forEach((ln, i) => {
    const y = g.laneY(i);
    const sel = selected === ln.name;
    const summary = laneSummary(ln);
    const aria = onPick ? `${summary} · ${ln.present ? t('osv.lane_pick') : t('osv.lane_pick_gone')}` : summary;
    const grp = svg('g', {
      class: `lc-lane${sel ? ' selected' : ''}${onPick ? ' lc-pick' : ''}`,
      role: onPick ? 'button' : 'img',
      'aria-label': aria,
      ...(onPick ? { tabindex: 0, 'aria-pressed': String(sel) } : {}),
    });
    titled(grp, aria);
    grp.append(
      svg('rect', { class: 'tl-lane lc-bg', x: g.x0, y: y + 2, width: g.x1 - g.x0, height: g.laneH - 4, rx: 3 }),
      svg('text', { class: `tick lc-name${ln.present ? '' : ' gone'}`, x: g.x0 - 8, y: y + g.laneH / 2 + 4, 'text-anchor': 'end' }, clipText(ln.name, Math.floor((labelW - 12) / 6.2))),
    );
    for (const s of segsOf.get(i) || []) {
      const b = s.seg.bucket;
      const tip = `${t(`osv.phase.${css(b)}`)}${s.seg.startUnknown ? ` (${t('osv.start_unknown')})` : ''} · ${fmtStamp(s.seg.from)}–${fmtStamp(s.seg.to).slice(6)} UTC${s.seg.open ? ` · ${t('osv.still_open')}` : ''}`;
      grp.append(
        titled(
          svg('rect', {
            class: `lc-seg lc-${css(b)}${s.seg.open ? ' open' : ''}${s.seg.startUnknown || s.seg.clippedStart ? ' cut' : ''}`,
            x: s.x,
            y: y + 6,
            width: s.w,
            height: g.laneH - 12,
            rx: 2,
            fill: b === 'Error' ? `url(#${pid})` : null,
          }),
          tip,
        ),
      );
      if (s.w > 64) grp.append(svg('text', { class: 'lc-seg-text', x: s.x + 6, y: y + g.laneH / 2 + 4 }, t(`osv.phase.${css(b)}`)));
      if (s.seg.open && !s.seg.clippedEnd)
        grp.append(svg('path', { class: `lc-arrow lc-${css(b)}`, d: `M${s.x + s.w} ${y + 8}L${s.x + s.w + 7} ${y + g.laneH / 2}L${s.x + s.w} ${y + g.laneH - 8}Z` }));
    }
    for (const m of marksOf.get(i) || []) {
      if (m.mark.action === 'deleted') grp.append(svg('text', { class: 'lc-cross', x: m.x, y: y + g.laneH / 2 + 4, 'text-anchor': 'middle' }, '✕'));
    }
    if (onPick) {
      grp.addEventListener('click', () => onPick(ln.name));
      grp.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onPick(ln.name);
        }
      });
    }
    nodes.push(grp);
  });
  return svg('svg', { class: 'chart rtchart lcchart', viewBox: `0 0 ${width} ${g.height}`, role: 'group', 'aria-label': ariaLabel }, svg('title', ariaLabel), ...nodes);
}
