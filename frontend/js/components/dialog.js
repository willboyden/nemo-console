// Native <dialog> gives us modality, focus trapping and Esc-to-close for free (good a11y baseline).
import { h, icon } from '../dom.js';
import { t } from '../i18n.js';

function make(cls, title, body, { onClose } = {}) {
  const opener = document.activeElement;
  const dlg = h('dialog', { class: cls, 'aria-labelledby': 'dlg-title' });
  if (cls === 'modal') dlg.setAttribute('role', 'alertdialog');
  const head = h('header', { class: 'dlg-head' }, h('h2', { id: 'dlg-title' }, title),
    h('button', { type: 'button', class: 'btn ghost sm', 'aria-label': t('common.close'), onClick: () => dlg.close() }, icon('close', 16)));
  const content = h('div', { class: 'dlg-body' }, body);
  dlg.append(head, content);
  dlg.addEventListener('close', () => { dlg.remove(); onClose?.(); opener?.focus?.(); });
  dlg.addEventListener('click', (e) => { if (e.target === dlg) dlg.close(); });
  document.body.append(dlg);
  dlg.showModal();
  return { el: dlg, body: content, close: () => dlg.close(), setTitle: (s) => { head.firstChild.textContent = s; } };
}
export const openDrawer = (title, body, o) => make('drawer', title, body, o);
export const openModal = (title, body, o) => make('modal', title, body, o);

/**
 * Confirm dialog for any disruptive/destructive control. It restates the exact effect (never just "Are you sure?").
 * opts: {title, effect, lines:[string], danger, confirmLabel, extra:Node}. Resolves true only on the confirm button.
 * The Cancel button is focused first so Enter/Space cannot confirm by accident.
 */
export function confirmEffect({ title, effect, lines = [], danger = false, confirmLabel = t('common.confirm'), extra = null }) {
  return new Promise((resolve) => {
    let result = false;
    const cancel = h('button', { type: 'button', class: 'btn', onClick: () => m.close() }, t('common.cancel'));
    const m = openModal(title || t('common.confirm'), h('div', { class: 'stack-v' },
      h('p', { class: `note ${danger ? 'bad' : 'warn'}`, role: 'note' }, h('strong', t('confirm.effect')), ' ', effect),
      lines.length ? h('ul', { class: 'effect-list' }, lines.map((l) => h('li', l))) : null,
      extra,
      h('div', { class: 'row gap end' }, cancel,
        h('button', { type: 'button', class: `btn ${danger ? 'danger' : 'primary'}`, onClick: () => { result = true; m.close(); } }, confirmLabel))),
    { onClose: () => resolve(result) });
    cancel.focus();
  });
}
