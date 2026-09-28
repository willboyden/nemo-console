// System status: health header, endpoints, components and Problems (each with a copy-pasteable fix).
import { h } from '../dom.js';
import { t } from '../i18n.js';
import { timeAgo } from '../format.js';
import { healthBadge, compBadge, problemList, kv } from './ui.js';

export const AUTH_KEY = { none: 'auth.none', bearer: 'auth.bearer', forwarded: 'auth.forwarded' };

export function statusSummary(st) {
  return h('div', { class: 'stack-v' },
    h('div', { class: 'row gap wrap' }, healthBadge(st.health), st.version ? h('span', { class: 'badge muted' }, `v${st.version}`) : null,
      h('span', { class: 'muted small' }, t('status.checked', { ago: timeAgo(st.checked_at) }))),
    h('p', { class: 'status-summary' }, st.summary));
}

export function componentsTable(components) {
  if (!components?.length) return h('p', { class: 'hint' }, t('status.no_components'));
  return h('div', { class: 'tablewrap' }, h('table', { class: 'compact' },
    h('thead', h('tr', ['status.col_kind', 'status.col_name', 'status.col_state', 'status.col_detail'].map((k) => h('th', { scope: 'col' }, t(k))))),
    h('tbody', components.map((c) => h('tr', h('td', h('span', { class: 'badge muted' }, t(`kind.${c.kind}`))), h('td', h('code', c.name), c.image ? h('div', { class: 'muted small ellipsis' }, c.image) : null), h('td', compBadge(c.state)), h('td', { class: 'wrap-anywhere' }, c.detail || '–'))))));
}

export function endpointsList(endpoints) {
  if (!endpoints?.length) return null;
  return h('dl', { class: 'facts' }, endpoints.map((e) => kv(`${e.name}`, h('span', h('code', e.url), ' ', h('span', { class: 'badge muted' }, e.role), ' ', h('span', { class: 'muted small' }, t(AUTH_KEY[e.auth] || 'auth.none'))))));
}

export function statusBody(st) {
  return h('div', { class: 'stack-v' }, statusSummary(st), problemList(st.problems), h('h3', { class: 'sub' }, t('status.components')), componentsTable(st.components),
    st.endpoints?.length ? h('h3', { class: 'sub' }, t('status.endpoints')) : null, endpointsList(st.endpoints));
}

/** Components and endpoints only (the page header already shows health, summary and Problems). */
export function statusDetails(st) {
  return h('div', { class: 'stack-v' }, componentsTable(st.components), st.endpoints?.length ? h('h3', { class: 'sub' }, t('status.endpoints')) : null, endpointsList(st.endpoints));
}
