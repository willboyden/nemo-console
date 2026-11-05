// Field help for the config editor's Form view: what a key is and what changing it does. The text lives in
// i18n/fields.en.json (catalogs of path patterns per config file); this module finds the entry for a field and, for a
// Dynamo command-line argument, the matching flag from the system's Params catalog. Pure logic, no DOM, so tests import it.
//
// Pattern segments: a literal key, `*` = any one mapping key, `#` = any list index, `**` = any number of further levels
// (only as the last segment). The most specific pattern wins: literal segments outweigh wildcards.

export const FIELDS_URL = 'i18n/fields.en.json';
let pending = null;
/** The catalog document, fetched once per page load (a failed fetch is retried next time). */
export function loadFieldHelp(url = FIELDS_URL) {
  pending ||= fetch(url).then((r) => {
    if (!r.ok) throw new Error(`field help load failed: ${r.status}`);
    return r.json();
  }).catch((e) => { pending = null; throw e; });
  return pending;
}

const esc = (s) => s.replace(/[.+?^${}()|[\]\\]/g, '\\$&');
/** A file-name glob: `*` = any text (including none). Everything else is literal. */
export const globMatch = (pat, s) => new RegExp(`^${String(pat).split('*').map(esc).join('.*')}$`).test(String(s));

/**
 * The catalogs a config file uses: every `files` row whose system and file globs match, in document order (an earlier
 * row, then an earlier catalog in a row, wins a tie). Returns null when no row matches.
 */
export function catalogsFor(doc, system, file) {
  if (!doc?.files || !doc.catalogs) return null;
  const rows = doc.files.filter((r) => globMatch(r.system, system) && globMatch(r.file, file));
  if (!rows.length) return null;
  const entries = [];
  let order = 0;
  for (const r of rows) {
    for (const id of r.catalogs || []) {
      for (const [pat, entry] of Object.entries(doc.catalogs[id] || {})) entries.push({ segs: pat.split('.'), entry, order: order++ });
    }
  }
  return { entries, flags: rows.some((r) => r.flags) };
}

/** How well a pattern fits a path: -1 = no match; higher = more specific. */
export function matchScore(segs, path) {
  let score = 0;
  for (let i = 0; i < segs.length; i++) {
    const s = segs[i];
    if (s === '**') return i === segs.length - 1 && path.length > i ? score + 1 : -1;
    if (i >= path.length) return -1;
    const p = path[i];
    if (s === '#') { if (typeof p !== 'number') return -1; score += 2; }
    else if (s === '*') { if (typeof p !== 'string') return -1; score += 2; }
    else if (String(p) !== s || typeof p === 'number') return -1;
    else score += 4;
  }
  return segs.length === path.length ? score + 2 : -1;
}

/** The best catalog entry for a path (keys are strings, list indexes numbers), or null. */
export function lookupHelp(cat, path) {
  if (!cat) return null;
  let best = null, bestScore = -1;
  for (const e of cat.entries) {
    const sc = matchScore(e.segs, path);
    if (sc > bestScore || (sc === bestScore && best && e.order < best.order)) { best = e; bestScore = sc; }
  }
  return bestScore >= 0 ? best.entry : null;
}

/**
 * A command-line argument's flag in the system's Params catalog. `tokens` is the whole command list and `i` the
 * argument's index: `--flag`, `--flag=value`, or the value that follows a `--flag`.
 * Returns { flag, param, isValue } or null.
 */
export function flagFor(params, tokens, i) {
  if (!Array.isArray(params) || !params.length || !Array.isArray(tokens)) return null;
  const find = (tok) => {
    if (typeof tok !== 'string' || !tok.startsWith('-')) return null;
    const flag = tok.split('=')[0];
    const param = params.find((p) => p.flag === flag);
    return param ? { flag, param } : null;
  };
  const own = find(tokens[i]);
  if (own) return { ...own, isValue: String(tokens[i]).includes('=') };
  const prev = i > 0 ? find(tokens[i - 1]) : null;
  if (prev && prev.param.type !== 'bool' && !String(tokens[i - 1]).includes('=')) return { ...prev, isValue: true };   // a switch takes no value
  return null;
}

/** An environment variable the Params catalog manages (flag `env:NAME`), or null. Returns { flag, param, isValue }. */
export function envFor(params, name) {
  if (!Array.isArray(params)) return null;
  const param = params.find((p) => p.flag === `env:${name}`);
  return param ? { flag: name, param, isValue: false } : null;
}
