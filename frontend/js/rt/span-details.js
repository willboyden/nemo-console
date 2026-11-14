// The details panel of one span: every Span field laid out readably (time, LLM, tool, error blocks), the marks under
// it, and the on-demand Content section. Captured text is always a text node.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt } from '../format.js';
import { btn, badge } from '../components/ui.js';
import { svg } from '../viz/svg.js';
import { relMs } from '../viz/tree.js';
import { fmtDur, fmtRate, isRoutingMark, shortId } from '../viz/fmt.js';
import { fmtOffsetMs } from '../viz/scale.js';
import { catChip, statusChip, kvList, tokenBar, copyBtn, swap } from './ui.js';
import { contentSection, renderSpanContent } from './content.js';

const ms3 = (ts) => (ts == null ? '–' : `${new Date(ts * 1000).toISOString().slice(5, 23).replace('T', ' ')}`);

/** Chunk cadence: a strip of the (up to 64) chunk offsets with the first-chunk marker, over the span's duration. */
function cadence(llm, durMs) {
  const offs = llm.chunk_offsets_ms || [];
  if (!offs.length || !(durMs > 0)) return null;
  const W = 300;
  const x = (ms) => Math.min(W, Math.max(0, (ms / durMs) * W));
  const desc = t('rt.sd.cadence_alt', { n: fmtInt(llm.chunks ?? offs.length), ttfc: fmtDur(llm.ttfc_ms), gap: fmtDur(llm.max_chunk_gap_ms) });
  return svg(
    'svg',
    { class: 'cadence', viewBox: `0 0 ${W} 22`, role: 'img', 'aria-label': desc, preserveAspectRatio: 'none' },
    svg('title', desc),
    svg('rect', { class: 'cad-bg', x: 0, y: 8, width: W, height: 6 }),
    ...offs.map((o) => svg('line', { class: 'cad-tick', x1: x(o), x2: x(o), y1: 5, y2: 17 })),
    llm.ttfc_ms != null ? svg('path', { class: 'cad-ttfc', d: `M${x(llm.ttfc_ms) - 4} 0L${x(llm.ttfc_ms) + 4} 0L${x(llm.ttfc_ms)} 6Z` }) : null,
  );
}

function llmBlock(s) {
  const l = s.llm;
  return h(
    'section',
    { class: 'sd-block' },
    h('h4', { class: 'sub' }, t('rt.sd.llm')),
    kvList([
      [t('rt.sd.model'), h('span', h('code', l.model ?? '–'), l.served_model ? h('span', [' → ', h('code', l.served_model)]) : null)],
      [t('rt.sd.provider'), l.provider ? `${l.provider} · ${l.api ?? '–'}` : (l.api ?? null)],
      [t('rt.sd.stream'), l.stream == null ? null : l.stream ? t('common.yes') : t('common.no')],
      [t('rt.sd.finish'), l.finish_reason ? badge(l.finish_reason, l.finish_reason === 'stop' ? 'ok' : l.finish_reason === 'tool_calls' ? 'info' : 'warn') : null],
      [t('rt.sd.ttfc'), l.ttfc_ms != null ? fmtDur(l.ttfc_ms) : null],
      [t('rt.sd.tps'), l.tokens_per_s != null ? fmtRate(l.tokens_per_s) : null],
      [t('rt.sd.messages'), l.messages != null ? fmtInt(l.messages) : null],
      [t('rt.sd.tool_defs'), l.tool_defs != null ? fmtInt(l.tool_defs) : null],
      [t('rt.sd.max_tokens'), l.max_tokens != null ? fmtInt(l.max_tokens) : null],
      [t('rt.sd.effort'), l.reasoning_effort],
      [t('rt.sd.correlation'), l.correlation],
    ]),
    h('h5', { class: 'sub' }, t('rt.sd.tokens')),
    l.input_tokens != null || l.output_tokens != null
      ? h(
          'div',
          { class: 'stack-v' },
          tokenBar(l.input_tokens, l.output_tokens, l.cached_tokens),
          kvList([
            [t('rt.sd.tok_in'), fmtInt(l.input_tokens)],
            [t('rt.sd.tok_cached'), fmtInt(l.cached_tokens)],
            [t('rt.sd.tok_cache_write'), l.cache_write_tokens ? fmtInt(l.cache_write_tokens) : null],
            [t('rt.sd.tok_out'), fmtInt(l.output_tokens)],
            [t('rt.sd.tok_reasoning'), l.reasoning_tokens ? fmtInt(l.reasoning_tokens) : null],
            [t('rt.sd.tok_total'), fmtInt(l.total_tokens)],
          ]),
        )
      : h('p', { class: 'hint' }, t('rt.tokens.none')),
    l.tool_calls_requested || l.requested_tools?.length
      ? h(
          'div',
          h('h5', { class: 'sub' }, t('rt.sd.requested_tools', { n: fmtInt(l.tool_calls_requested ?? l.requested_tools.length) })),
          h(
            'span',
            { class: 'chips' },
            (l.requested_tools || []).map((n) => h('span', { class: 'chip' }, n)),
          ),
        )
      : null,
    l.chunks != null || l.chunk_offsets_ms?.length
      ? h(
          'div',
          h('h5', { class: 'sub' }, t('rt.sd.chunks')),
          cadence(l, s.duration_ms ?? null),
          h('p', { class: 'hint num' }, t('rt.sd.chunk_stats', { n: fmtInt(l.chunks), gap: fmtDur(l.max_chunk_gap_ms), shown: fmtInt(l.chunk_offsets_ms?.length ?? 0) })),
        )
      : null,
  );
}

