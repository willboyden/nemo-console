// Playground: one chat request THROUGH a system, streamed. Nothing is persisted (kept in memory for the page only).
import { h, clear } from '../dom.js';
import { t } from '../i18n.js';
import { clampMaxTokens, fmtInt } from '../format.js';
import { extractDelta, chatBody, servedBy } from '../stream-text.js';
import { stream } from '../app-context.js';
import { badge, btn, adminBtn, card, field, select, emptyBox, errorBox, skeleton } from './ui.js';
import { toastError } from './toast.js';
import { announce } from './live.js';
import { isAdmin } from '../app-context.js';

export function playgroundPanel(scope, systemId) {
  const host = h('div', { class: 'stack-v' }, skeleton(3));
  scope.api.playground(systemId).then((spec) => { if (scope.alive) { clear(host); host.append(spec ? build(spec) : emptyBox(t('pg.unavailable'), t('pg.unavailable_hint'))); } })
    .catch((e) => { if (scope.alive) { clear(host); host.append(errorBox(e)); } });

  function build(spec) {
    let messages = [], active = null;
    const model = select(spec.models.map((m) => ({ value: m, label: m })), spec.default_model, null, { id: 'pg-model' });
    const maxTok = h('input', { type: 'number', min: 1, max: 4096, value: '256', id: 'pg-max' });
    const prompt = h('textarea', { rows: 4, id: 'pg-prompt', placeholder: t('pg.prompt_ph'), 'aria-label': t('pg.prompt') });
    const log = h('div', { class: 'pg-log', role: 'log', 'aria-live': 'off', tabindex: '0', 'aria-label': t('pg.transcript') });
    const meta = h('div', { class: 'row gap wrap', 'aria-live': 'polite' });
    const send = adminBtn(t('pg.send'), { kind: 'primary', icon: 'send', onClick: () => go() });
    const stop = btn(t('pg.stop'), { icon: 'stop', onClick: () => active?.close() }); stop.hidden = true;
    const clearBtn = btn(t('pg.clear'), { onClick: () => { messages = []; clear(log); clear(meta); } });
    prompt.addEventListener('keydown', (e) => { if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); go(); } });

    function go() {
      const text = prompt.value.trim();
      if (!text || active || !isAdmin()) return;
      const mt = clampMaxTokens(maxTok.value); maxTok.value = String(mt);
      messages.push({ role: 'user', content: text });
      log.append(h('div', { class: 'msg user' }, h('div', { class: 'msg-role' }, t('pg.you')), h('div', { class: 'msg-text' }, text)));
      const out = h('div', { class: 'msg-text streaming' }), bubble = h('div', { class: 'msg assistant' }, h('div', { class: 'msg-role' }, spec.wire === 'anthropic_messages' ? t('pg.assistant') : t('pg.assistant')), out);
      log.append(bubble); log.scrollTop = log.scrollHeight;
      prompt.value = ''; clear(meta);
      let reply = '', usage = {}, ended = false;
      send.disabled = true; stop.hidden = false;
      const finish = (err) => {
        if (ended) return; ended = true; active = null; send.disabled = false; stop.hidden = true; out.classList.remove('streaming');
        if (reply) messages.push({ role: 'assistant', content: reply });
        else if (err) { out.textContent = ''; bubble.classList.add('err'); out.append(h('span', { class: 'bad-text' }, err)); }
        if (usage.prompt_tokens != null || usage.completion_tokens != null) meta.append(badge(t('pg.tokens', { p: fmtInt(usage.prompt_tokens), c: fmtInt(usage.completion_tokens) }), 'muted'));
        announce(err ? err : t('pg.done'));
      };
      active = stream(`/systems/${systemId}/playground/chat`, {
        method: 'POST', reconnect: false,
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(chatBody(spec.wire, { model: model.value, messages, maxTokens: mt })),
        onResponse: (res) => {
          const sb = servedBy(res.headers);
          if (sb.route) meta.append(badge(t('pg.served_route', { v: sb.route }), 'info'));
          if (sb.tier) meta.append(badge(t('pg.served_tier', { v: sb.tier }), 'info'));
        },
        onEvent: (ev) => {
          if (ev.event === 'done') return;
          const d = extractDelta(spec.wire, ev);
          if (d.text) { reply += d.text; out.textContent = reply; log.scrollTop = log.scrollHeight; }   // textContent: model text is never markup
          if (d.usage) usage = { ...usage, ...d.usage };
        },
        onError: (e) => { if (e?.status) { messages.pop(); finish(`${e.message}`); toastError(e); } },
        onClose: () => finish(),
      });
    }
    return h('div', { class: 'stack-v' },
      h('p', { class: 'note info' }, t('pg.not_persisted'), spec.note ? ` ${spec.note}` : ''),
      card(t('pg.title'), h('div', { class: 'stack-v' },
        h('div', { class: 'row gap wrap bottom' }, field(t('pg.model'), model), field(t('pg.max_tokens'), maxTok, t('pg.max_hint'))),
        log, meta, field(t('pg.prompt'), prompt, t('pg.send_hint')), h('div', { class: 'row gap' }, send, stop, clearBtn))));
  }
  return host;
}
