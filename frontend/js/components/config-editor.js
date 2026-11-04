// Config file editor. Two views of one draft: Form (a typed control per value, for TOML and YAML) and Text (a
// line-numbered editor). Live edit checks run on every change (cfg-checks.js: syntax, the file's schema where the
// console knows it, Switchyard cross-references, secrets the backend would refuse) and mark the lines and fields they
// concern; the server's validator (Check, Ctrl+S, and always before Apply) stays the authority. Then the existing
// safe write path: diff vs the file on disk, confirm (states apply_effect), Apply with base_sha256, 409 -> reload +
// re-diff, the auto-rolled-back outcome, revision history + rollback. Read-only files show why_readonly.
// Unsaved drafts survive switching files and leaving the page within the session (memory only, never storage).
import { h, clear, icon } from '../dom.js';
import { t } from '../i18n.js';
import { diffLines, diffStats, toUnified, hunkHeader, toSideBySide } from '../diff.js';
import { shortSha, fmtDate } from '../format.js';
import { isAdmin } from '../app-context.js';
import { checkConfig, countBy, sortDiagnostics } from '../cfg-checks.js';
import { badge, btn, adminBtn, card, kv, errorBox, skeleton, emptyBox, sevBadge, select } from './ui.js';
import { confirmEffect } from './dialog.js';
import { toast, toastError } from './toast.js';
import { announce } from './live.js';
import { describeError } from '../errors.js';
import { codeArea } from './code-area.js';
import { configForm } from './config-form.js';

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

// Unsaved drafts by "system/file", kept for the session only (a page reload asks first, see the beforeunload guard).
const drafts = new Map();
const liveDirty = new Set();
let guarded = false;
function guardUnload() {
  if (guarded) return;
  guarded = true;
  window.addEventListener('beforeunload', (e) => { if (drafts.size || [...liveDirty].some((f) => f())) { e.preventDefault(); e.returnValue = ''; } });
}
const VIEW_KEY = 'nc.cfg.view';
const savedView = () => { try { return localStorage.getItem(VIEW_KEY); } catch { return null; } };
const saveView = (v) => { try { localStorage.setItem(VIEW_KEY, v); } catch { /* private mode: no memory, no problem */ } };
const STRUCTURED = new Set(['toml', 'yaml']);
const SOURCE_KEY = { syntax: 'chk.src_syntax', schema: 'chk.src_schema', secret: 'chk.src_secret', lint: 'chk.src_lint', server: 'chk.src_server' };

