import { h, icon } from '../dom.js';
import { describeError } from '../errors.js';
import { t } from '../i18n.js';
export function toast(message, { kind = 'info', timeout = 4500 } = {}) {
  window.dispatchEvent(new CustomEvent('nc:toast', { detail: { message, kind, timeout } }));
}
export const toastError = (e, prefix) => toast(`${prefix ? prefix + ': ' : ''}${describeError(e)}`, { kind: 'bad', timeout: 8000 });

class NcToasts extends HTMLElement {
  connectedCallback() {
    // role=status + aria-live so screen readers announce toasts without stealing focus.
    this.setAttribute('role', 'status'); this.setAttribute('aria-live', 'polite');
    this._on = (e) => this.push(e.detail);
    window.addEventListener('nc:toast', this._on);
  }
  disconnectedCallback() { window.removeEventListener('nc:toast', this._on); }
  push({ message, kind, timeout }) {
    const close = h('button', { type: 'button', class: 'btn ghost sm', 'aria-label': t('common.dismiss'), onClick: () => el.remove() }, icon('close', 14));
    const el = h('div', { class: `toast ${kind}` }, h('span', message), close);
    this.append(el);
    if (timeout) setTimeout(() => el.remove(), timeout);
  }
}
customElements.define('nc-toasts', NcToasts);