export function createDetails({ scope, api, traceId, onClose, onZoom, model: getModel }) {
  const el = h('aside', { class: 'sd', 'aria-label': t('rt.sd.title'), hidden: true });
  function show(span) {
    if (!span) {
      el.hidden = true;
      swap(el);
      return;
    }
    const m = getModel();
    el.hidden = false;
    const marks = (m.marksMap.get(span.id) || []).filter((x) => x.parent_id === span.id);
    const off = relMs(span.start_ts, m.tl);
    const head = h(
      'header',
      { class: 'sd-head' },
      h(
        'div',
        { class: 'sd-title' },
        h('h3', { class: 'wrap-anywhere' }, span.name ?? '–'),
        h('div', { class: 'row gap wrap' }, catChip(span), statusChip(span.status), ...(span.flags || []).map((f) => badge(f, 'muted'))),
      ),
      h(
        'div',
        { class: 'row gap' },
        btn(t('rt.sd.zoom'), { size: 'sm', onClick: () => onZoom(span.id) }),
        btn('', { icon: 'close', size: 'sm', kind: 'ghost', title: t('rt.sd.close'), onClick: onClose }),
      ),
    );
    const timing = kvList([
      [t('rt.sd.id'), h('span', { class: 'row gap nowrap' }, h('code', { class: 'small', title: span.id }, shortId(span.id, 8, 6)), copyBtn(span.id, t('rt.sd.copy_id')))],
      [t('rt.sd.parent'), span.parent_id ? h('code', { class: 'small', title: span.parent_id }, shortId(span.parent_id, 8, 6)) : t('rt.sd.root')],
      [t('rt.sd.start'), ms3(span.start_ts)],
      [t('rt.sd.end'), span.status === 'open' ? t('rt.status.open') : ms3(span.end_ts)],
      [t('rt.sd.offset'), off != null ? `+${fmtOffsetMs(off)}` : null],
      [t('rt.sd.duration'), span.status === 'open' ? t('rt.status.open') : fmtDur(span.duration_ms)],
      [t('rt.sd.depth'), fmtInt(span.depth)],
      [t('rt.sd.turn'), span.turn_index != null ? fmtInt(span.turn_index) : null],
      [t('rt.sd.role'), span.role],
      [t('rt.sd.subtype'), span.subtype],
      [t('rt.sd.size'), span.size_bytes != null ? `${fmtInt(span.size_bytes)} B` : null],
    ]);
    const tool = span.tool
      ? h(
          'section',
          { class: 'sd-block' },
          h('h4', { class: 'sub' }, t('rt.sd.tool')),
          kvList([
            [t('rt.sd.tool_name'), h('code', span.tool.name ?? '–')],
            [t('rt.sd.call_id'), span.tool.call_id],
            [t('rt.sd.tool_type'), span.tool.type],
            [t('rt.sd.mcp'), span.tool.mcp == null ? null : span.tool.mcp ? t('common.yes') : t('common.no')],
          ]),
        )
      : null;
    const error = span.error
      ? h('div', { class: 'note bad', role: 'alert' }, h('strong', t('rt.sd.error')), ' ', h('code', span.error.type ?? '–'), span.error.status_code != null ? ` · HTTP ${span.error.status_code}` : '')
      : null;
    const marksBlock = marks.length
      ? h(
          'section',
          { class: 'sd-block' },
          h('h4', { class: 'sub' }, t('rt.sd.marks', { n: marks.length })),
          h(
            'ul',
            { class: 'plain-list marks' },
            marks.map((mk) =>
              h(
                'li',
                h(
                  'details',
                  { class: 'cnt-fold' },
                  h(
                    'summary',
                    h('code', mk.name),
                    ' ',
                    isRoutingMark(mk) ? badge(t('rt.sd.routing_mark'), 'info') : null,
                    mk.severity && mk.severity !== 'info' ? badge(mk.severity, mk.severity === 'error' ? 'bad' : 'warn') : null,
                    h('span', { class: 'muted small num' }, ` +${fmtOffsetMs(relMs(mk.ts, m.tl))}`),
                  ),
                  kvList(
                    Object.entries(mk.data || {})
                      .filter(([k, v]) => k !== 'measurements' && (v == null || typeof v !== 'object'))
                      .map(([k, v]) => [k, v == null ? null : String(v)]),
                  ),
                  (mk.data?.measurements || []).length
                    ? h(
                        'ul',
                        { class: 'plain-list' },
                        mk.data.measurements.map((x) => h('li', h('code', x.name), ` ${x.kind ?? ''} = ${x.value}${x.unit ? ` ${x.unit}` : ''}`)),
                      )
                    : null,
                ),
              ),
            ),
          ),
        )
      : null;
    const content = contentSection({
      scope,
      available: span.content_available !== false && (span.category === 'llm' || span.category === 'tool'),
      load: (opts) => api.spanContent(traceId, span.id, opts),
      render: (c, ctl) => renderSpanContent(c, ctl),
    });
    swap(el, head, error, timing, span.llm ? llmBlock(span) : null, tool, marksBlock, content);
  }
  return { el, show };
}
