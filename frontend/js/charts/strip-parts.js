// The words around an up / down strip (addendum 9.16): the one-sentence summary (uptime, the down periods) and the legend whose swatches have the height of the cells they key.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtDur } from '../viz/fmt.js';
import { STRIP_KEYS } from './strip-chart.js';

/** `Guardrails: up 99.2% of the time it was sampled in the last hour. One down period (longest 6m00s).` `noun` names the thing that is up. */
export function stripSummary(model, windowText, noun) {
  if (!model || model.empty) return t('strip.none', { window: windowText });
  const pct = model.uptime >= 0.9995 || model.uptime <= 0.0005 ? `${Math.round(model.uptime * 100)}%` : `${(model.uptime * 100).toFixed(1)}%`;
  const runs = model.downRuns.length;
  const longest = model.longestDown ? fmtDur((model.longestDown.to - model.longestDown.from) * 1000) : '–';
  // a dip shorter than a cell makes the cell partial, not down: say so instead of claiming there was no trouble
  const downs =
    runs === 0
      ? model.partCells
        ? t('strip.dips', { count: model.partCells })
        : t('strip.downs_none')
      : runs === 1
        ? t('strip.downs_single', { longest })
        : t('strip.downs', { count: runs, longest });
  return t('strip.summary', { noun, pct, window: windowText, downs });
}

/** The legend: up, partial, down, no data. */
export function stripLegend() {
  return h(
    'ul',
    { class: 'legend strip-legend' },
    STRIP_KEYS.map((k) => h('li', h('i', { class: `sw st-sw st-${k}`, 'aria-hidden': 'true' }), h('span', t(`strip.state_${k}`)))),
  );
}
