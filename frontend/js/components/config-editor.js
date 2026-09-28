// Config file editor: read, edit, Validate, DIFF vs current, confirm (states apply_effect), Apply with base_sha256,
// 409 -> reload + re-diff, auto-rolled-back outcome, revision history + rollback. Read-only files show why_readonly.
import { h, clear, icon } from '../dom.js';
import { t } from '../i18n.js';
import { diffLines, diffStats, toUnified, hunkHeader, toSideBySide } from '../diff.js';
import { shortSha, fmtDate } from '../format.js';
import { isAdmin } from '../app-context.js';
import { badge, btn, adminBtn, card, kv, errorBox, skeleton, emptyBox, sevBadge, select } from './ui.js';
import { confirmEffect } from './dialog.js';
import { toast, toastError } from './toast.js';
import { announce } from './live.js';
import { describeError } from '../errors.js';

export function lineOffset(text, line) {
  let idx = 0;
  for (let i = 1; i < line; i++) { const n = text.indexOf('\n', idx); if (n < 0) return text.length; idx = n + 1; }
  return idx;
}

/** Render a diff (ops from diffLines) as DOM. mode: 'unified' | 'split'. */
export function diffView(ops, mode) {
  const st = diffStats(ops);
  if (!st.changed) return h('p', { class: 'note ok', role: 'status' }, t('diff.none'));
  const head = h('p', { class: 'diff-stats small' }, h('span', { class: 'add-text' }, `+${st.add}`), ' ', h('span', { class: 'del-text' }, `-${st.del}`), ' ', t('diff.lines'));
  if (mode === 'split') {
    const cell = (c) => [h('td', { class: `ln ${c ? c.type : 'gap'}` }, c ? String(c.n) : ''), h('td', { class: `code ${c ? c.type : 'gap'}` }, c ? c.text : '')];
    const body = [];
    for (const hk of toUnified(ops, 3)) {
      body.push(h('tr', { class: 'hunk' }, h('td', { colspan: 4 }, hunkHeader(hk))));
      for (const r of toSideBySide(hk.lines)) body.push(h('tr', cell(r.left), cell(r.right)));
    }
    return h('div', { class: 'diff-wrap' }, head, h('div', { class: 'tablewrap tall' }, h('table', { class: 'diff split', 'aria-label': t('diff.title') },
      h('colgroup', h('col', { class: 'c-ln' }), h('col', { class: 'c-code' }), h('col', { class: 'c-ln' }), h('col', { class: 'c-code' })),
      h('thead', h('tr', h('th', { colspan: 2, scope: 'colgroup' }, t('diff.current')), h('th', { colspan: 2, scope: 'colgroup' }, t('diff.edited')))),
      h('tbody', body))));
  }
  const body = [];
  for (const hk of toUnified(ops, 3)) {
    body.push(h('tr', { class: 'hunk' }, h('td', { colspan: 3 }, hunkHeader(hk))));
    for (const l of hk.lines) body.push(h('tr', h('td', { class: `ln ${l.type}` }, l.a != null ? String(l.a) : ''), h('td', { class: `ln ${l.type}` }, l.b != null ? String(l.b) : ''),
      h('td', { class: `code ${l.type}` }, h('span', { class: 'sign', 'aria-hidden': 'true' }, l.type === 'add' ? '+' : l.type === 'del' ? '-' : ' '), l.text,
        l.type !== 'eq' ? h('span', { class: 'sr-only' }, ` (${t(`diff.${l.type}`)})`) : null)));
  }
  return h('div', { class: 'diff-wrap' }, head, h('div', { class: 'tablewrap tall' }, h('table', { class: 'diff unified', 'aria-label': t('diff.title') }, h('tbody', body))));
}

