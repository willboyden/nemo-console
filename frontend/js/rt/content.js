// On-demand content of a span or a trajectory step (admin-only routes). Nothing is fetched until the button is pressed
// and never by polling. Every captured string is a text node; markup in a prompt is shown as the characters it is.
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { fmtInt } from '../format.js';
import { btn, badge, skeleton } from '../components/ui.js';
import { isAdmin } from '../app-context.js';
import { copyBtn, swap } from './ui.js';
import { contentProblem, prettyArgs } from './pure.js';

const truncNote = (p) => (p?.truncated ? h('p', { class: 'note warn small' }, t('rt.content.part_cut', { shown: fmtInt(p.text?.length ?? 0), total: fmtInt(p.chars ?? 0) })) : null);

export function renderPart(p) {
  if (!p) return null;
  const type = p.type ?? 'other';
  if (type === 'reasoning')
    return h(
      'details',
      { class: 'part part-reasoning' },
      h('summary', t('rt.content.reasoning', { n: fmtInt(p.chars ?? p.text?.length ?? 0) })),
      h('pre', { class: 'msg-text' }, p.text ?? ''),
      truncNote(p),
    );
  if (type === 'tool_call')
    return h(
      'div',
      { class: 'part part-call' },
      h('div', { class: 'part-head' }, badge(`→ ${t('rt.content.tool_call')}`, 'info'), h('code', p.name ?? '–'), p.call_id ? h('span', { class: 'muted small' }, p.call_id) : null),
      h('pre', { class: 'msg-text code' }, prettyArgs(p.text)),
      truncNote(p),
    );
  if (type === 'tool_result')
    return h(
      'div',
      { class: 'part part-result' },
      h('div', { class: 'part-head' }, badge(`← ${t('rt.content.tool_result')}`, 'muted'), p.call_id ? h('span', { class: 'muted small' }, p.call_id) : null),
      h('pre', { class: 'msg-text' }, p.text ?? ''),
      truncNote(p),
    );
  if (type === 'image')
    return h(
      'div',
      { class: 'part part-image', role: 'img', 'aria-label': t('rt.content.image') },
      h('span', { 'aria-hidden': 'true' }, '▣'),
      ' ',
      t('rt.content.image_note', { chars: fmtInt(p.chars ?? 0) }),
    );
  return h(
    'div',
    { class: `part part-text${type === 'text' ? '' : ' part-other'}` },
    type === 'text' ? null : h('div', { class: 'part-head' }, badge(String(type), 'muted')),
    h('pre', { class: 'msg-text' }, p.text ?? ''),
    p.text ? h('div', { class: 'part-actions' }, copyBtn(p.text)) : null,
    truncNote(p),
  );
}

const roleBlock = (role, parts) =>
  h(
    'div',
    { class: `msg role-${['user', 'assistant', 'system', 'tool'].includes(role) ? role : 'other'}` },
    h('div', { class: 'msg-role' }, String(role ?? '?')),
    h('div', { class: 'msg-parts' }, (parts || []).map(renderPart)),
  );

function kvLine(obj) {
  const rows = Object.entries(obj || {}).filter(([, v]) => v != null && typeof v !== 'object');
  if (!rows.length) return null;
  return h(
    'dl',
    { class: 'facts rt-facts tight' },
    rows.map(([k, v]) => h('div', { class: 'kv' }, h('dt', k), h('dd', String(v)))),
  );
}
const section = (title, ...body) => h('section', { class: 'cnt-sec' }, h('h4', { class: 'sub' }, title), ...body);
const fold = (title, ...body) => h('details', { class: 'cnt-fold' }, h('summary', title), h('div', { class: 'stack-v' }, ...body));

