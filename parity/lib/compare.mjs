// Structural comparison of two tagged JSON trees (lib/json.mjs). Objects compare as key -> value (order ignored),
// arrays element by element (order kept), null and an absent key are different, an int never equals a float, ints
// compare exactly (BigInt) and floats numerically. Each difference is reported with its JSON pointer.
import { escapeKey, stringify } from './json.mjs';

const SHOW_MAX = 400;
const show = (node) => {
  const s = node === undefined ? '(absent)' : stringify(node);
  return s.length > SHOW_MAX ? `${s.slice(0, SHOW_MAX)}...[${s.length} chars]` : s;
};

/** Two long strings: show both around the first differing character. */
export function strWindow(a, b, before = 60, after = 160) {
  let i = 0;
  while (i < a.length && i < b.length && a[i] === b[i]) i++;
  const cut = (s) => {
    const from = Math.max(0, i - before);
    return `${from ? `…[${from}]` : ''}${JSON.stringify(s.slice(from, i + after)).slice(1, -1)}${i + after < s.length ? `…[+${s.length - i - after}]` : ''}`;
  };
  return [cut(a), cut(b)];
}

export function compareTrees(a, b, path = '', out = [], limit = 50) {
  if (out.length >= limit) return out;
  if (a === undefined || b === undefined) {
    if (a !== b) out.push({ path: path || '/', a: show(a), b: show(b) });
    return out;
  }
  if (a.k !== b.k) {
    out.push({ path: path || '/', a: `${show(a)} (${a.k})`, b: `${show(b)} (${b.k})` });
    return out;
  }
  switch (a.k) {
    case 'null': return out;
    case 'bool':
      if (a.v !== b.v) out.push({ path: path || '/', a: show(a), b: show(b) });
      return out;
    case 'str':
      if (a.v !== b.v) {
        if (a.v.length + b.v.length > 300) { const [wa, wb] = strWindow(a.v, b.v); out.push({ path: path || '/', a: wa, b: wb }); }
        else out.push({ path: path || '/', a: show(a), b: show(b) });
      }
      return out;
    case 'int':
      if (a.v !== b.v) out.push({ path: path || '/', a: show(a), b: show(b) });
      return out;
    case 'float': {
      const same = a.v === b.v || (Number.isNaN(a.v) && Number.isNaN(b.v));
      if (!same) out.push({ path: path || '/', a: show(a), b: show(b) });
      return out;
    }
    case 'arr': {
      const len = Math.max(a.v.length, b.v.length);
      for (let i = 0; i < len && out.length < limit; i++) compareTrees(a.v[i], b.v[i], `${path}/${i}`, out, limit);
      return out;
    }
    case 'obj': {
      const keys = [...new Set([...a.v.keys(), ...b.v.keys()])].sort();
      for (const key of keys) {
        if (out.length >= limit) break;
        compareTrees(a.v.get(key), b.v.get(key), `${path}/${escapeKey(key)}`, out, limit);
      }
      return out;
    }
    default:
      out.push({ path: path || '/', a: show(a), b: show(b) });
      return out;
  }
}

/** Plain values (strings, numbers, arrays of strings) compared as JSON text, for headers / argv / SSE framing. */
export function comparePlain(a, b, path, out) {
  const sa = JSON.stringify(a);
  const sb = JSON.stringify(b);
  if (sa === sb) return out;
  if (typeof a === 'string' && typeof b === 'string' && a.length + b.length > 300) {
    const [wa, wb] = strWindow(a, b);
    out.push({ path, a: wa, b: wb });
  } else out.push({ path, a: sa === undefined ? '(absent)' : sa, b: sb === undefined ? '(absent)' : sb });
  return out;
}

/** Multisets of strings (e.g. normalised argv lines): reports what only one side has. */
export function compareSets(a, b, path, out) {
  const count = (xs) => xs.reduce((m, x) => m.set(x, (m.get(x) || 0) + 1), new Map());
  const ca = count(a);
  const cb = count(b);
  const onlyA = [...ca.keys()].filter((k) => !cb.has(k)).sort();
  const onlyB = [...cb.keys()].filter((k) => !ca.has(k)).sort();
  if (onlyA.length || onlyB.length) {
    // pair up the closest lines so the report shows WHERE they differ
    if (onlyA.length === 1 && onlyB.length === 1) { const [wa, wb] = strWindow(onlyA[0], onlyB[0]); out.push({ path, a: [wa], b: [wb] }); }
    else out.push({ path, a: onlyA.length ? onlyA : '(none extra)', b: onlyB.length ? onlyB : '(none extra)' });
  }
  return out;
}
