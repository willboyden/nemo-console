// Value heatmaps (addendum 9.14): a colour ramp of HEAT_LEVELS steps over a value domain, and the geometry of a rows x columns
// matrix (cells in a row, a gap before the first row of each host). Pure functions, no DOM; the SVG is built in
// js/charts/heatmap.js. Unit-tested in tests/viz-heat.test.mjs.
const isNum = (v) => typeof v === 'number' && Number.isFinite(v);

export const HEAT_LEVELS = 9;

/**
 * The ramp step 0..HEAT_LEVELS-1 of a value over {min, max}: the domain is cut into equal parts (curve 'sqrt' squeezes the low
 * end apart, for counts and rates), a value outside is clamped to the end steps. A value that is not a number is null (no data),
 * never step 0.
 */
export function levelOf(v, { min = 0, max = 1, curve = 'linear' } = {}, levels = HEAT_LEVELS) {
  if (!isNum(v)) return null;
  if (!(max > min)) return v > min ? levels - 1 : 0;
  let t = (v - min) / (max - min);
  t = Math.min(1, Math.max(0, t));
  if (curve === 'sqrt') t = Math.sqrt(t);
  return Math.min(levels - 1, Math.floor(t * levels));
}

/** The domain value where ramp step i starts (the inverse of levelOf, for the legend). */
export function levelStart(i, { min = 0, max = 1, curve = 'linear' } = {}, levels = HEAT_LEVELS) {
  let t = i / levels;
  if (curve === 'sqrt') t = t * t;
  return min + t * (max - min);
}

/** Runs of equal steps along a row: [{c0, c1 (exclusive), level|null}]; neighbouring cells with the same step (or both no data) become one run. */
export function runLengths(levels) {
  const out = [];
  levels.forEach((lv, c) => {
    const l = lv == null ? null : lv;
    const last = out[out.length - 1];
    if (last && last.level === l && last.c1 === c) last.c1 = c + 1;
    else out.push({ c0: c, c1: c + 1, level: l });
  });
  return out;
}

/**
 * Geometry of a matrix: `rows` rows of `cols` cells, labels on the left (`labelW` px), `rowGap[r]` extra px above row r (a gap
 * between hosts). Returns {width, height, x0, x1, cw, cellH, rowY[], plotTop, plotBottom, colX(c), colAt(px), rowAt(py)} where colAt /
 * rowAt map a pixel back to a cell (-1 outside).
 */
export function matrixLayout({ rows, cols, width, labelW = 110, cellH = 18, rowGap = [], top = 4, bottom = 24, right = 8 }) {
  const x0 = labelW,
    x1 = Math.max(x0 + 1, width - right),
    cw = cols > 0 ? (x1 - x0) / cols : 0;
  const rowY = [];
  let y = top;
  for (let r = 0; r < rows; r++) {
    y += rowGap[r] || 0;
    rowY.push(y);
    y += cellH;
  }
  return {
    width,
    height: y + bottom,
    x0,
    x1,
    cw,
    cellH,
    rowY,
    plotTop: top,
    plotBottom: y,
    colX: (c) => x0 + c * cw,
    colAt: (px) => (cw > 0 && px >= x0 && px < x1 ? Math.min(cols - 1, Math.floor((px - x0) / cw)) : -1),
    rowAt: (py) => rowY.findIndex((ry) => py >= ry && py < ry + cellH),
  };
}
