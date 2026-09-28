// Small presentational helpers built on h().
import { h, icon, copyText } from '../dom.js';
import { t } from '../i18n.js';
import { describeError } from '../errors.js';
import { isAdmin } from '../app-context.js';
import { toast } from './toast.js';

export const badge = (text, kind = 'muted', title) => h('span', { class: `badge ${kind}`, title }, text);
export function btn(label, { icon: ic, kind = '', size = '', onClick, title, type = 'button', disabled, ariaLabel } = {}) {
  return h('button', { type, class: `btn ${kind} ${size}`.trim(), onClick, title, disabled, 'aria-label': ariaLabel || (label ? null : title) },
    ic ? icon(ic, 16) : null, label ? h('span', label) : null);
}
/** A control only admins may use: for a viewer it is rendered disabled with the reason as tooltip and description. */
export function adminBtn(label, opts = {}) {
  if (isAdmin()) return btn(label, opts);
  return btn(label, { ...opts, onClick: undefined, disabled: true, title: t('role.viewer_disabled') });
}
export const card = (title, body, actions, { id } = {}) => h('section', { class: 'card', id, 'aria-label': typeof title === 'string' ? title : null },
  title || actions ? h('header', { class: 'card-head' }, h('h2', title), actions ? h('div', { class: 'row gap' }, actions) : null) : null,
  h('div', { class: 'card-body' }, body));

const HEALTH_KIND = { up: 'ok', degraded: 'warn', down: 'bad', unknown: 'muted' };
export const healthBadge = (hl) => h('span', { class: `badge ${HEALTH_KIND[hl] || 'muted'}` }, h('span', { class: `dot ${hl}`, 'aria-hidden': 'true' }), t(`health.${hl}`) === `health.${hl}` ? hl : t(`health.${hl}`));
const COMP_KIND = { running: 'ok', stopped: 'muted', failed: 'bad', absent: 'warn', unknown: 'muted' };
export const compBadge = (s) => badge(t(`comp.${s}`) === `comp.${s}` ? s : t(`comp.${s}`), COMP_KIND[s] || 'muted');
const SEV_KIND = { info: 'info', warn: 'warn', error: 'bad' };
export const sevBadge = (s) => badge(t(`sev.${s}`) === `sev.${s}` ? s : t(`sev.${s}`), SEV_KIND[s] || 'muted');
const DANGER_KIND = { safe: 'ok', disruptive: 'warn', destructive: 'bad' };
export const dangerBadge = (d) => badge(t(`danger.${d}`) === `danger.${d}` ? d : t(`danger.${d}`), DANGER_KIND[d] || 'muted');

let uid = 0;
export function field(label, control, hint) {
  const id = control.id || `f${++uid}`;
  control.id = id;
  return h('div', { class: 'field' }, h('label', { for: id }, label), control, hint ? h('div', { class: 'hint' }, hint) : null);
}
export function select(options, value, onChange, attrs = {}) {
  return h('select', { ...attrs, onChange: (e) => onChange?.(e.target.value) },
    options.map((o) => { const v = typeof o === 'string' ? { value: o, label: o } : o; return h('option', { value: v.value, selected: v.value === value }, v.label); }));
}

export const skeleton = (n = 3) => h('div', { class: 'skeleton-group', role: 'status', 'aria-busy': 'true', 'aria-label': t('common.loading') },
  Array.from({ length: n }, () => h('div', { class: 'skeleton' })));
export function errorBox(err, retry) {
  return h('div', { class: 'state error', role: 'alert' }, icon('warn', 22),
    h('div', h('strong', t('common.error')), h('p', describeError(err))),
    retry ? btn(t('common.retry'), { icon: 'restart', onClick: retry }) : null);
}
export const emptyBox = (title, hint, action) => h('div', { class: 'state empty' }, h('strong', title), hint ? h('p', hint) : null, action || null);
export const kv = (k, v) => h('div', { class: 'kv' }, h('dt', k), h('dd', v));

/** A copy-pasteable command with a Copy button (fixes from Problem.fix). The command is text, never markup. */
export function fixBlock(command) {
  const b = btn(t('common.copy'), { icon: 'copy', size: 'sm', onClick: async () => { const ok = await copyText(command); toast(ok ? t('common.copied') : t('common.copy_failed'), { kind: ok ? 'ok' : 'bad', timeout: 1800 }); } });
  return h('div', { class: 'fix' }, h('span', { class: 'fix-label' }, t('problem.fix')), h('code', { class: 'fix-cmd' }, command), b);
}
export function problemList(problems) {
  if (!problems?.length) return null;
  return h('ul', { class: 'problems' }, problems.map((p) => h('li', { class: `problem ${p.severity}` },
    h('div', { class: 'problem-head' }, sevBadge(p.severity), h('span', { class: 'problem-msg' }, p.message)), h('code', { class: 'muted small problem-code' }, p.code),
    p.fix ? fixBlock(p.fix) : null)));
}

/** ARIA tablist with roving tabindex + arrow keys. onSelect(id). */
export function tabs(defs, active, onSelect, label) {
  const list = h('div', { class: 'tabs', role: 'tablist', 'aria-label': label });
  const buttons = defs.map((d) => h('button', { type: 'button', class: `tab${d.id === active ? ' on' : ''}`, role: 'tab', id: `tab-${d.id}`, 'aria-selected': d.id === active ? 'true' : 'false',
    'aria-controls': 'tabpanel', tabindex: d.id === active ? '0' : '-1', dataset: { tab: d.id }, onClick: () => select_(d.id) }, d.label));
  const select_ = (id, focus = false) => {
    buttons.forEach((b) => { const on = b.dataset.tab === id; b.classList.toggle('on', on); b.setAttribute('aria-selected', String(on)); b.tabIndex = on ? 0 : -1; if (on && focus) b.focus(); });
    onSelect(id);
  };
  list.addEventListener('keydown', (e) => {
    const i = buttons.findIndex((b) => b === document.activeElement);
    if (i < 0) return;
    const move = { ArrowRight: 1, ArrowLeft: -1 }[e.key];
    if (move) { e.preventDefault(); select_(buttons[(i + move + buttons.length) % buttons.length].dataset.tab, true); }
    else if (e.key === 'Home') { e.preventDefault(); select_(buttons[0].dataset.tab, true); }
    else if (e.key === 'End') { e.preventDefault(); select_(buttons.at(-1).dataset.tab, true); }
  });
  list.append(...buttons);
  return list;
}
export const stat = (label, value, sub) => h('div', { class: 'stat' }, h('span', { class: 'stat-label' }, label), h('strong', { class: 'stat-value num' }, value), sub ? h('span', { class: 'stat-sub muted small' }, sub) : null);
