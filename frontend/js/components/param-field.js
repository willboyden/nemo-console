// DOM for the param form generated from the /params catalog. Logic lives in param-form.js (pure, tested).
import { h, icon } from '../dom.js';
import { t } from '../i18n.js';
import { filterSpecs, groupSpecs, isSet, coerce, validateValue, validateAll, isSecretSpec, isRedacted } from '../param-form.js';
import { badge } from './ui.js';
import { isAdmin } from '../app-context.js';

/**
 * renderParamForm(host, {specs, values, onChange, search, showAdvanced}) — `values` is the draft (only set keys).
 * Unset params show the default as placeholder. Inputs commit in place so typing never loses focus.
 */
export function renderParamForm(host, { specs, values, onChange, search = '', showAdvanced = false }) {
  host.replaceChildren();
  const visible = filterSpecs(specs, { query: search, showAdvanced });
  if (!visible.length) { host.append(h('p', { class: 'hint' }, t('params.none'))); return; }
  const errors = validateAll(specs, values);
  for (const { group, specs: gs } of groupSpecs(visible)) {
    const setCount = gs.filter((s) => isSet(values, s.key)).length;
    const det = h('details', { class: 'pgroup', open: true }, h('summary', h('span', t(`group.${group}`) === `group.${group}` ? group : t(`group.${group}`)), setCount ? badge(t('params.set_n', { n: setCount }), 'info') : null));
    const grid = h('div', { class: 'pgrid' });
    for (const s of gs) grid.append(paramRow(s, values, errors[s.key], (v) => onChange(s.key, v)));
    det.append(grid); host.append(det);
  }
}

function paramRow(spec, values, error, set) {
  const id = `p-${spec.key}`, cur = values[spec.key], isOver = isSet(values, spec.key), ro = !isAdmin();
  const errEl = h('div', { class: 'perr', id: `${id}-err`, role: 'alert' }, error ? t(error.key, error.vars) : '');
  let input, row, resetBtn;
  const commit = (raw) => {
    const c = coerce(spec, raw);
    const bad = !c.ok ? { key: c.error } : validateValue(spec, c.value);
    errEl.textContent = bad ? t(bad.key, bad.vars) : '';
    input.setAttribute('aria-invalid', bad ? 'true' : 'false');
    if (c.ok && !bad) { set(c.value); row.classList.toggle('modified', c.value !== undefined); resetBtn.hidden = c.value === undefined; }
  };
  const defLabel = spec.default != null ? t('params.default_value', { v: String(spec.default) }) : t('params.unset');
  const secret = isSecretSpec(spec);
  if (spec.type === 'bool') {
    const state = isOver ? String(!!cur) : '';
    input = h('select', { id, disabled: ro, onChange: (e) => commit(e.target.value) }, h('option', { value: '', selected: state === '' }, defLabel), h('option', { value: 'true', selected: state === 'true' }, t('common.on')), h('option', { value: 'false', selected: state === 'false' }, t('common.off')));
  } else if (spec.type === 'enum') {
    input = h('select', { id, disabled: ro, onChange: (e) => commit(e.target.value) }, h('option', { value: '', selected: !isOver }, defLabel), (spec.choices || []).map((c) => h('option', { value: c, selected: isOver && String(cur) === c }, c)));
  } else if (spec.type === 'string_list') {
    input = h('input', { id, type: 'text', disabled: ro, placeholder: spec.default != null ? [].concat(spec.default).join(', ') : t('params.unset'), value: isOver ? [].concat(cur).join(', ') : '', spellcheck: 'false', onChange: (e) => commit(e.target.value) });
  } else {
    const num = spec.type === 'int' || spec.type === 'float';
    // A "[set]" secret is shown as a placeholder only; the value is never put in the input and never sent back.
    input = h('input', { id, type: num ? 'number' : secret ? 'password' : 'text', disabled: ro, inputmode: num ? 'decimal' : null, step: spec.type === 'float' ? 'any' : '1', min: spec.min ?? null, max: spec.max ?? null,
      placeholder: isRedacted(cur) ? t('params.secret_set') : spec.default != null ? String(spec.default) : t('params.unset'), value: isOver && !isRedacted(cur) ? String(cur) : '',
      autocomplete: secret ? 'new-password' : 'off', spellcheck: 'false', onChange: (e) => commit(e.target.value) });
  }
  input.setAttribute('aria-describedby', `${id}-help ${id}-err`);
  if (error) input.setAttribute('aria-invalid', 'true');
  resetBtn = h('button', { type: 'button', class: 'btn ghost sm', hidden: !isOver || ro, title: t('params.reset'), 'aria-label': t('params.reset_named', { name: spec.label }), onClick: () => { if (input.type === 'checkbox') return; input.value = ''; commit(''); input.focus(); } }, icon('restart', 14));
  row = h('div', { class: `prow${isOver ? ' modified' : ''}` },
    h('div', { class: 'prow-head' }, h('label', { for: id }, spec.label), spec.requires_restart ? badge(t('params.restart'), 'warn', t('params.restart_tip')) : badge(t('params.live'), 'ok'), spec.advanced ? badge(t('params.advanced'), 'muted') : null),
    h('div', { class: 'prow-input' }, input, resetBtn),
    spec.flag ? h('code', { class: 'flag' }, spec.flag) : null, h('div', { class: 'hint', id: `${id}-help` }, spec.help), errEl);
  return row;
}
