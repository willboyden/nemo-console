// Form view of a TOML / YAML config: one section per table or mapping, one typed control per value. Editing a control
// rewrites only that value's characters in the draft (cfg-edit.js), so comments and layout survive and the Text view
// and the diff show exactly what changed. A value that does not fit its type is shown as an error and never written.
// Problems from the live checks (and the last server check) are shown on the field or section they belong to.
// Every field (and every section the catalog knows) has an info button: a pop-up says what the key is, what changing
// it does, what it accepts and when a change takes effect (i18n/fields.en.json via cfg-help.js).
import { h, clear, icon } from '../dom.js';
import { t } from '../i18n.js';
import { parseConfig, entryOf } from '../cfg-parse.js';
import { lookupHelp, flagFor, envFor } from '../cfg-help.js';
import { richRuns } from '../help.js';
import { encodeValue, spliceValue, spliceBlock, displayValue, flattenFields, pathKey } from '../cfg-edit.js';
import { specAt, describeSpec, choicesFor } from '../cfg-checks.js';
import { btn, emptyBox, sevBadge } from './ui.js';

let uid = 0;
const SEV = { error: 0, warn: 1, info: 2 };
const worst = (ds) => ds.reduce((w, d) => (SEV[d.severity] < SEV[w] ? d.severity : w), 'info');

/**
 * The comment on a value's line (`x = 1  # why`), else a SHORT comment block right above its key (at most `max`
 * lines: a longer block is a file or section header, not about this one key, and stays in the Text view).
 */