export function outcomeBanner(res, { rollback = false } = {}) {
  const facts = [
    res.revision_id ? t('apply.revision', { id: res.revision_id }) : null,
    res.new_sha256 ? t('apply.sha', { sha: shortSha(res.new_sha256) }) : null,
    t('apply.restarted', { v: res.restarted ? t('common.yes') : t('common.no') }),
    t('apply.post_check', { v: t(`postcheck.${res.post_check}`) }),
  ].filter(Boolean);
  let kind, title;
  if (res.rolled_back) { kind = 'bad'; title = t('apply.rolled_back'); }
  else if (!res.ok) { kind = 'bad'; title = t('apply.failed'); }
  else if (res.post_check === 'unhealthy') { kind = 'warn'; title = t('apply.unhealthy'); }
  else { kind = 'ok'; title = rollback ? t('apply.rollback_ok') : t('apply.ok'); }
  return h('div', { class: `note ${kind} outcome`, role: 'status' }, h('strong', title),
    res.rolled_back ? h('p', t('apply.rolled_back_explain')) : null,
    h('p', { class: 'small' }, facts.join(' · ')), res.detail ? h('pre', { class: 'cmd' }, res.detail) : null);
}

export function validationView(v) {
  const errs = v.diagnostics.filter((d) => d.severity === 'error').length;
  return { errs, node: h('div', { class: 'stack-v validation', role: 'status' },
    h('div', { class: 'row gap wrap' }, badge(v.ok ? t('val.ok') : t('val.failed'), v.ok ? 'ok' : 'bad'), h('span', { class: 'muted small' }, t('val.validator', { name: v.validator })),
      v.authoritative ? null : badge(t('val.not_authoritative'), 'warn')),
    v.authoritative ? null : h('p', { class: 'note warn' }, h('strong', t('val.caveat_title')), ' ', t('val.caveat'), v.note ? ` ${v.note}` : ''),
    v.authoritative && v.note ? h('p', { class: 'hint' }, v.note) : null,
    v.diagnostics.length ? h('ul', { class: 'diags' }, v.diagnostics.map((d) => h('li', sevBadge(d.severity), ' ', d.line != null ? h('button', { type: 'button', class: 'link', dataset: { line: String(d.line) } }, t('val.line', { n: d.line })) : null, ' ', d.message))) : h('p', { class: 'hint' }, t('val.no_diags'))) };
}

