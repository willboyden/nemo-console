// Actions from GET /systems/{id}/actions. disruptive/destructive always go through a confirm dialog that repeats the effect.
import { h, clear } from '../dom.js';
import { t } from '../i18n.js';
import { fmtMs } from '../format.js';
import { actionParamValues } from '../param-form.js';
import { badge, btn, adminBtn, card, dangerBadge, field, select, problemList, emptyBox, errorBox, skeleton } from './ui.js';
import { confirmEffect } from './dialog.js';
import { toastError } from './toast.js';
import { announce } from './live.js';
import { describeError } from '../errors.js';

/** Render the output of an action prominently. */
export function actionResultView(res, label) {
  return h('div', { class: `action-result ${res.ok ? 'ok' : 'bad'}`, role: 'status' },
    h('div', { class: 'row gap wrap' }, badge(res.ok ? t('act.ok') : t('act.failed'), res.ok ? 'ok' : 'bad'), h('strong', label), h('span', { class: 'muted small num' }, fmtMs(res.duration_ms))),
    problemList(res.problems), res.output ? h('pre', { class: 'cmd output', tabindex: '0', 'aria-label': t('act.output_label', { name: label }) }, res.output) : h('p', { class: 'hint' }, t('act.no_output')));
}

export function actionParamInputs(spec) {
  const inputs = {};
  const nodes = spec.params.map((p) => {
    let input;
    if (p.type === 'bool') input = h('input', { type: 'checkbox', checked: p.default === true });
    else if (p.type === 'enum') input = select((p.choices || []).map((c) => ({ value: c, label: c })), String(p.default ?? ''), null);
    else input = h('input', { type: p.type === 'int' || p.type === 'float' ? 'number' : 'text', value: p.default != null ? String(p.default) : '', min: p.min ?? null, max: p.max ?? null });
    inputs[p.key] = { spec: p, input };
    return p.type === 'bool' ? h('div', { class: 'field' }, h('label', { class: 'check' }, input, h('span', p.label)), h('div', { class: 'hint' }, p.help)) : field(p.label, input, p.help);
  });
  const read = () => actionParamValues(spec.params, Object.fromEntries(Object.entries(inputs).map(([k, v]) => [k, v.spec.type === 'bool' ? (v.input.checked ? 'true' : 'false') : v.input.value])));
  return { nodes, read };
}

/** opts.only: action ids to show; opts.exclude: ids to hide; opts.results: shared Map for last results. */
export function runAction(scope, systemId, spec, params) {
  return scope.api.runAction(systemId, spec.id, params, spec.danger !== 'safe');
}

export function actionsPanel(scope, systemId, { only = null, exclude = [], onResult = null } = {}) {
  const host = h('div', { class: 'stack-v' }, skeleton(3));
  const results = new Map();
  scope.api.actions(systemId).then((list) => {
    if (!scope.alive) return;
    list = list.filter((a) => (!only || only.includes(a.id)) && !exclude.includes(a.id));
    clear(host);
    if (!list.length) { host.append(emptyBox(t('act.none'))); return; }
    for (const a of list) host.append(actionCard(a));
  }).catch((e) => { if (scope.alive) { clear(host); host.append(errorBox(e)); } });

  function actionCard(a) {
    const out = h('div', { 'aria-live': 'polite' });
    const p = actionParamInputs(a);
    let run;
    const exec = async () => {
      const params = p.read();
      if (a.danger !== 'safe') {
        // An action may carry its own confirm line (act.note.<system>.<action>) when the generic danger text would be
        // wrong: a Tier 3 evaluation interrupts nothing, it spends tokens.
        const noteKey = `act.note.${systemId}.${a.id}`, note = t(noteKey);
        const ok = await confirmEffect({ title: t('act.confirm_title', { name: a.label }), effect: a.description, danger: a.danger === 'destructive', confirmLabel: a.label,
          lines: [note !== noteKey ? note : t(`act.danger_${a.danger}`), ...Object.entries(params).map(([k, v]) => `${k} = ${JSON.stringify(v)}`)] });
        if (!ok) return;
      }
      run.disabled = true; clear(out).append(skeleton(1));
      try {
        const res = await runAction(scope, systemId, a, params);
        if (!scope.alive) return;
        results.set(a.id, res); clear(out).append(actionResultView(res, a.label));
        announce(t(res.ok ? 'act.done_ok' : 'act.done_failed', { name: a.label }));
        onResult?.(a, res);
      } catch (e) { if (!scope.alive) return; clear(out).append(h('div', { class: 'note bad', role: 'alert' }, describeError(e))); toastError(e); }
      finally { run.disabled = false; }
    };
    run = adminBtn(a.label, { kind: a.danger === 'safe' ? '' : a.danger === 'destructive' ? 'danger' : 'primary', icon: 'play', onClick: exec });
    return card(a.label, h('div', { class: 'stack-v' }, h('p', { class: 'muted' }, a.description), p.nodes.length ? h('div', { class: 'row gap wrap bottom' }, p.nodes) : null, h('div', { class: 'row gap' }, run), out), dangerBadge(a.danger));
  }
  return host;
}