export function configEditor(scope, systemId) {
  guardUnload();
  const root = h('div', { class: 'stack-v cfg' });
  const st = { specs: [], file: null, draft: '', validation: null, validatedText: null, checking: false, diffMode: 'split', showDiff: true, outcome: null, conflict: null, busy: false,
    history: null, historyOpen: false, view: savedView() === 'text' ? 'text' : 'form', checks: null, restored: null, ui: null };
  const pickerHost = h('div'), bodyHost = h('div', { class: 'stack-v' });
  root.append(pickerHost, bodyHost);

  const key = (id = st.file?.spec.id) => `${systemId}/${id}`;
  const dirty = () => !!st.file && st.draft !== st.file.content;
  const isDirty = () => dirty();
  liveDirty.add(isDirty);
  // The view goes away (another tab or page): keep an unsaved draft for when it comes back.
  const stash = () => { if (!st.file) return; if (dirty()) drafts.set(key(), { draft: st.draft, baseSha: st.file.sha256 }); else drafts.delete(key()); };
  scope.own(() => { stash(); liveDirty.delete(isDirty); });

  // 404 = the file is not on this machine (system not deployed here, or its path not configured): a state, not a failure.
  const loadFailed = (e, retry) => (e.status === 404
    ? emptyBox(t('cfg.missing'), describeError(e), btn(t('common.retry'), { icon: 'restart', onClick: retry }))
    : errorBox(e, retry));
  const editable = () => st.file?.spec.editable && isAdmin();
  const structured = () => STRUCTURED.has(st.file?.spec.format);

  function markPicker() {
    pickerHost.querySelectorAll('button[data-file]').forEach((b) => {
      const id = b.dataset.file, on = id === st.file?.spec.id;
      b.setAttribute('aria-pressed', String(on));
      b.classList.toggle('has-draft', on ? dirty() : drafts.has(key(id)));
    });
  }
  async function loadList() {
    clear(bodyHost).append(skeleton(4));
    try {
      st.specs = await scope.api.configList(systemId);
      if (!scope.alive) return;
      if (!st.specs.length) { clear(bodyHost).append(emptyBox(t('cfg.no_files'))); return; }
      clear(pickerHost);
      if (st.specs.length > 1) {
        pickerHost.append(h('div', { class: 'seg-control', role: 'group', 'aria-label': t('cfg.files') }, st.specs.map((s) => h('button', { type: 'button', 'aria-pressed': 'false', dataset: { file: s.id }, onClick: () => switchFile(s.id) },
          s.label, s.editable ? null : h('span', { class: 'sr-only' }, ` (${t('cfg.readonly')})`), s.editable ? null : icon('lock', 13), h('span', { class: 'draft-dot', title: t('cfg.has_draft') })))));
      }
      await loadFile((st.file?.spec.id) || st.specs[0].id);
    } catch (e) { if (scope.alive) clear(bodyHost).append(loadFailed(e, loadList)); }
  }
  /** Another file of this system: the current draft is kept in memory, not thrown away. */
  async function switchFile(id) {
    if (id === st.file?.spec.id) return;
    stash();
    await loadFile(id);
  }
  const discardOk = () => confirmEffect({ title: t('cfg.discard_title'), effect: t('cfg.discard_effect'), danger: true, confirmLabel: t('cfg.discard') });
  async function loadFile(id, { keepDraft = false, keepOutcome = false } = {}) {
    clear(bodyHost).append(skeleton(4));
    try {
      const f = await scope.api.config(systemId, id);
      if (!scope.alive) return;
      const prevDraft = st.draft, sameFile = st.file?.spec.id === id;
      st.file = f; st.validation = null; st.validatedText = null; st.history = null; st.restored = null;
      st.outcome = keepDraft || keepOutcome ? st.outcome : null;
      if (keepDraft && sameFile) st.draft = prevDraft;
      else if (drafts.has(key(id))) {
        const saved = drafts.get(key(id));
        st.draft = saved.draft;
        st.restored = { changedOnDisk: saved.baseSha !== f.sha256 };
      } else st.draft = f.content;
      drafts.delete(key(id));
      if (!keepDraft) st.conflict = null;
      renderBody();
      markPicker();
    } catch (e) { if (scope.alive) clear(bodyHost).append(loadFailed(e, () => loadFile(id))); }
  }

  const statusEl = () => h('span', { class: 'row gap cfg-status' }, dirty() ? badge(t('cfg.unsaved'), 'warn') : badge(t('cfg.in_sync'), 'muted'), h('span', { class: 'muted small num', title: st.file.sha256 }, t('cfg.sha', { sha: shortSha(st.file.sha256) })));

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
    bodyHost.append(card(sp.label, info));
    if (st.restored) {
      bodyHost.append(h('div', { class: `note ${st.restored.changedOnDisk ? 'warn' : 'info'} row gap wrap between`, role: 'status' },
        h('span', st.restored.changedOnDisk ? t('cfg.restored_changed') : t('cfg.restored')),
        btn(t('cfg.discard'), { size: 'sm', onClick: async () => { if (await discardOk()) { st.draft = st.file.content; st.restored = null; renderBody(); markPicker(); } } })));
    }

    // ---- the two views of the draft
    const viewHost = h('div', { class: 'cfg-view' });
    const code = codeArea({ value: st.draft, label: editable() ? t('cfg.editor_label', { file: sp.label }) : t('cfg.viewer_label', { file: sp.label }), readOnly: !editable(), format: sp.format,
      onInput: (v) => onDraft(v), onSave: () => onValidate(true) });
    const form = structured() ? configForm({ format: sp.format, getText: () => st.draft, setText: (v) => onDraft(v), baseText: () => st.file.content, getSchema: () => (st.checks?.parse.root ? st.checks.schema : null), editable: editable(),
      onGoToLine: (n) => { setView('text'); code.focusLine(n); }, onSave: () => onValidate(true) }) : null;
    let viewSwitch = null;
    if (form) {
      viewSwitch = h('div', { class: 'seg-control', role: 'group', 'aria-label': t('cfg.view') }, ['form', 'text'].map((v) => h('button', { type: 'button', 'aria-pressed': 'false', dataset: { view: v }, onClick: () => { setView(v); saveView(v); } }, t(`cfg.view_${v}`))));
    }
    function setView(v) {
      st.view = form ? v : 'text';
      viewSwitch?.querySelectorAll('button').forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.view === st.view)));
      clear(viewHost);
      if (st.view === 'form') { viewHost.append(form.el); form.build(); form.refresh(merged()); }
      else { viewHost.append(code.el); code.setValue(st.draft); code.setMarkers(merged()); }
    }

    // ---- toolbar
    const controls = h('div', { class: 'row gap wrap cfg-toolbar' });
    const diffStat = h('span', { class: 'diff-stats small cfg-diffstat', 'aria-live': 'polite' });
    let applyBtn = null;
    if (editable()) {
      const modeSel = select([{ value: 'split', label: t('diff.mode_split') }, { value: 'unified', label: t('diff.mode_unified') }, { value: 'off', label: t('diff.mode_off') }], st.showDiff ? st.diffMode : 'off',
        (v) => { st.showDiff = v !== 'off'; if (v !== 'off') st.diffMode = v; refreshDiff(); }, { 'aria-label': t('diff.mode') });
      applyBtn = adminBtn(t('cfg.apply'), { kind: 'primary', icon: 'check', onClick: onApply, disabled: !dirty() });
      controls.append(applyBtn, btn(t('cfg.check'), { icon: 'shield', onClick: () => onValidate(true), title: t('cfg.check_title') }), modeSel,
        adminBtn(t('cfg.reload'), { icon: 'restart', onClick: () => onReload() }), btn(t('cfg.history'), { icon: 'history', onClick: () => toggleHistory() }), diffStat);
    } else controls.append(btn(t('cfg.reload'), { icon: 'restart', onClick: () => onReload() }), btn(t('cfg.history'), { icon: 'history', onClick: () => toggleHistory() }));

    const checksHost = h('div', { class: 'stack-v' });
    const outHost = h('div', { class: 'stack-v', 'aria-live': 'polite' }), diffHost = h('div'), histHost = h('div');
    const contentCard = card(t('cfg.content'), h('div', { class: 'stack-v' }, viewHost, controls), [viewSwitch, statusEl()]);
    bodyHost.append(h('div', { class: 'cfg-main' }, h('div', { class: 'cfg-left stack-v' }, contentCard, outHost), h('aside', { class: 'cfg-side', 'aria-label': t('chk.title') }, checksHost)), diffHost, histHost);
    if (st.conflict) outHost.append(conflictBox());
    if (st.outcome) outHost.append(outcomeBanner(st.outcome.res, st.outcome.opts));

    const refreshStatus = () => {
      const head = contentCard.querySelector('.card-head .cfg-status');
      head?.replaceWith(statusEl());
      if (applyBtn && isAdmin()) applyBtn.disabled = st.busy || !dirty();
      const s = diffStats(diffLines(st.file.content, st.draft));
      clear(diffStat);
      if (s.changed) diffStat.append(h('span', { class: 'add-text' }, `+${s.add}`), ' ', h('span', { class: 'del-text' }, `-${s.del}`), ' ', t('diff.lines'));
      markPicker();
    };
    const refreshDiff = () => {
      clear(diffHost);
      if (!editable() || !st.showDiff || !dirty()) return;
      diffHost.append(card(t('diff.title'), diffView(diffLines(f.content, st.draft), st.diffMode)));
    };
    st.ui = { outHost, histHost, checksHost, code, form, refreshDiff, refreshStatus, setView };

    // Diagnostics with a line number jump to the field (Form) or the line (Text).
    checksHost.addEventListener('click', (e) => {
      const b = e.target.closest('button[data-line]'); if (!b) return;
      const n = Number(b.dataset.line);
      if (st.view === 'form' && form?.focusLine(n)) return;
      setView('text'); code.focusLine(n);
    });

    runChecks();
    // Form is the default for TOML/YAML unless the person picked Text (remembered) or the file cannot be shown as a form.
    setView(form && st.view === 'form' && !st.checks?.parse.unsupported ? 'form' : 'text');
    refreshStatus();
    refreshDiff();
    renderHistory(histHost);
    if (st.historyOpen && !st.history) fetchHistory().then(() => { if (scope.alive) renderHistory(histHost); });
  }

  // ---- live checks
  let timer = null;
  function onDraft(v) {
    st.draft = v;
    st.restored = st.restored && dirty() ? st.restored : null;
    clearTimeout(timer);
    timer = setTimeout(() => { if (!scope.alive || !st.ui) return; runChecks(); st.ui.refreshDiff(); }, 150);
    st.ui?.refreshStatus();
  }
  scope.own(() => clearTimeout(timer));
  const serverFresh = () => !!st.validation && st.validatedText === st.draft;
  const merged = () => sortDiagnostics([...(st.checks?.diagnostics || []),
    ...(serverFresh() ? st.validation.diagnostics.map((d) => ({ severity: d.severity, line: d.line ?? null, message: d.message, source: 'server' })) : [])]);
  function runChecks() {
    const sp = st.file.spec;
    st.checks = checkConfig({ system: systemId, file: sp.id, format: sp.format, text: st.draft });
    const all = merged();
    st.ui.code.setMarkers(all);
    if (st.view === 'form') st.ui.form?.refresh(all);
    renderChecks(all);
  }
  function renderChecks(all) {
    const host = st.ui.checksHost, sp = st.file.spec, c = st.checks, n = countBy(all);
    const parse = c.parse;
    const syntax = !structured() ? badge(t('chk.plain_text'), 'muted')
      : parse.unsupported ? badge(t('chk.syntax_partial'), 'muted', t('chk.syntax_partial_hint', { what: parse.unsupported }))
        : parse.root ? badge(t('chk.syntax_ok'), 'ok') : badge(t('chk.syntax_bad'), 'bad');
    const summary = h('div', { class: 'row gap wrap chk-summary' }, syntax,
      n.error ? badge(t('chk.n_errors', { count: n.error }), 'bad') : null, n.warn ? badge(t('chk.n_warnings', { count: n.warn }), 'warn') : null,
      !n.error && !n.warn ? badge(t('chk.clean'), 'ok') : null);
    const about = h('p', { class: 'hint' }, c.schema ? t(parse.root ? 'chk.schema_known' : 'chk.schema_paused', { name: t(`chk.schema_${c.schema.id}`) }) : structured() ? t('chk.schema_none') : t('chk.text_only'),
      parse.unsupported ? ` ${t('chk.syntax_partial_hint', { what: parse.unsupported })}` : '');
    const items = all.slice(0, 60);
    const list = items.length ? h('ul', { class: 'diags chk-list' }, items.map((d) => h('li', { class: `chk ${d.severity}` },
      h('div', { class: 'row gap' }, sevBadge(d.severity), d.line ? h('button', { type: 'button', class: 'link', dataset: { line: String(d.line) } }, t('val.line', { n: d.line })) : null,
        h('span', { class: 'chk-src muted small' }, t(SOURCE_KEY[d.source] || 'chk.src_lint'))),
      h('p', { class: 'chk-msg' }, d.message)))) : h('p', { class: 'note ok' }, t('chk.none'));
    const more = all.length > items.length ? h('p', { class: 'muted small' }, t('chk.more', { count: all.length - items.length })) : null;
    clear(host).append(card(t('chk.title'), h('div', { class: 'stack-v' }, summary, about, list, more, serverBlock())));
  }
  function serverBlock() {
    const v = st.validation, sp = st.file.spec;
    const head = h('div', { class: 'row gap wrap between' }, h('strong', t('chk.server')), sp.validators.length ? h('span', { class: 'muted small' }, sp.validators.join(', ')) : null);
    let state;
    if (!isAdmin()) state = null;
    else if (st.checking) state = skeleton(1);
    else if (!v) state = h('p', { class: 'hint' }, t('chk.server_never'));
    else {
      const fresh = serverFresh();
      state = h('div', { class: 'stack-v' },
        h('div', { class: 'row gap wrap' }, badge(v.ok ? t('val.ok') : t('val.failed'), fresh ? (v.ok ? 'ok' : 'bad') : 'muted'), fresh ? null : badge(t('chk.server_stale'), 'warn'),
          v.authoritative ? null : badge(t('val.not_authoritative'), 'warn')),
        fresh ? null : h('p', { class: 'hint' }, t('chk.server_stale_hint')),
        v.authoritative ? null : h('p', { class: 'note warn small' }, h('strong', t('val.caveat_title')), ' ', t('val.caveat'), v.note ? ` ${v.note}` : ''),
        v.authoritative && v.note ? h('p', { class: 'hint' }, v.note) : null);
    }
    const action = isAdmin() ? btn(serverFresh() ? t('chk.server_again') : t('cfg.check'), { size: 'sm', icon: 'shield', onClick: () => onValidate(true), disabled: st.checking })
      : h('p', { class: 'hint' }, t('chk.server_admin'));
    return h('div', { class: 'stack-v chk-server' }, head, state, action);
  }

  function conflictBox() {
    return h('div', { class: 'note bad conflict', role: 'alert' }, h('strong', t('conflict.title')), h('p', t('conflict.body')),
      h('div', { class: 'row gap wrap' },
        btn(t('conflict.reload_rediff'), { kind: 'primary', icon: 'restart', onClick: async () => { st.conflict = null; await loadFile(st.file.spec.id, { keepDraft: true }); announce(t('conflict.rediffed')); toast(t('conflict.rediffed'), { kind: 'info' }); } }),
        btn(t('conflict.discard'), { onClick: async () => { st.conflict = null; st.draft = ''; drafts.delete(key()); await loadFile(st.file.spec.id); } })));
  }

  async function onReload() {
    if (dirty() && !(await discardOk())) return;
    st.conflict = null; st.draft = st.file.content; drafts.delete(key());
    await loadFile(st.file.spec.id);
  }

  /** The server's validator on the current draft. Viewers cannot call it (the route is admin-only). */
  async function onValidate(announceIt) {
    if (!isAdmin() || !st.file) return null;
    const text = st.draft;
    st.checking = true; renderChecks(merged());
    try {
      const v = await scope.api.validate(systemId, st.file.spec.id, text);
      if (!scope.alive) return null;
      st.validation = v; st.validatedText = text;
      const errs = v.diagnostics.filter((d) => d.severity === 'error').length;
      if (announceIt) announce(v.ok ? t('val.ok') : t('val.failed_n', { n: errs }));
      return v;
    } catch (e) { toastError(e); return null; }
    finally { st.checking = false; if (scope.alive && st.ui) runChecks(); }
  }

  async function onApply() {
    if (!dirty()) { toast(t('cfg.no_changes'), { kind: 'info' }); return; }
    const { outHost } = st.ui;
    const v = await onValidate(false);
    if (!v) return;
    if (v.diagnostics.some((d) => d.severity === 'error') || !v.ok) { toast(t('cfg.fix_first'), { kind: 'bad' }); announce(t('cfg.fix_first')); return; }
    const sp = st.file.spec, stats = diffStats(diffLines(st.file.content, st.draft));
    const local = countBy(st.checks?.diagnostics || []);
    const restartBox = h('input', { type: 'checkbox', checked: true, id: 'restart-after' });
    const lines = [t('cfg.confirm_changes', { add: stats.add, del: stats.del }), t('cfg.confirm_base', { sha: shortSha(st.file.sha256), path: sp.display_path }), t('cfg.confirm_backup'),
      local.error || local.warn ? t('cfg.confirm_local', { errors: local.error, warnings: local.warn }) : null,
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
    } finally { st.busy = false; st.ui?.refreshStatus(); }
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
      st.historyOpen = true; st.draft = st.file.content; drafts.delete(key()); await loadFile(sp.id, { keepOutcome: true });
      await fetchHistory(); renderHistory(st.ui.histHost);
    } catch (e) { toastError(e); }
  }

  loadList();
  return root;
}