export function configEditor(scope, systemId) {
  const root = h('div', { class: 'stack-v cfg' });
  const st = { specs: [], file: null, draft: '', validation: null, diffMode: 'split', showDiff: true, outcome: null, conflict: null, busy: false, history: null, historyOpen: false };
  const pickerHost = h('div'), bodyHost = h('div', { class: 'stack-v' });
  root.append(pickerHost, bodyHost);

  const dirty = () => st.file && st.draft !== st.file.content;
  const editable = () => st.file?.spec.editable && isAdmin();

  async function loadList() {
    clear(bodyHost).append(skeleton(4));
    try {
      st.specs = await scope.api.configList(systemId);
      if (!scope.alive) return;
      if (!st.specs.length) { clear(bodyHost).append(emptyBox(t('cfg.no_files'))); return; }
      clear(pickerHost);
      if (st.specs.length > 1) {
        const sel = h('div', { class: 'seg-control', role: 'group', 'aria-label': t('cfg.files') }, st.specs.map((s) => h('button', { type: 'button', 'aria-pressed': 'false', dataset: { file: s.id }, onClick: () => loadFile(s.id) },
          s.label, s.editable ? null : h('span', { class: 'sr-only' }, ` (${t('cfg.readonly')})`), s.editable ? null : icon('lock', 13))));
        pickerHost.append(sel);
      }
      await loadFile((st.file?.spec.id) || st.specs[0].id);
    } catch (e) { if (scope.alive) clear(bodyHost).append(errorBox(e, loadList)); }
  }
  const discardOk = () => confirmEffect({ title: t('cfg.discard_title'), effect: t('cfg.discard_effect'), danger: true, confirmLabel: t('cfg.discard') });
  async function loadFile(id, { keepDraft = false, keepOutcome = false } = {}) {
    if (!keepDraft && dirty() && st.file?.spec.id !== id && !(await discardOk())) return;
    clear(bodyHost).append(skeleton(4));
    try {
      const f = await scope.api.config(systemId, id);
      if (!scope.alive) return;
      const prevDraft = st.draft;
      st.file = f; st.draft = keepDraft ? prevDraft : f.content; st.validation = null; st.outcome = keepDraft || keepOutcome ? st.outcome : null; st.history = null;
      if (!keepDraft) st.conflict = null;
      pickerHost.querySelectorAll('button[data-file]').forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.file === id)));
      renderBody();
    } catch (e) { if (scope.alive) clear(bodyHost).append(errorBox(e, () => loadFile(id))); }
  }

  const statusEl = () => h('span', { class: 'row gap' }, dirty() ? badge(t('cfg.unsaved'), 'warn') : badge(t('cfg.in_sync'), 'muted'), h('span', { class: 'muted small num', title: st.file.sha256 }, t('cfg.sha', { sha: shortSha(st.file.sha256) })));

  function renderBody() {
    const f = st.file, sp = f.spec;
    clear(bodyHost);
    const facts = h('dl', { class: 'facts' },
      kv(t('cfg.path'), h('code', sp.display_path)), kv(t('cfg.format'), sp.format),
      sp.validators.length ? kv(t('cfg.validators'), sp.validators.join(', ')) : null,
      sp.apply_effect ? kv(t('cfg.apply_effect'), sp.apply_effect) : null,
      kv(t('cfg.modified'), fmtDate(f.mtime)));
    const info = h('div', { class: 'stack-v' }, h('p', { class: 'muted' }, sp.description), facts,
      sp.editable ? null : h('p', { class: 'note info readonly', role: 'note' }, icon('lock', 14), ' ', h('strong', t('cfg.readonly_why')), ' ', sp.why_readonly || t('cfg.readonly_default')),
      sp.editable && !isAdmin() ? h('p', { class: 'note info' }, t('role.viewer_readonly')) : null,
      sp.editable && !sp.authoritative ? h('p', { class: 'note warn' }, t('val.caveat_short')) : null);
    bodyHost.append(card(sp.label, info, statusEl()));

    const controls = h('div', { class: 'row gap wrap' });
    const outHost = h('div', { class: 'stack-v', 'aria-live': 'polite' }), diffHost = h('div'), valHost = h('div', { 'aria-live': 'polite' });
    let ta;
    if (editable()) {
      ta = h('textarea', { class: 'code-edit', rows: 22, spellcheck: 'false', 'aria-label': t('cfg.editor_label', { file: sp.label }), wrap: 'off', value: st.draft,
        onInput: (e) => { st.draft = e.target.value; st.validation = null; clear(valHost); refreshDiff(); refreshStatus(); } });
    } else {
      ta = h('pre', { class: 'code-view', tabindex: '0', 'aria-label': t('cfg.viewer_label', { file: sp.label }) }, f.content);
    }
    const refreshStatus = () => { const c = bodyHost.querySelector('.card .card-head'); if (c) { c.lastChild?.remove(); c.append(statusEl()); } applyBtn && (applyBtn.disabled = st.busy || !dirty()); };
    const refreshDiff = () => {
      clear(diffHost);
      if (!editable() || !st.showDiff) return;
      diffHost.append(card(t('diff.title'), diffView(diffLines(f.content, st.draft), st.diffMode)));
    };
    let applyBtn = null;
    if (editable()) {
      const modeSel = select([{ value: 'split', label: t('diff.mode_split') }, { value: 'unified', label: t('diff.mode_unified') }, { value: 'off', label: t('diff.mode_off') }], st.showDiff ? st.diffMode : 'off',
        (v) => { st.showDiff = v !== 'off'; if (v !== 'off') st.diffMode = v; refreshDiff(); }, { 'aria-label': t('diff.mode') });
      applyBtn = adminBtn(t('cfg.apply'), { kind: 'primary', icon: 'check', onClick: onApply, disabled: !dirty() });
      controls.append(btn(t('cfg.validate'), { icon: 'shield', onClick: () => onValidate(valHost, true) }), modeSel, adminBtn(t('cfg.reload'), { icon: 'restart', onClick: () => onReload() }), applyBtn,
        btn(t('cfg.history'), { icon: 'history', onClick: () => toggleHistory() }));
      // Diagnostics with a line number jump into the editor.
      valHost.addEventListener('click', (e) => {
        const b = e.target.closest('button[data-line]'); if (!b) return;
        const pos = lineOffset(st.draft, Number(b.dataset.line)); ta.focus(); ta.setSelectionRange(pos, pos);
        ta.scrollTop = Math.max(0, (Number(b.dataset.line) - 3) * 19);
      });
    } else controls.append(btn(t('cfg.reload'), { icon: 'restart', onClick: () => onReload() }), btn(t('cfg.history'), { icon: 'history', onClick: () => toggleHistory() }));

    bodyHost.append(card(t('cfg.content'), h('div', { class: 'stack-v' }, ta, controls)), valHost, outHost, diffHost);
    if (st.conflict) outHost.append(conflictBox());
    if (st.outcome) outHost.append(outcomeBanner(st.outcome.res, st.outcome.opts));
    if (st.validation) valHost.append(validationView(st.validation).node);
    const histHost = h('div'); bodyHost.append(histHost); renderHistory(histHost);
    if (st.historyOpen && !st.history) fetchHistory().then(() => { if (scope.alive) renderHistory(histHost); });
    refreshDiff();
    st.ui = { outHost, valHost, histHost, ta, refreshDiff, refreshStatus };
  }

  function conflictBox() {
    return h('div', { class: 'note bad conflict', role: 'alert' }, h('strong', t('conflict.title')), h('p', t('conflict.body')),
      h('div', { class: 'row gap wrap' },
        btn(t('conflict.reload_rediff'), { kind: 'primary', icon: 'restart', onClick: async () => { st.conflict = null; await loadFile(st.file.spec.id, { keepDraft: true }); announce(t('conflict.rediffed')); toast(t('conflict.rediffed'), { kind: 'info' }); } }),
        btn(t('conflict.discard'), { onClick: async () => { st.conflict = null; st.draft = ''; await loadFile(st.file.spec.id); } })));
  }

  async function onReload() {
    if (dirty() && !(await discardOk())) return;
    st.conflict = null; await loadFile(st.file.spec.id);
  }

  async function onValidate(valHost, announceIt) {
    clear(valHost).append(skeleton(1));
    try {
      const v = await scope.api.validate(systemId, st.file.spec.id, st.draft);
      st.validation = v;
      const r = validationView(v);
      clear(valHost).append(r.node);
      if (announceIt) announce(v.ok ? t('val.ok') : t('val.failed_n', { n: r.errs }));
      return v;
    } catch (e) { clear(valHost).append(errorBox(e)); toastError(e); return null; }
  }

  async function onApply() {
    if (!dirty()) { toast(t('cfg.no_changes'), { kind: 'info' }); return; }
    const { valHost, outHost } = st.ui;
    const v = await onValidate(valHost, false);
    if (!v) return;
    if (v.diagnostics.some((d) => d.severity === 'error') || !v.ok) { toast(t('cfg.fix_first'), { kind: 'bad' }); announce(t('cfg.fix_first')); return; }
    const sp = st.file.spec, stats = diffStats(diffLines(st.file.content, st.draft));
    const restartBox = h('input', { type: 'checkbox', checked: true, id: 'restart-after' });
    const lines = [t('cfg.confirm_changes', { add: stats.add, del: stats.del }), t('cfg.confirm_base', { sha: shortSha(st.file.sha256), path: sp.display_path }), t('cfg.confirm_backup'),
      v.authoritative ? null : t('val.caveat_confirm')].filter(Boolean);
    const ok = await confirmEffect({ title: t('cfg.confirm_title', { file: sp.label }), effect: sp.apply_effect || t('cfg.apply_effect_default'), lines, danger: true, confirmLabel: t('cfg.apply'),
      extra: h('label', { class: 'check' }, restartBox, h('span', t('cfg.restart_after'))) });
    if (!ok) return;
    st.busy = true; st.ui.refreshStatus();
    try {
      const res = await scope.api.apply(systemId, sp.id, { content: st.draft, base_sha256: st.file.sha256, restart: restartBox.checked });
      st.outcome = { res, opts: {} };
      announce(res.rolled_back ? t('apply.rolled_back') : res.ok ? t('apply.ok') : t('apply.failed'));
      // After a successful write the file on disk equals our draft; after a rollback it equals the old content and the draft is kept for fixing.
      await loadFile(sp.id, { keepDraft: !res.ok || res.rolled_back, keepOutcome: true });
    } catch (e) {
      if (e.status === 409) { st.conflict = { at: Date.now() }; announce(t('conflict.title')); renderBody(); }
      else { toastError(e); clear(outHost).append(h('div', { class: 'note bad', role: 'alert' }, describeError(e))); }
    } finally { st.busy = false; }
  }

  async function toggleHistory() { st.historyOpen = !st.historyOpen; if (st.historyOpen && !st.history) await fetchHistory(); renderHistory(st.ui.histHost); }
  async function fetchHistory() {
    try { st.history = await scope.api.configHistory(systemId, st.file.spec.id); } catch (e) { st.history = { error: e }; }
  }
  function renderHistory(host) {
    clear(host);
    if (!st.historyOpen) return;
    if (!st.history) { host.append(skeleton(2)); return; }
    if (st.history.error) { host.append(errorBox(st.history.error, async () => { await fetchHistory(); renderHistory(host); })); return; }
    const rows = st.history;
    host.append(card(t('hist.title'), rows.length ? h('div', { class: 'tablewrap' }, h('table', { class: 'compact' },
      h('thead', h('tr', ['hist.when', 'hist.actor', 'hist.sha', 'hist.note', ''].map((k) => h('th', { scope: 'col' }, k ? t(k) : '')))),
      h('tbody', rows.map((r) => h('tr', h('td', { class: 'num' }, fmtDate(r.ts)), h('td', r.actor), h('td', h('code', shortSha(r.sha256)), r.sha256 === st.file.sha256 ? [' ', badge(t('hist.current'), 'info')] : null), h('td', r.note || '–'),
        h('td', { class: 'al-right' }, st.file.spec.editable ? adminBtn(t('hist.rollback'), { size: 'sm', icon: 'history', onClick: () => onRollback(r) }) : null)))))) : emptyBox(t('hist.empty'))));
  }
  async function onRollback(r) {
    const sp = st.file.spec;
    const ok = await confirmEffect({ title: t('hist.confirm_title'), effect: sp.apply_effect || t('cfg.apply_effect_default'), danger: true, confirmLabel: t('hist.rollback'),
      lines: [t('hist.confirm_restore', { id: r.id, sha: shortSha(r.sha256), when: fmtDate(r.ts) }), t('cfg.confirm_backup')] });
    if (!ok) return;
    try {
      const res = await scope.api.rollback(systemId, sp.id, r.id);
      st.outcome = { res, opts: { rollback: true } };
      announce(res.ok ? t('apply.rollback_ok') : t('apply.failed'));
      st.historyOpen = true; await loadFile(sp.id, { keepOutcome: true });
      await fetchHistory(); renderHistory(st.ui.histHost);
    } catch (e) { toastError(e); }
  }

  loadList();
  return root;
}