function commentFor(lines, line, endCol, max = 2) {
  const own = lines[line - 1] ?? '';
  const m = /^[^#]*?\s#\s?(.*)$/.exec(own.slice(endCol));
  if (m && m[1].trim()) return m[1].trim();
  const above = [];
  for (let i = line - 2; i >= 0; i--) {
    const s = lines[i].trim();
    if (!s.startsWith('#')) break;
    if (i === 0) return ''; // a block that starts the file is the file's header, not about this key
    above.unshift(s.replace(/^#+\s?/, ''));
    if (above.length > max) return '';
  }
  const text = above.filter((s) => s && !/^[─━=\-#\s]+$/.test(s)).join(' ');
  return text.length > 220 ? `${text.slice(0, 217)}…` : text;
}

/** Sections: every mapping (and list of mappings) that holds values, in document order, with a breadcrumb title. */
function sectionsOf(root) {
  const out = [];
  const walk = (node, path, line) => {
    const fields = [],
      kids = [];
    if (node.kind === 'map') {
      for (const e of node.entries) {
        if (entryOf(node, e.key) !== e) continue; // a duplicate key that a later one overrides: the loader ignores it
        if (e.node.kind === 'scalar') fields.push({ path: [...path, e.key], label: e.key, node: e.node });
        else if (e.node.kind === 'seq' && e.node.items.every((x) => x.kind === 'scalar'))
          e.node.items.forEach((x, i) => fields.push({ path: [...path, e.key, i], label: `${e.key} [${i + 1}]`, node: x }));
        else kids.push([e.node, [...path, e.key], e.line]);
      }
    } else node.items.forEach((x, i) => (x.kind === 'scalar' ? fields.push({ path: [...path, i], label: `[${i + 1}]`, node: x }) : kids.push([x, [...path, i], x.line])));
    if (fields.length || (node.kind === 'map' && !node.entries.length)) out.push({ path, line: line ?? node.line, fields, empty: !fields.length && !kids.length });
    for (const k of kids) walk(...k);
  };
  walk(root, [], 1);
  return out;
}
const crumb = (path) => path.map((p) => (typeof p === 'number' ? `#${p + 1}` : p));

/**
 * @param o.format   'toml' | 'yaml'
 * @param o.getText / o.setText  the draft
 * @param o.baseText  the file on disk (for "changed" and Revert)
 * @param o.getSchema() the file's schema from the live checks (cfg-checks.js), or null
 * @param o.editable  false = every control is read-only
 * @param o.onGoToLine(line)  open the Text view at a line
 * @param o.onSave()  Ctrl/Cmd+S
 * @param o.getHelp() { cat: catalogsFor(...) or null, params: the system's Params (for command-line flags) or null,
 *                    file: the file's spec (editable, why_readonly, apply_effect), admin: may the viewer edit }
 */
export function configForm(o) {
  const root = h('div', { class: 'cfg-form stack-v' });
  let fields = new Map(),
    sections = [],
    filter = { q: '', only: 'all' },
    diags = [],
    base = new Map(),
    doc = null;
  const lines = () => o.getText().split('\n');

  function build() {
    closePop(false);
    clear(root);
    const res = parseConfig(o.format, o.getText());
    if (res.unsupported) {
      root.append(emptyBox(t('form.unsupported'), t('form.unsupported_hint', { what: res.unsupported })));
      return false;
    }
    if (!res.root) {
      const e = res.errors[0];
      root.append(
        emptyBox(t('form.syntax'), t('form.syntax_hint', { line: e.line, message: e.message }), btn(t('form.go_line', { n: e.line }), { kind: 'primary', onClick: () => o.onGoToLine(e.line) })),
      );
      return false;
    }
    doc = res.root;
    const baseRes = parseConfig(o.format, o.baseText());
    base = new Map(baseRes.root ? flattenFields(baseRes.root).map((f) => [pathKey(f.path), f.node]) : []);
    fields = new Map();
    sections = sectionsOf(res.root);
    const search = h('input', {
      type: 'search',
      class: 'form-search',
      placeholder: t('form.search_ph'),
      'aria-label': t('form.search'),
      value: filter.q,
      autocomplete: 'off',
      onInput: (e) => {
        filter.q = e.target.value;
        applyFilter();
      },
    });
    const seg = h(
      'div',
      { class: 'seg-control', role: 'group', 'aria-label': t('form.show') },
      ['all', 'changed', 'problems'].map((k) =>
        h(
          'button',
          {
            type: 'button',
            'aria-pressed': String(filter.only === k),
            dataset: { only: k },
            onClick: (e) => {
              filter.only = k;
              seg.querySelectorAll('button').forEach((b) => b.setAttribute('aria-pressed', String(b === e.currentTarget)));
              applyFilter();
            },
          },
          t(`form.only_${k}`),
        ),
      ),
    );
    const count = h('span', { class: 'muted small', role: 'status', 'aria-live': 'polite' });
    const body = h('div', { class: 'form-sections' });
    const ls = lines();
    for (const s of sections) body.append(sectionEl(s, ls));
    const none = h('p', { class: 'note info form-none', hidden: true }, t('form.no_match'));
    root.append(h('div', { class: 'row gap wrap between form-bar' }, h('div', { class: 'row gap wrap' }, search, seg), count), body, none, pop);
    root._count = count;
    root._none = none;
    refresh(diags);
    return true;
  }

  function sectionEl(s, ls) {
    const title = s.path.length ? crumb(s.path) : [t('form.top')];
    const note = s.path.length ? commentFor(ls, s.line, 1e9, 3) : '';
    const msg = h('div', { class: 'ff-msg' });
    const grid = h('div', { class: 'ff-grid' });
    for (const f of s.fields) grid.append(fieldEl(f, ls));
    const head = h(
      'summary',
      { class: 'sec-head' },
      h(
        'span',
        { class: 'sec-title' },
        title.map((c, i) => [i ? h('span', { class: 'crumb-sep', 'aria-hidden': 'true' }, '›') : null, h('span', { class: i === title.length - 1 ? 'crumb last' : 'crumb' }, String(c))]),
      ),
      h('span', { class: 'sec-meta muted small' }, s.fields.length ? t('form.n_values', { count: s.fields.length }) : ''),
      h('span', { class: 'sec-flag' }),
    );
    const about =
      s.path.length && lookupHelp(o.getHelp?.()?.cat, s.path)
        ? h(
            'button',
            { type: 'button', class: 'ff-info sec-info', 'aria-haspopup': 'dialog', 'aria-expanded': 'false', onClick: (e) => togglePop(e.currentTarget, { section: s }) },
            icon('info', 15),
            h('span', t('form.info_section')),
          )
        : null;
    const top = note || about ? h('div', { class: 'sec-about' }, note ? h('p', { class: 'hint sec-note' }, note) : h('span'), about) : null;
    const el = h('details', { class: 'sec', open: true, dataset: { path: pathKey(s.path) } }, head, top, msg, s.empty ? h('p', { class: 'muted small' }, t('form.empty_section')) : grid);
    s.el = el;
    s.msg = msg;
    s.flag = head.lastChild;
    return el;
  }

  function fieldEl(f, ls) {
    const schema = o.getSchema?.(),
      id = `ff${++uid}`,
      k = pathKey(f.path),
      spec = schema ? specAt(schema, f.path) : null;
    const n = f.node;
    const ro = !o.editable || (n.style === 'block' && n.block.empty);
    const choices = choicesFor(spec, doc);
    const msg = h('div', { class: 'ff-msg', id: `${id}-msg` });
    const comment = n.style === 'block' || n.style?.startsWith('ml-') ? '' : commentFor(ls, n.line, n.end - (o.getText().lastIndexOf('\n', n.start - 1) + 1));
    const described = describeSpec(spec);
    const hint = [described, comment && !described.toLowerCase().includes(comment.toLowerCase()) ? comment : ''].filter(Boolean).join(' · ');
    let ctl;
    const isBool = n.type === 'bool' || (n.type === 'null' && spec?.t === 'bool');
    if (isBool) {
      ctl = h('input', {
        type: 'checkbox',
        id,
        role: 'switch',
        checked: n.value === true,
        disabled: ro,
        'aria-describedby': `${id}-msg ${id}-hint`,
        onChange: (e) => {
          write(k, e.target.checked);
          e.target.nextSibling.textContent = String(e.target.checked);
        },
      });
      ctl = h('label', { class: 'switch' }, ctl, h('span', { class: 'mono small' }, String(n.value === true)));
    } else if (choices) {
      const cur = displayValue(n);
      const opts = choices.includes(cur) ? choices : [cur, ...choices];
      ctl = h(
        'select',
        { id, disabled: ro, 'aria-describedby': `${id}-msg ${id}-hint`, onChange: (e) => write(k, e.target.value) },
        opts.map((c) => h('option', { value: c, selected: c === cur }, c === cur && !choices.includes(c) ? t('form.not_allowed', { v: c || t('form.empty') }) : c)),
      );
    } else {
      const v = displayValue(n);
      const multi = n.style === 'block' || v.includes('\n') || (n.type === 'string' && v.length > 90);
      ctl = h(multi ? 'textarea' : 'input', {
        id,
        type: multi ? null : 'text',
        class: multi ? 'ff-text' : null,
        rows: multi ? Math.min(12, Math.max(2, v.split('\n').length + 1)) : null,
        value: v,
        readonly: ro ? true : null,
        spellcheck: 'false',
        inputmode: n.type === 'int' || n.type === 'float' ? 'decimal' : null,
        placeholder: n.type === 'null' ? t('form.empty') : null,
        'aria-describedby': `${id}-msg ${id}-hint`,
        onInput: (e) => write(k, e.target.value, e.target),
      });
    }
    const typeTag = n.type === 'null' ? t('form.type_null') : t(`form.type_${n.type}`);
    const revert = o.editable ? btn('', { icon: 'history', size: 'sm', kind: 'ghost', title: t('form.revert'), onClick: () => revertField(k) }) : null;
    const info = h(
      'button',
      {
        type: 'button',
        class: 'ff-info',
        'aria-label': t('form.info', { key: crumb(f.path).join(' › ') }),
        title: t('form.info', { key: String(f.label) }),
        'aria-haspopup': 'dialog',
        'aria-expanded': 'false',
        onClick: (e) => togglePop(e.currentTarget, { key: k }),
      },
      icon('info', 15),
    );
    const row = h(
      'div',
      { class: 'ff', dataset: { path: k } },
      h(
        'div',
        { class: 'ff-head' },
        h('label', { class: 'ff-key', for: id }, h('code', String(f.label)), h('span', { class: 'ff-dot', title: t('form.changed') }), h('span', { class: 'ff-type' }, typeTag)),
        info,
      ),
      h('div', { class: 'ff-ctl' }, ctl, revert),
      hint ? h('div', { class: 'hint ff-hint', id: `${id}-hint` }, hint) : h('span', { id: `${id}-hint`, hidden: true }),
      msg,
      h('div', { class: 'ff-local', role: 'alert' }),
    );
    fields.set(k, { ...f, spec, row, ctl, msg, local: row.lastChild, revert, info, comment, label: String(f.label), hay: `${crumb(f.path).join(' ')} ${displayValue(n)}`.toLowerCase() });
    return row;
  }

  /** After any write: parse the new draft and point every field at its node again (offsets moved). */
  function remap(text) {
    const res = parseConfig(o.format, text);
    if (!res.root) return false;
    doc = res.root;
    const now = new Map(flattenFields(res.root).map((x) => [pathKey(x.path), x.node]));
    if (now.size !== fields.size) return false;
    for (const [k, f] of fields) {
      const nn = now.get(k);
      if (!nn) return false;
      f.node = nn;
    }
    return true;
  }

  function write(k, input, inputEl) {
    const f = fields.get(k);
    if (!f) return;
    const enc = encodeValue(o.format, f.node, input, f.spec);
    f.local.textContent = '';
    if (enc.error) {
      f.local.append(icon('warn', 14), ' ', t('form.not_written', { why: enc.error }));
      (inputEl || f.ctl).setAttribute?.('aria-invalid', 'true');
      f.row.classList.add('bad-input');
      return;
    }
    (inputEl || f.ctl).removeAttribute?.('aria-invalid');
    f.row.classList.remove('bad-input');
    const text = o.getText();
    const next = enc.block !== undefined ? spliceBlock(text, f.node, enc.block) : spliceValue(text, f.node, enc.raw);
    if (next === text) return;
    o.setText(next);
    if (!remap(next)) build();
    else markChanged();
  }

  function revertField(k) {
    const f = fields.get(k),
      b = base.get(k);
    if (!f || !b) return;
    const text = o.getText();
    const next = f.node.style === 'block' && b.style === 'block' ? spliceBlock(text, f.node, b.block.lines.join('\n')) : spliceValue(text, f.node, b.raw);
    if (next === text) return;
    o.setText(next);
    build(); // the control must show the old value again
  }

  const isChanged = (k, f) => {
    const b = base.get(k);
    return !b || b.raw !== f.node.raw;
  };
  function markChanged() {
    let changed = 0;
    for (const [k, f] of fields) {
      const c = isChanged(k, f);
      f.row.classList.toggle('changed', c);
      if (f.revert) f.revert.hidden = !c;
      if (c) changed++;
    }
    root._changed = changed;
    applyFilter();
  }

  /** Show the live checks (and fresh server diagnostics) on their fields and sections. */
  function refresh(ds) {
    diags = ds || [];
    if (!fields.size && !sections.length) return;
    const byPath = new Map(),
      byLine = new Map();
    for (const d of diags) {
      if (d.path) (byPath.get(pathKey(d.path)) || byPath.set(pathKey(d.path), []).get(pathKey(d.path))).push(d);
      else if (d.line) (byLine.get(d.line) || byLine.set(d.line, []).get(d.line)).push(d);
    }
    const used = new Set();
    for (const [k, f] of fields) {
      const mine = [...(byPath.get(k) || []), ...(byLine.get(f.node.line) || [])];
      mine.forEach((d) => used.add(d));
      clear(f.msg);
      for (const d of mine) f.msg.append(h('p', { class: `ff-problem ${d.severity}` }, sevBadge(d.severity), ' ', d.short ?? d.message));
      f.row.classList.toggle('has-problem', mine.length > 0);
      f.row.dataset.sev = mine.length ? worst(mine) : '';
      if (mine.some((d) => d.severity === 'error')) f.ctl.setAttribute?.('aria-invalid', 'true');
      else if (!f.row.classList.contains('bad-input')) f.ctl.removeAttribute?.('aria-invalid');
    }
    // A problem with no field of its own (a missing key, an unknown block, a server line on a table header) lands on
    // the deepest section that contains its path, or on the section that starts on its line.
    const secDiags = new Map(sections.map((s) => [s, []]));
    const secFor = (p) => sections.reduce((best, s) => (s.path.length <= p.length && s.path.every((x, i) => x === p[i]) && (!best || s.path.length > best.path.length) ? s : best), null);
    for (const d of diags) {
      if (used.has(d)) continue;
      const s = d.path ? secFor(d.path) : d.line ? sections.find((x) => x.line === d.line) : null;
      if (s) {
        secDiags.get(s).push(d);
        used.add(d);
      }
    }
    for (const s of sections) {
      const mine = secDiags.get(s);
      clear(s.msg);
      for (const d of mine) s.msg.append(h('p', { class: `ff-problem ${d.severity}` }, sevBadge(d.severity), ' ', pathKey(d.path || []) === pathKey(s.path) ? (d.short ?? d.message) : d.message));
      const inside = [...s.el.querySelectorAll('.ff.has-problem')].map((r) => r.dataset.sev);
      const all = [...mine.map((d) => d.severity), ...inside.map((x) => x)];
      clear(s.flag);
      if (all.length) s.flag.append(sevBadge(all.includes('error') ? 'error' : all.includes('warn') ? 'warn' : 'info'));
      s.el.classList.toggle('has-problem', all.length > 0);
    }
    markChanged();
  }

  function applyFilter() {
    if (!root._count) return;
    const q = filter.q.trim().toLowerCase().split(/\s+/).filter(Boolean);
    let shown = 0;
    for (const [, f] of fields) {
      const okQ = q.every((w) => f.hay.includes(w));
      const okO =
        filter.only === 'all' ||
        (filter.only === 'changed' && f.row.classList.contains('changed')) ||
        (filter.only === 'problems' && (f.row.classList.contains('has-problem') || f.row.classList.contains('bad-input')));
      f.row.hidden = !(okQ && okO);
      if (!f.row.hidden) shown++;
    }
    for (const s of sections) {
      const anyField = [...s.el.querySelectorAll('.ff')].some((r) => !r.hidden);
      const selfHit = q.length && q.every((w) => crumb(s.path).join(' ').toLowerCase().includes(w));
      const selfProblem = filter.only === 'problems' && s.msg.childElementCount > 0;
      s.el.hidden = !(anyField || (filter.only === 'all' && (selfHit || !q.length) && !s.fields.length) || selfProblem);
      if ((q.length || filter.only !== 'all') && !s.el.hidden) s.el.open = true;
    }
    root._none.hidden = shown > 0 || sections.some((s) => !s.el.hidden);
    root._count.textContent = t('form.count', { shown, total: fields.size, changed: root._changed || 0 });
  }

  /** Scroll to and focus the control of the field at line n (a diagnostic link while the Form is shown). */
  function focusLine(line) {
    for (const [, f] of fields)
      if (f.node.line === line) {
        f.row.hidden = false;
        f.row.closest('details').open = true;
        f.row.scrollIntoView({ block: 'center' });
        (f.ctl.querySelector?.('input') || f.ctl).focus();
        return true;
      }
    for (const s of sections)
      if (s.line === line) {
        s.el.hidden = false;
        s.el.open = true;
        s.el.scrollIntoView({ block: 'start' });
        s.el.querySelector('summary').focus();
        return true;
      }
    return false;
  }

  // ---- the info pop-up: one per form, filled for the button that opened it. A manual popover (top layer) where the
  // browser has the Popover API, else a fixed box; both close on Esc, on a click outside, and on the same button again.
  const popId = `ffpop${++uid}`;
  const pop = h('div', { class: 'ff-pop', id: popId, role: 'dialog', 'aria-labelledby': `${popId}-t`, tabindex: '-1', hidden: true });
  const hasPopover = typeof pop.showPopover === 'function';
  if (hasPopover) {
    pop.setAttribute('popover', 'manual');
    pop.hidden = false;
  }
  let popFor = null; // the button the pop-up belongs to
  const isOpen = () => (hasPopover ? pop.matches(':popover-open') : !pop.hidden);
  const rich = (text) =>
    richRuns(text).map((runs) =>
      h(
        'p',
        runs.map((r) => (r.code ? h('code', r.text) : r.text)),
      ),
    );
  const block = (title, ...body) => h('section', { class: 'fh-block' }, h('h4', title), ...body);

  function place() {
    if (!popFor || !isOpen()) return;
    const a = popFor.getBoundingClientRect(),
      vw = document.documentElement.clientWidth,
      vh = window.innerHeight,
      m = 8;
    const w = Math.min(420, vw - 2 * m);
    pop.style.width = `${w}px`;
    pop.style.maxHeight = `${Math.max(160, vh - 2 * m)}px`;
    const left = Math.min(Math.max(m, a.right - w), vw - w - m);
    const ph = pop.offsetHeight;
    const below = a.bottom + 6,
      above = a.top - 6 - ph;
    const top = below + ph <= vh - m || above < m ? Math.min(below, Math.max(m, vh - m - ph)) : above;
    pop.style.left = `${left}px`;
    pop.style.top = `${top}px`;
  }
  const onKey = (e) => {
    if (!root.isConnected) {
      closePop(false);
      return;
    }
    if (e.key === 'Escape' && isOpen()) {
      e.preventDefault();
      e.stopPropagation();
      closePop(true);
    }
  };
  const onDown = (e) => {
    if (!root.isConnected) {
      closePop(false);
      return;
    }
    if (isOpen() && !pop.contains(e.target) && e.target !== popFor && !popFor?.contains(e.target)) closePop(false);
  };
  function closePop(refocus) {
    if (!popFor) return;
    const b = popFor;
    popFor = null;
    b.setAttribute('aria-expanded', 'false');
    if (hasPopover) {
      if (pop.matches(':popover-open')) pop.hidePopover();
    } else pop.hidden = true;
    document.removeEventListener('keydown', onKey, true);
    document.removeEventListener('pointerdown', onDown, true);
    window.removeEventListener('resize', place);
    window.removeEventListener('scroll', place, true);
    if (refocus || pop.contains(document.activeElement) || document.activeElement === document.body) b.focus();
  }
  function togglePop(btnEl, what) {
    if (popFor === btnEl) {
      closePop(true);
      return;
    }
    closePop(false);
    fillPop(what);
    popFor = btnEl;
    btnEl.setAttribute('aria-expanded', 'true');
    btnEl.setAttribute('aria-controls', popId);
    if (hasPopover) pop.showPopover();
    else pop.hidden = false;
    place();
    document.addEventListener('keydown', onKey, true);
    document.addEventListener('pointerdown', onDown, true);
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    pop.focus({ preventScroll: true });
  }

  /** The pop-up's content for a field ({key}) or a section ({section}). */
  function fillPop(what) {
    clear(pop);
    const hp = o.getHelp?.() || {};
    const f = what.key ? fields.get(what.key) : null;
    const path = f ? f.path : what.section.path;
    const entry = lookupHelp(hp.cat, path);
    const title = h('h3', { id: `${popId}-t`, class: 'fh-title' }, h('code', crumb(path).join(' › ')));
    pop.append(h('div', { class: 'fh-head' }, title, btn('', { icon: 'close', size: 'sm', kind: 'ghost', title: t('fh.close'), onClick: () => closePop(true) })));
    const bodyEl = h('div', { class: 'fh-body' });
    pop.append(bodyEl);
    if (hp.loading) bodyEl.append(h('p', { class: 'muted' }, t('fh.loading')));
    else if (hp.failed) bodyEl.append(h('p', { class: 'note warn' }, t('fh.unavailable')));
    if (entry?.what) bodyEl.append(block(t('fh.what'), ...rich(entry.what)));
    if (entry?.change) bodyEl.append(block(t('fh.change'), ...rich(entry.change)));
    if (f) {
      const fl = !hp.params ? null : typeof path.at(-1) === 'number' ? flagFor(hp.params, siblings(path), path.at(-1)) : path.at(-2) === 'environment' ? envFor(hp.params, path.at(-1)) : null;
      if (fl) {
        const p = fl.param,
          extra = [];
        if (p.min != null && p.max != null) extra.push(t('fh.flag_range', { min: p.min, max: p.max }));
        if (p.default != null && p.default !== '') extra.push(t('fh.flag_default', { v: typeof p.default === 'object' ? JSON.stringify(p.default) : p.default }));
        bodyEl.append(
          block(
            t('fh.flag', { flag: fl.flag }),
            h('p', h('strong', p.label), ' — ', p.help),
            fl.isValue ? h('p', { class: 'muted small' }, t('fh.flag_value', { flag: fl.flag })) : null,
            extra.length ? h('p', { class: 'muted small' }, extra.join(' ')) : null,
          ),
        );
      }
      if (!entry && !fl && !hp.loading && !hp.failed) bodyEl.append(h('p', { class: 'note info' }, t('fh.missing')));
      const accepts = describeSpec(f.spec) || choicesFor(f.spec, doc)?.join(', ') || (f.node.type === 'null' ? t('form.type_null') : t(`form.type_${f.node.type}`));
      const disk = base.get(what.key);
      const now = displayValue(f.node),
        was = disk ? displayValue(disk) : null;
      bodyEl.append(block(t('fh.accepts'), h('p', accepts), disk && was !== now ? h('p', { class: 'muted small' }, t('fh.now', { v: short(now), disk: short(was) })) : null));
      const said = `${accepts} ${entry?.what || ''} ${entry?.change || ''}`.toLowerCase();
      if (f.comment && !said.includes(f.comment.toLowerCase())) bodyEl.append(block(t('fh.comment'), h('p', { class: 'mono small' }, f.comment)));
    }
    const file = hp.file;
    if (file) {
      if (!file.editable) bodyEl.append(block(t('fh.readonly'), h('p', file.why_readonly || '')));
      else {
        if (file.apply_effect) bodyEl.append(block(t('fh.effect'), h('p', file.apply_effect)));
        if (hp.admin === false) bodyEl.append(h('p', { class: 'muted small' }, t('fh.viewer')));
      }
    }
  }
  const short = (v) => {
    const s = String(v ?? '').replace(/\s+/g, ' ');
    return s === '' ? t('form.empty') : s.length > 60 ? `${s.slice(0, 57)}…` : s;
  };
  /** The scalar values of the list a field sits in (a command line), for flag lookups. */
  function siblings(path) {
    let n = doc;
    for (const p of path.slice(0, -1)) {
      n = typeof p === 'number' ? n?.items?.[p] : n?.kind === 'map' ? entryOf(n, p)?.node : null;
      if (!n) return [];
    }
    return n?.kind === 'seq' ? n.items.map((x) => (x.kind === 'scalar' ? displayValue(x) : null)) : [];
  }

  root.addEventListener('keydown', (e) => {
    if ((e.ctrlKey || e.metaKey) && (e.key === 's' || e.key === 'S')) {
      e.preventDefault();
      o.onSave?.();
    }
  });
  return { el: root, build, refresh, focusLine, closePop: () => closePop(false) };
}