/** SpanContent -> nodes. */
export function renderSpanContent(c, { onMore } = {}) {
  const out = [];
  const r = c.request;
  if (r) {
    out.push(section(t('rt.content.request'), kvLine({ model: r.model, ...(r.params || {}) })));
    if (r.system?.length) out.push(fold(t('rt.content.system', { n: r.system.length }), r.system.map(renderPart)));
    const hidden = (r.messages_total ?? 0) - (r.messages_returned ?? 0);
    out.push(
      section(
        t('rt.content.messages', { n: fmtInt(r.messages_returned ?? r.messages?.length ?? 0), total: fmtInt(r.messages_total ?? 0) }),
        hidden > 0 && onMore
          ? h(
              'div',
              { class: 'row gap wrap' },
              h('span', { class: 'muted small' }, t('rt.content.messages_hidden', { n: fmtInt(hidden) })),
              btn(t('rt.content.show_first'), { size: 'sm', onClick: () => onMore({ messages: 'head' }) }),
              btn(t('rt.content.show_last'), { size: 'sm', onClick: () => onMore({ messages: 'tail' }) }),
            )
          : null,
        h(
          'div',
          { class: 'convo' },
          (r.messages || []).map((m) => roleBlock(m.role, m.parts)),
        ),
      ),
    );
    if (r.tools?.length)
      out.push(
        fold(
          t('rt.content.tools', { n: r.tools.length }),
          h(
            'ul',
            { class: 'plain-list' },
            r.tools.map((x) => h('li', h('code', x.name), x.description ? h('span', { class: 'muted small' }, ` ${x.description}`) : null)),
          ),
        ),
      );
  }
  if (c.response)
    out.push(
      section(
        t('rt.content.response'),
        kvLine({ model: c.response.model, finish_reason: c.response.finish_reason, ...(c.response.usage || {}) }),
        h('div', { class: 'convo' }, roleBlock('assistant', c.response.parts)),
      ),
    );
  if (c.tool)
    out.push(
      section(
        t('rt.content.tool'),
        c.tool.arguments ? h('div', h('h5', { class: 'sub' }, t('rt.content.arguments')), renderPart({ ...c.tool.arguments, type: 'tool_call', name: '', call_id: null })) : null,
        c.tool.result ? h('div', h('h5', { class: 'sub' }, t('rt.content.result')), renderPart({ ...c.tool.result, type: 'tool_result' })) : null,
      ),
    );
  const hd = Object.entries(c.headers || {});
  if (hd.length) out.push(fold(t('rt.content.headers', { n: hd.length }), kvLine(Object.fromEntries(hd))));
  if (!r && !c.response && !c.tool) out.push(h('p', { class: 'hint' }, t('rt.content.none')));
  const notes = [];
  if (c.truncated) notes.push(h('p', { class: 'note warn small' }, t('rt.content.truncated')));
  if (c.redactions) notes.push(h('p', { class: 'note info small' }, t('rt.content.redactions', { count: c.redactions })));
  return [...notes, ...out];
}

/** StepContent -> nodes. */
export function renderStepContent(c) {
  const out = [];
  if (c.message) out.push(section(t('rt.content.message'), renderPart(c.message)));
  if (c.reasoning) out.push(section(t('rt.content.reasoning_title'), renderPart(c.reasoning)));
  if (c.tool_calls?.length)
    out.push(
      section(
        t('rt.content.tool_calls', { n: c.tool_calls.length }),
        c.tool_calls.map((x) => renderPart({ ...x.arguments, type: 'tool_call', name: x.name, call_id: x.id })),
      ),
    );
  if (c.observations?.length)
    out.push(
      section(
        t('rt.content.observations', { n: c.observations.length }),
        c.observations.map((o) => renderPart({ ...o.content, type: 'tool_result', call_id: o.call_id })),
      ),
    );
  if (!out.length) out.push(h('p', { class: 'hint' }, t('rt.content.none')));
  const notes = [];
  if (c.truncated) notes.push(h('p', { class: 'note warn small' }, t('rt.content.truncated')));
  if (c.redactions) notes.push(h('p', { class: 'note info small' }, t('rt.content.redactions', { count: c.redactions })));
  return [...notes, ...out];
}

/**
 * The Content section: a button, and on press one request. `load(opts)` returns the content, `render` draws it.
 * `available` false (no source line) disables the button. Admin only: for a viewer the button says so up front.
 */
export function contentSection({ scope, load, render, available = true, label, compact = false }) {
  const body = h('div', { class: 'cnt-body', 'aria-live': 'polite' });
  const state = { opts: {} };
  const go = async (more = {}) => {
    Object.assign(state.opts, more);
    swap(body, skeleton(2));
    try {
      const c = await load({ ...state.opts });
      if (!scope.alive) return;
      const nodes = render(c, {
        onMore: (o) => go(o),
      });
      const bigger = btn(t('rt.content.full_text'), { size: 'sm', onClick: () => go({ max_chars: 200000 }) });
      swap(body, ...nodes, state.opts.max_chars ? null : h('div', { class: 'row gap' }, bigger, h('span', { class: 'hint' }, t('rt.content.full_hint'))));
    } catch (e) {
      if (!scope.alive) return;
      const p = contentProblem(e);
      swap(body, h('div', { class: `note ${p.kind}`, role: 'alert' }, h('strong', p.title), h('p', p.text)));
    }
  };
  const button = btn(label ?? t('rt.content.load'), {
    icon: 'lock',
    size: 'sm',
    disabled: !available,
    onClick: () => go(),
    title: available ? t('rt.content.load_tip') : t('rt.content.unavailable_tip'),
  });
  if (compact) return h('section', { class: 'cnt compact' }, h('div', { class: 'row gap wrap' }, button, isAdmin() ? null : h('span', { class: 'hint' }, t('rt.content.explain_viewer_short'))), body);
  return h(
    'section',
    { class: 'cnt' },
    h('h4', { class: 'sub' }, t('rt.content.title')),
    h('p', { class: 'hint' }, isAdmin() ? t('rt.content.explain') : t('rt.content.explain_viewer')),
    h('div', { class: 'row gap wrap' }, button),
    body,
  );
}
