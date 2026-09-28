// Base class for view web components (light DOM so global CSS/tokens apply) and Scope, a disposable owner of
// timers/streams/listeners used per view and per tab panel.
import { stream, api } from '../app-context.js';
import { clear } from '../dom.js';
import { errorBox, skeleton } from './ui.js';

export class Scope {
  constructor() { this._cleanups = []; this.alive = true; }
  own(fn) { this._cleanups.push(fn); return fn; }
  every(ms, fn) { const id = setInterval(fn, ms); this.own(() => clearInterval(id)); return id; }
  listen(target, ev, fn, o) { target.addEventListener(ev, fn, o); this.own(() => target.removeEventListener(ev, fn, o)); }
  stream(path, opts) { const s = stream(path, opts); this.own(() => s.close()); return s; }
  child() { const c = new Scope(); this.own(() => c.dispose()); return c; }
  dispose() { this.alive = false; for (const f of this._cleanups.splice(0)) { try { f(); } catch { /* ignore */ } } }
  get api() { return api; }
  // Load async data into `el` with skeleton -> content | error states. `fn` returns a Node or array of Nodes.
  async loadInto(el, fn, { skeletonRows = 3 } = {}) {
    clear(el).append(skeleton(skeletonRows));
    try { const out = await fn(); if (!this.alive) return; clear(el).append(...[out].flat().filter(Boolean)); }
    catch (e) { if (!this.alive) return; clear(el).append(errorBox(e, () => this.loadInto(el, fn, { skeletonRows }))); }
  }
}

export class NcView extends HTMLElement {
  constructor() { super(); this.scope = new Scope(); }
  connectedCallback() { this.classList.add('view'); this.setup?.(); }
  disconnectedCallback() { this.scope.dispose(); }
}
