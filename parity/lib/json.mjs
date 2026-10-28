// Kind-preserving JSON. JSON.parse turns 1.0 into 1, so it cannot tell Python's float 1.0 from the int 1; this tiny
// parser keeps each number's kind. Values become a tagged tree:
//   {k:'null'} {k:'bool',v} {k:'int',v:BigInt} {k:'float',v:Number,raw} {k:'str',v} {k:'arr',v:[..]} {k:'obj',v:Map}
// A number is a float when its token has '.', 'e' or 'E' (Python and serde_json both print 1.0, never 1, for a float);
// NaN / Infinity / -Infinity (Python's json.dumps default) are accepted as floats.

export class JsonError extends Error {}

export function parse(text) {
  let i = 0;
  const n = text.length;
  const ws = () => { while (i < n && (text[i] === ' ' || text[i] === '\t' || text[i] === '\n' || text[i] === '\r')) i++; };
  const fail = (why) => { throw new JsonError(`${why} at offset ${i}`); };
  const lit = (word, node) => { if (text.startsWith(word, i)) { i += word.length; return node; } return fail(`bad literal`); };

  function str() {
    i++; // opening quote
    let out = '';
    let start = i;
    while (true) {
      if (i >= n) fail('unterminated string');
      const c = text.charCodeAt(i);
      if (c === 34) { out += text.slice(start, i); i++; return out; }          // "
      if (c === 92) {                                                          // backslash
        out += text.slice(start, i);
        const e = text[i + 1];
        if (e === 'u') {
          const hex = text.slice(i + 2, i + 6);
          if (!/^[0-9a-fA-F]{4}$/.test(hex)) fail('bad \\u escape');
          out += String.fromCharCode(parseInt(hex, 16));
          i += 6;
        } else {
          const map = { '"': '"', '\\': '\\', '/': '/', b: '\b', f: '\f', n: '\n', r: '\r', t: '\t' };
          if (!(e in map)) fail('bad escape');
          out += map[e];
          i += 2;
        }
        start = i;
        continue;
      }
      if (c < 0x20) fail('control character in string');
      i++;
    }
  }

  function num() {
    const m = /^-?(?:0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?/.exec(text.slice(i, i + 400));
    if (!m) {
      if (text.startsWith('-Infinity', i)) { i += 9; return { k: 'float', v: -Infinity, raw: '-Infinity' }; }
      fail('bad number');
    }
    i += m[0].length;
    if (m[1] !== undefined || m[2] !== undefined) return { k: 'float', v: Number(m[0]), raw: m[0] };
    return { k: 'int', v: BigInt(m[0]) };
  }

  function value() {
    ws();
    if (i >= n) fail('unexpected end');
    const c = text[i];
    if (c === '{') {
      i++;
      const map = new Map();
      ws();
      if (text[i] === '}') { i++; return { k: 'obj', v: map }; }
      while (true) {
        ws();
        if (text[i] !== '"') fail('expected key');
        const key = str();
        ws();
        if (text[i] !== ':') fail('expected :');
        i++;
        map.set(key, value());            // a duplicate key keeps the last value (as Python's json does)
        ws();
        if (text[i] === ',') { i++; continue; }
        if (text[i] === '}') { i++; return { k: 'obj', v: map }; }
        fail('expected , or }');
      }
    }
    if (c === '[') {
      i++;
      const arr = [];
      ws();
      if (text[i] === ']') { i++; return { k: 'arr', v: arr }; }
      while (true) {
        arr.push(value());
        ws();
        if (text[i] === ',') { i++; continue; }
        if (text[i] === ']') { i++; return { k: 'arr', v: arr }; }
        fail('expected , or ]');
      }
    }
    if (c === '"') return { k: 'str', v: str() };
    if (c === 't') return lit('true', { k: 'bool', v: true });
    if (c === 'f') return lit('false', { k: 'bool', v: false });
    if (c === 'n') return lit('null', { k: 'null' });
    if (c === 'N') return lit('NaN', { k: 'float', v: NaN, raw: 'NaN' });
    if (c === 'I') return lit('Infinity', { k: 'float', v: Infinity, raw: 'Infinity' });
    return num();
  }

  const out = value();
  ws();
  if (i !== n) fail('trailing data');
  return out;
}

/** Plain JS value -> tagged tree (Number.isInteger => int; used for values the harness builds itself). */
export function fromPlain(x) {
  if (x === null || x === undefined) return { k: 'null' };
  if (typeof x === 'boolean') return { k: 'bool', v: x };
  if (typeof x === 'bigint') return { k: 'int', v: x };
  if (typeof x === 'number') return Number.isInteger(x) ? { k: 'int', v: BigInt(x) } : { k: 'float', v: x, raw: String(x) };
  if (typeof x === 'string') return { k: 'str', v: x };
  if (Array.isArray(x)) return { k: 'arr', v: x.map(fromPlain) };
  if (x instanceof Map) return { k: 'obj', v: new Map([...x].map(([k, v]) => [k, fromPlain(v)])) };
  return { k: 'obj', v: new Map(Object.entries(x).map(([k, v]) => [k, fromPlain(v)])) };
}

function fmtFloat(v) {
  if (Number.isNaN(v)) return 'NaN';
  if (!Number.isFinite(v)) return v > 0 ? 'Infinity' : '-Infinity';
  const s = String(v);
  return /[.eE]/.test(s) ? s : `${s}.0`;
}

/** Canonical text of a tagged tree: sorted keys, floats always show their kind (1.0), ints never do. */
export function stringify(node, indent = 0) {
  if (node === undefined) return '(absent)';
  const pad = (d) => (indent ? '\n' + ' '.repeat(indent * d) : '');
  const walk = (x, d) => {
    switch (x.k) {
      case 'null': return 'null';
      case 'bool': return String(x.v);
      case 'int': return x.v.toString();
      case 'float': return fmtFloat(x.v);
      case 'str': return JSON.stringify(x.v);
      case 'arr': return x.v.length ? `[${x.v.map((e) => pad(d + 1) + walk(e, d + 1)).join(',')}${pad(d)}]` : '[]';
      case 'obj': {
        const keys = [...x.v.keys()].sort();
        return keys.length ? `{${keys.map((key) => `${pad(d + 1)}${JSON.stringify(key)}:${indent ? ' ' : ''}${walk(x.v.get(key), d + 1)}`).join(',')}${pad(d)}}` : '{}';
      }
      default: return `<${x.k}>`;
    }
  };
  return walk(node, 0);
}

/** Tagged tree -> plain JS for reports (ints beyond 2^53 become strings; floats lose only their kind marker). */
export function toPlain(node) {
  switch (node.k) {
    case 'null': return null;
    case 'bool': case 'str': return node.v;
    case 'int': return (node.v <= BigInt(Number.MAX_SAFE_INTEGER) && node.v >= -BigInt(Number.MAX_SAFE_INTEGER)) ? Number(node.v) : node.v.toString();
    case 'float': return Number.isFinite(node.v) ? node.v : fmtFloat(node.v);
    case 'arr': return node.v.map(toPlain);
    case 'obj': return Object.fromEntries([...node.v].map(([k, v]) => [k, toPlain(v)]));
    default: return null;
  }
}

/** JSON pointer lookup on a tagged tree (RFC 6901). Returns undefined when absent. */
export function pointer(node, ptr) {
  if (ptr === '' || ptr === '/') return node;
  let cur = node;
  for (const raw of ptr.split('/').slice(1)) {
    const key = raw.replace(/~1/g, '/').replace(/~0/g, '~');
    if (!cur) return undefined;
    if (cur.k === 'obj') cur = cur.v.get(key);
    else if (cur.k === 'arr') cur = cur.v[Number(key)];
    else return undefined;
  }
  return cur;
}

/** Map every node bottom-up: fn(node, path, parentKey) returns a replacement node (or the same node). */
export function mapTree(node, fn, path = '', key = null) {
  let out = node;
  if (node.k === 'arr') out = { k: 'arr', v: node.v.map((e, i) => mapTree(e, fn, `${path}/${i}`, i)) };
  else if (node.k === 'obj') out = { k: 'obj', v: new Map([...node.v].map(([k, v]) => [k, mapTree(v, fn, `${path}/${escapeKey(k)}`, k)])) };
  return fn(out, path, key);
}

export const escapeKey = (k) => String(k).replace(/~/g, '~0').replace(/\//g, '~1');
