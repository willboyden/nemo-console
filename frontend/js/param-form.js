// Pure logic behind the param form generated from the /params catalog (ParamSpec in domain/models.py). No DOM here.
export const GROUP_ORDER = ['general', 'model', 'memory', 'parallelism', 'context', 'scheduling', 'performance', 'network', 'advanced'];
export const REDACTED = '[set]';
export const isRedacted = (v) => v === REDACTED;
export const isSet = (values, key) => Object.prototype.hasOwnProperty.call(values, key) && values[key] !== undefined;
export const isSecretSpec = (s) => /(token|secret|password|api[_-]?key)/i.test(`${s.key} ${s.flag || ''}`);

export function filterSpecs(specs, { query = '', showAdvanced = false } = {}) {
  const q = query.trim().toLowerCase();
  return specs.filter((s) => {
    if (q) return [s.key, s.flag, s.label, s.help, s.group].some((x) => String(x || '').toLowerCase().includes(q));
    return showAdvanced || !s.advanced;
  });
}

export function groupSpecs(specs) {
  const by = new Map();
  for (const s of specs) { const g = s.group || 'general'; if (!by.has(g)) by.set(g, []); by.get(g).push(s); }
  const order = [...GROUP_ORDER, ...[...by.keys()].filter((g) => !GROUP_ORDER.includes(g)).sort()];
  return order.filter((g) => by.has(g)).map((group) => ({ group, specs: by.get(group) }));
}

/** Raw input text -> typed value. Empty input means "unset" (value undefined, ok true). Messages are i18n keys. */
export function coerce(spec, raw) {
  if (raw === '' || raw == null) return { ok: true, value: undefined };
  switch (spec.type) {
    case 'int': return /^-?\d+$/.test(String(raw).trim()) ? { ok: true, value: parseInt(raw, 10) } : { ok: false, error: 'params.err_int' };
    case 'float': { const n = Number(raw); return Number.isFinite(n) && String(raw).trim() !== '' ? { ok: true, value: n } : { ok: false, error: 'params.err_float' }; }
    case 'bool': return { ok: true, value: raw === true || raw === 'true' };
    case 'string_list': return { ok: true, value: String(raw).split(/[\n,]/).map((x) => x.trim()).filter(Boolean) };
    default: return { ok: true, value: String(raw) };
  }
}

/** Returns null when valid, else {key, vars} for the message. */
export function validateValue(spec, v) {
  if (v === undefined || v === null) return null;
  if ((spec.type === 'int' || spec.type === 'float') && typeof v === 'number') {
    if (spec.min != null && v < spec.min) return { key: 'params.err_min', vars: { n: spec.min } };
    if (spec.max != null && v > spec.max) return { key: 'params.err_max', vars: { n: spec.max } };
  }
  if (spec.type === 'enum' && spec.choices && !spec.choices.includes(String(v))) return { key: 'params.err_choice' };
  return null;
}

export function validateAll(specs, values) {
  const errs = {};
  for (const s of specs) if (isSet(values, s.key)) { const e = validateValue(s, values[s.key]); if (e) errs[s.key] = e; }
  return errs;
}

/** Only known keys with a real value; the "[set]" placeholder is never sent back (it would overwrite the secret). */
export function pruneValues(specs, values) {
  const known = new Set(specs.map((s) => s.key));
  return Object.fromEntries(Object.entries(values || {}).filter(([k, v]) => known.has(k) && v !== undefined && !isRedacted(v)));
}
export function splitRedacted(values) {
  const clean = {}, redacted = [];
  for (const [k, v] of Object.entries(values || {})) { if (isRedacted(v)) redacted.push(k); else clean[k] = v; }
  return { values: clean, redacted };
}
/** Keys whose value differs from the currently applied values (order-insensitive for lists). */
export function changedKeys(specs, applied, draft) {
  const norm = (v) => (Array.isArray(v) ? JSON.stringify(v) : v === undefined ? undefined : String(v));
  return specs.map((s) => s.key).filter((k) => norm(applied?.[k]) !== norm(draft?.[k]));
}
/** Does applying `keys` need a restart? True when any changed spec has requires_restart. */
export const needsRestart = (specs, keys) => specs.some((s) => keys.includes(s.key) && s.requires_restart !== false);
/** Defaults + explicit values for ActionSpec.params -> body params (empty inputs omitted). */
export function actionParamValues(specs, raw) {
  const out = {};
  for (const s of specs) {
    const c = coerce(s, raw[s.key]);
    if (c.ok && c.value !== undefined) out[s.key] = c.value; else if (s.default != null && raw[s.key] === undefined) out[s.key] = s.default;
  }
  return out;
}
