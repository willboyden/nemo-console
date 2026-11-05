// Live edit checks for the config editor: syntax (cfg-parse.js), the file's schema where the console knows one, the
// cross-references inside a Switchyard routes file, content the backend refuses to save (secrets, NUL), and plain
// hygiene (line endings, invisible characters). Pure logic, no DOM, so tests import it.
//
//   checkConfig({ system, file, format, text }) -> { parse, diagnostics, schema }
//   diagnostic: { severity: 'error' | 'warn' | 'info', line, col?, message, source, path? }
//
// These checks are early warnings that run on every keystroke. The backend's validator (the Check button and every
// Apply) stays the authority. The schema rules mirror the backend adapters (aiperf.rs INT_KEYS / ENDPOINT_TYPES,
// modelopt.rs RECIPE_KEYS / METHODS, evaluator.rs SUPPORTED_BENCHMARKS / REFUSED_BENCHMARKS / limits);
// tests/cfg-checks.test.mjs reads those constants from the Rust source and fails when the two drift.
import { parseConfig, entryOf } from './cfg-parse.js';
/** A schema table's OWN entry for a file-controlled key: `constructor`, `__proto__` or `toString` never hit Object.prototype. */
const own = (obj, k) => (obj && Object.hasOwn(obj, k) ? obj[k] : undefined);

// ---------------------------------------------------------------------------------------------- schema vocabulary
// map:  fixed keys (closed: unknown keys are errors), `pattern` keys matched by a regex, required keys
// dict: user-named entries (profiles, recipes, routes) whose names match keyRe, each validated by `values`
// list: items validated by `items`; uniqueBy names a key that may not repeat
// int / num / bool / str: scalars with ranges, choices, patterns; `ref` names a top-level table whose keys are the
//                          allowed values (Switchyard targets, llm clients)
const map = (keys, o = {}) => ({ t: 'map', keys, closed: true, required: [], ...o });
const dict = (values, o = {}) => ({ t: 'dict', values, min: 0, ...o });
const list = (items, o = {}) => ({ t: 'list', items, min: 0, ...o });
const int = (min, max, o = {}) => ({ t: 'int', min, max, ...o });
const num = (o = {}) => ({ t: 'num', ...o });
const bool = (o = {}) => ({ t: 'bool', ...o });
const str = (o = {}) => ({ t: 'str', ...o });
const one = (o = {}) => ({ t: 'const', value: 1, help: 'must be 1', ...o });

const NAME = { keyRe: /^[a-z0-9][a-z0-9-]{0,39}$/, keyHint: 'lower case letters, digits and -, starting with a letter or digit, up to 40 characters' };

export const AIPERF = {
  INT_KEYS: [['concurrency', 1, 64, true], ['requests', 1, 2000, true], ['isl', 1, 32768, true], ['osl', 1, 4096, true],
    ['warmup_requests', 0, 100, false], ['timeout_s', 1, 600, false], ['seed', 0, 2147483647, false]],
  ENDPOINT_TYPES: ['chat', 'completions'], MAX_PROFILES: 32,
};
const AIPERF_HELP = { concurrency: 'requests kept in flight', requests: 'measured requests', isl: 'synthetic input tokens (mean)', osl: 'output tokens requested (mean)',
  warmup_requests: 'default 0', timeout_s: 'per-request timeout in seconds, default 120', seed: 'synthetic-prompt seed, default 42' };
const aiperfSchema = map({
  version: one(),
  profiles: dict(map({
    description: str({ oneLine: true, maxLen: 300, help: 'one line, at most 300 characters' }),
    model: str({ re: /^[A-Za-z0-9][A-Za-z0-9._:/@+-]{0,127}$/, reHint: 'a router model alias (letters, digits and . _ : / @ + -)' }),
    endpoint_type: str({ choices: AIPERF.ENDPOINT_TYPES, help: 'default chat' }),
    streaming: bool({ help: 'default true; time to first token and inter-token latency need streaming' }),
    ...Object.fromEntries(AIPERF.INT_KEYS.map(([k, lo, hi]) => [k, int(lo, hi, { help: AIPERF_HELP[k] })])),
  }, { required: ['model', ...AIPERF.INT_KEYS.filter((r) => r[3]).map((r) => r[0])] }), { ...NAME, min: 1, max: AIPERF.MAX_PROFILES, what: 'preset' }),
}, { id: 'aiperf_profiles', required: ['version', 'profiles'], cross: aiperfCross });
function aiperfCross(root, report) {
  const profiles = entryOf(root, 'profiles')?.node;
  if (profiles?.kind !== 'map') return;
  for (const { key, node } of profiles.entries) {
    if (node.kind !== 'map') continue;
    const v = (k) => entryOf(node, k)?.node;
    const s = v('streaming');
    if (s?.kind === 'scalar' && s.type === 'bool' && s.value === false) report('warn', `${key}: streaming is off, so time to first token and inter-token latency are not measured`, s.line, ['profiles', key, 'streaming']);
    const c = v('concurrency'), r = v('requests');
    if (c?.type === 'int' && r?.type === 'int' && r.value < c.value) report('warn', `${key}: fewer requests than the concurrency, so the target concurrency is never reached`, r.line, ['profiles', key, 'requests']);
  }
}

export const MODELOPT = { METHODS: ['reap', 'quantize', 'minitron'], RECIPE_KEYS: ['method', 'base_model', 'output', 'description', 'preset', 'sparsity', 'config'], MAX_RECIPES: 64 };
const modeloptSchema = map({
  version: one({ orTrue: true }),   // the tool tests `version != 1`, and in Python True == 1
  recipes: dict(map({
    method: str({ choices: MODELOPT.METHODS, help: 'what the job does' }),
    base_model: str({ re: /^[A-Za-z0-9][A-Za-z0-9._-]*\/[A-Za-z0-9._-]+$/, reHint: 'a Hugging Face repo id (org/name)', help: 'read from the offline cache' }),
    output: str({ prefix: '/runs/', minLen: 7, help: 'a path under /runs (the job\'s run dir)' }),
    description: str(), preset: str({ help: 'quantization preset, e.g. NVFP4' }),
    sparsity: num({ min: 0, maxExcl: 1, help: 'from 0 (inclusive) to 1 (exclusive): the fraction of weights REAP keeps' }),
    config: str({ help: 'a file name in the run dir (Minitron config)' }),
  }, { required: ['method', 'base_model', 'output'] }), { ...NAME, min: 1, max: MODELOPT.MAX_RECIPES, what: 'recipe' }),
}, { id: 'modelopt_recipes', required: ['version', 'recipes'] });

export const EVALUATOR = {
  SUPPORTED: ['drop', 'gsm8k', 'math500', 'mmlu', 'mmlu_pro', 'triviaqa'],
  REFUSED: { humaneval: 'needs a Docker sandbox to execute generated code', pinchbench: 'needs a Docker sandbox', 'terminal-bench-hard': 'needs a Docker sandbox',
    'terminal-bench-hard-aa-split': 'needs a Docker sandbox', 'terminal-bench-v1': 'needs a Docker sandbox', nmp_harbor: 'needs a Docker sandbox', simpleqa: 'needs an LLM judge',
    healthbench: 'needs an LLM judge', xstest: 'needs an LLM judge and downloads its CSV from github.com at run time',
    gpqa: 'the dataset is gated on Hugging Face (needs an access token)', mgsm: 'its dataset loader runs remote code (trust_remote_code)' },
  MAX_SUITES: 20, MAX_BENCHES: 8, MAX_PROBLEMS: 5000, MAX_WORK: 20000,
};
const evaluatorSchema = map({
  version: one({ orTrue: true }),
  suites: dict(map({
    description: str({ maxLen: 200, help: 'at most 200 characters' }),
    model: str({ re: /^[A-Za-z0-9][A-Za-z0-9._/:@-]{0,127}$/, reHint: 'a router model alias (letters, digits and . _ / : @ -)' }),
    benchmarks: list(map({
      bench: str({ choices: EVALUATOR.SUPPORTED, refused: EVALUATOR.REFUSED, help: 'datasets must be prefetched first' }),
      max_problems: int(1, EVALUATOR.MAX_PROBLEMS, { help: 'a bounded, shuffled sample (seed 42)' }),
      max_tokens: int(16, 32768, { nullable: true }),
      temperature: num({ min: 0, max: 2, nullable: true }),
      repeats: int(1, 8, { help: 'default 1', onlyIfInt: true }),   // evaluator.rs: a repeats that is not an integer counts as 1
    }, { required: ['bench', 'max_problems'] }), { min: 1, max: EVALUATOR.MAX_BENCHES, uniqueBy: 'bench' }),
  }, { required: ['model', 'benchmarks'] }), { ...NAME, min: 1, max: EVALUATOR.MAX_SUITES, what: 'suite' }),
}, { id: 'evaluator_suites', required: ['version', 'suites'], cross: evaluatorCross });
function evaluatorCross(root, report) {
  const suites = entryOf(root, 'suites')?.node;
  if (suites?.kind !== 'map') return;
  for (const { key, line, node } of suites.entries) {
    const benches = node.kind === 'map' ? entryOf(node, 'benchmarks')?.node : null;
    if (benches?.kind !== 'seq') continue;
    let work = 0;
    for (const b of benches.items.slice(0, EVALUATOR.MAX_BENCHES)) {
      if (b.kind !== 'map') continue;
      const mp = entryOf(b, 'max_problems')?.node, rp = entryOf(b, 'repeats')?.node;
      const m = mp?.type === 'int' && mp.value >= 1 && mp.value <= EVALUATOR.MAX_PROBLEMS ? mp.value : 0;
      const r = rp?.type === 'int' && rp.value >= 1 && rp.value <= 8 ? rp.value : 1;
      work += m * r;
    }
    if (work > EVALUATOR.MAX_WORK) report('error', `suite '${key}': max_problems x repeats sums to ${work}; the limit is ${EVALUATOR.MAX_WORK}`, line, ['suites', key]);
  }
}

// Switchyard routes (also the Relay plugin's copy): llm clients, targets and routes that name each other.
const targetRef = (help) => str({ ref: 'targets', help });
const any = (help) => ({ t: 'any', help });   // shown in the Form, not checked here (the dry-run decides)
/** switchyard.rs base_url_problem, conservatively: only what is certain without a full URL parser. */
export function baseUrlProblem(v) {
  const s = String(v).trim();
  if (!s) return 'base_url is missing or not a string';
  const m = /^(https?):\/\/([^/?#]*)/i.exec(s);
  if (!m) return `base_url '${s}' must use http or https`;   // urlsplit: another scheme, or none at all
  if (m[2].includes('@')) return 'base_url must not contain userinfo (user:pass@host)';
  const host = (m[2].startsWith('[') ? m[2].slice(1, m[2].indexOf(']')) : m[2].replace(/:[^:]*$/, '')).toLowerCase().replace(/\.$/, '');
  if (!host) return `base_url '${s}' has no host`;
  if (/^\d{1,3}(?:\.\d{1,3}){3}$/.test(host)) return host.startsWith('127.') ? null : `base_url host ${host} is not loopback; only loopback or an in-network service name is allowed`;
  if (host.includes(':')) return host === '::1' || /^::ffff:127\./.test(host) ? null : `base_url host ${host} is not loopback; only loopback or an in-network service name is allowed`;
  if (host === 'localhost' || /^[a-z][a-z0-9_-]*$/.test(host)) return null;
  return `base_url host '${host}' is neither loopback nor a bare service name (no dots); external hosts need an egress-allowlist change, not a routes.toml edit`;
}
const switchyardSchema = map({
  llm_clients: dict(map({ base_url: str({ check: baseUrlProblem, help: 'loopback or an in-network service name (http or https)' }) }, { closed: false, required: ['base_url'] }), { what: 'llm client' }),
  targets: dict(map({ llm_client: str({ ref: 'llm_clients', help: 'one of the [llm_clients.*] blocks' }), id: any('the model id sent upstream') }, { closed: false, required: ['llm_client'] }), { what: 'target' }),
  routes: dict(map({
    classifier: map({ target: targetRef('the judge target') }, { closed: false }),
  }, { closed: false, pattern: [[/^(?:target|.+_target)$/, targetRef('one of the [targets.*] entries')]] }), { what: 'route', min: 1 }),
}, { id: 'switchyard_routes', closed: false, required: ['routes'], cross: switchyardCross });
function switchyardCross(root, report) {
  const targets = entryOf(root, 'targets')?.node, routes = entryOf(root, 'routes')?.node;
  if (targets?.kind === 'map') {
    const seen = new Map();
    for (const { key, node } of targets.entries) {
      if (node.kind !== 'map') continue;
      const id = entryOf(node, 'id')?.node, cl = entryOf(node, 'llm_client')?.node;
      const sig = `${id?.value}\u0000${cl?.value}`;
      if (seen.has(sig)) report('warn', `targets '${seen.get(sig)}' and '${key}' share (id '${id?.value}', llm_client '${cl?.value}'): Switchyard keeps only one and silently drops the other (with its system_prompt). Give one its own [llm_clients.*] block.`, node.line, ['targets', key]);
      else seen.set(sig, key);
    }
  }
  if (routes?.kind === 'map') for (const { key, node } of routes.entries) {
    const id = node.kind === 'map' ? entryOf(node, 'id')?.node : null;
    if (id?.kind === 'scalar' && id.value !== key) report('warn', `route table key '${key}' differs from its id '${id.value}' (clients see the id)`, id.line, ['routes', key, 'id']);
  }
}

/** The schema the console knows for one config file (system id, file id), or null. */
export function schemaFor(system, file) {
  if (system === 'aiperf' && file === 'profiles') return aiperfSchema;
  if (system === 'modelopt' && file === 'recipes') return modeloptSchema;
  if (system === 'evaluator' && file === 'suites.yaml') return evaluatorSchema;
  if ((system === 'switchyard' || system === 'relay') && file === 'routes') return switchyardSchema;
  return null;
}

/** The schema node for a path inside the document (keys and list indexes), or null. */
export function specAt(schema, path) {
  let s = schema;
  for (const k of path) {
    if (!s) return null;
    if (s.t === 'map') s = own(s.keys, k) || s.pattern?.find(([re]) => re.test(String(k)))?.[1] || null;
    else if (s.t === 'dict') s = s.values;
    else if (s.t === 'list') s = s.items;
    else return null;
  }
  return s;
}

/** A one-line description of what a field accepts, for the Form view. */
export function describeSpec(spec) {
  if (!spec) return '';
  const p = [];
  if (spec.t === 'int') p.push(`whole number ${spec.min}–${spec.max}`);
  if (spec.t === 'num') p.push(`number${spec.min != null ? ` from ${spec.min}` : ''}${spec.max != null ? ` to ${spec.max}` : spec.maxExcl != null ? ` to below ${spec.maxExcl}` : ''}`);
  if (spec.t === 'bool') p.push('true or false');
  if (spec.t === 'const') p.push(`must be ${spec.value}`);
  if (spec.t === 'str' && spec.choices) p.push(`one of ${spec.choices.join(', ')}`);
  if (spec.t === 'str' && spec.reHint) p.push(spec.reHint);
  if (spec.t === 'str' && spec.prefix) p.push(`starts with ${spec.prefix}`);
  if (spec.nullable) p.push('or empty');
  if (spec.help && !p.includes(spec.help)) p.push(spec.help);
  return p.join(' · ');
}

const nodeLabel = (n) => (n.kind === 'map' ? 'a mapping' : n.kind === 'seq' ? 'a list' : n.type === 'string' ? 'text' : n.type === 'null' ? 'empty' : `${n.type === 'int' ? 'a whole number' : n.type === 'float' ? 'a number' : n.type === 'bool' ? 'true/false' : 'a date'}`);
const show = (v) => JSON.stringify(v);

function closest(word, options) {
  const w = String(word).toLowerCase();
  const pre = options.filter((o) => o.length > 2 && w.length > 2 && (o.toLowerCase().startsWith(w) || w.startsWith(o.toLowerCase())));
  if (pre.length === 1) return pre[0];
  let best = null, score = 3;
  for (const o of options) {
    const d = lev(word, o);
    if (d < score) { score = d; best = o; }
  }
  return best;
}
function lev(a, b) {
  const m = a.length, n = b.length, d = Array.from({ length: m + 1 }, (_, i) => [i, ...Array(n).fill(0)]);
  for (let j = 1; j <= n; j++) d[0][j] = j;
  for (let i = 1; i <= m; i++) for (let j = 1; j <= n; j++) d[i][j] = Math.min(d[i - 1][j] + 1, d[i][j - 1] + 1, d[i - 1][j - 1] + (a[i - 1] === b[j - 1] ? 0 : 1));
  return d[m][n];
}

/** Walk a parsed document against a schema; report(severity, message, line, path). */
export function checkSchema(schema, root, report) {
  const refKeys = (name) => { const t = entryOf(root, name)?.node; return t?.kind === 'map' ? t.entries.map((e) => e.key) : []; };
  const where = () => '';   // the path is shown by the caller (the panel prefixes it, a Form field already shows it)
  function walk(spec, node, path, line) {
    if (!spec || !node) return;
    if (spec.t === 'map') {
      if (node.kind !== 'map') { report('error', `${where(path)}must be a mapping (found ${nodeLabel(node)})`, node.line ?? line, path); return; }
      for (const e of node.entries) {
        if (entryOf(node, e.key) !== e) continue;   // shadowed by a later duplicate (already warned about)
        const sub = own(spec.keys, e.key) || spec.pattern?.find(([re]) => re.test(e.key))?.[1];
        if (sub) walk(sub, e.node, [...path, e.key], e.line);
        else if (spec.closed) {
          const near = closest(e.key, Object.keys(spec.keys));
          report('error', `${where(path)}unknown key '${e.key}'${near ? ` (did you mean '${near}'?)` : ` (allowed: ${Object.keys(spec.keys).join(', ')})`}`, e.line, [...path, e.key]);
        }
      }
      for (const k of spec.required) if (!entryOf(node, k)) report('error', `${where(path)}'${k}' is required`, node.line ?? line, path);
      return;
    }
    if (spec.t === 'dict') {
      const what = spec.what || 'entry', whats = spec.what ? `${spec.what}s` : 'entries';
      if (node.kind !== 'map' || (spec.min && !node.entries.length)) { report('error', `${where(path)}must be a mapping${spec.min ? ` with at least ${spec.min} ${spec.min === 1 ? what : whats}` : ''}`, node.line ?? line, path); return; }
      if (spec.max && node.entries.length > spec.max) report('error', `${where(path)}at most ${spec.max} ${whats} are supported (found ${node.entries.length})`, node.line ?? line, path);
      for (const e of node.entries) {
        if (entryOf(node, e.key) !== e) continue;
        if (spec.keyRe && !spec.keyRe.test(e.key)) report('error', `the ${spec.what || 'entry'} name '${e.key}' must be ${spec.keyHint}`, e.line, [...path, e.key]);
        walk(spec.values, e.node, [...path, e.key], e.line);
      }
      return;
    }
    if (spec.t === 'list') {
      if (node.kind !== 'seq' || (spec.min && !node.items.length)) { report('error', `${where(path)}must be a list${spec.min ? ` with at least ${spec.min} item${spec.min === 1 ? '' : 's'}` : ''}`, node.line ?? line, path); return; }
      if (spec.max && node.items.length > spec.max) report('error', `${where(path)}at most ${spec.max} items`, node.line ?? line, path);
      const seen = new Map();
      node.items.forEach((it, i) => {
        walk(spec.items, it, [...path, i], it.line ?? line);
        const u = spec.uniqueBy && it.kind === 'map' ? entryOf(it, spec.uniqueBy)?.node : null;
        if (u?.kind === 'scalar') {
          if (seen.has(u.value)) report('error', `${where(path)}'${u.value}' is listed twice (also item ${seen.get(u.value) + 1})`, u.line, [...path, i, spec.uniqueBy]);
          else seen.set(u.value, i);
        }
      });
      return;
    }
    if (spec.t === 'any') return;
    // scalars
    const at = node.line ?? line;
    if (node.kind !== 'scalar') { report('error', `${where(path)}must be ${spec.t === 'str' ? 'text' : spec.t === 'bool' ? 'true or false' : 'a number'} (found ${nodeLabel(node)})`, at, path); return; }
    if (node.type === 'null' && spec.nullable) return;
    const v = node.value;
    if (spec.t === 'const') { if (!(((node.type === 'int' || node.type === 'float') && v === spec.value) || (spec.orTrue && node.type === 'bool' && v === true))) report('error', `${where(path)}must be ${spec.value}`, at, path); return; }
    if (spec.t === 'int') {
      if (spec.onlyIfInt && node.type !== 'int') return;
      if (node.type !== 'int' || v < spec.min || v > spec.max) {
        const hint = node.type === 'string' && /^[-+]?\d+(?:\.\d+)?[eE][-+]?\d+$/.test(v) ? ` ('${v}' is read as text: write it out in digits)` : node.type === 'string' && /^\d/.test(v) ? ' (it is quoted, so it is text)' : '';
        report('error', `${where(path)}must be a whole number from ${spec.min} to ${spec.max}${node.type === 'int' ? ` (found ${v})` : hint}`, at, path);
      }
      return;
    }
    if (spec.t === 'num') {
      const ok = (node.type === 'int' || node.type === 'float') && Number.isFinite(v) && (spec.min == null || v >= spec.min) && (spec.max == null || v <= spec.max) && (spec.maxExcl == null || v < spec.maxExcl);
      if (!ok) report('error', `${where(path)}must be a ${describeSpec({ ...spec, help: null, nullable: false })}${node.type === 'string' && /e/i.test(v) ? ` ('${v}' is read as text by PyYAML: write a decimal like 0.001)` : ''}`, at, path);
      return;
    }
    if (spec.t === 'bool') { if (node.type !== 'bool') report('error', `${where(path)}must be true or false (found ${nodeLabel(node)})`, at, path); return; }
    if (spec.t === 'str') {
      if (node.type !== 'string') { report('error', `${where(path)}must be text (found ${nodeLabel(node)}${node.type !== 'null' ? `: quote it, "${node.raw}"` : ''})`, at, path); return; }
      if (own(spec.refused, v)) { report('error', `${where(path)}${show(v)} is not supported: ${spec.refused[v]}`, at, path); return; }
      if (spec.choices && !spec.choices.includes(v)) { const near = closest(v, spec.choices); report('error', `${where(path)}must be one of ${spec.choices.join(', ')}${near ? ` (did you mean '${near}'?)` : ''}`, at, path); return; }
      if (spec.ref) {
        const keys = refKeys(spec.ref);
        if (!keys.includes(v)) { const near = closest(v, keys); report('error', `${where(path)}'${v}' is not a defined [${spec.ref}.*] entry${near ? ` (did you mean '${near}'?)` : ''}`, at, path); }
        return;
      }
      if (spec.re && !spec.re.test(v)) report('error', `${where(path)}must be ${spec.reHint}`, at, path);
      const why = spec.check?.(v);
      if (why) report('error', why, at, path);
      if (spec.prefix && (!v.startsWith(spec.prefix) || v.length < (spec.minLen || 0))) report('error', `${where(path)}must be a path under ${spec.prefix.replace(/\/$/, '')}`, at, path);
      if (spec.oneLine && v.includes('\n')) report('error', `${where(path)}must be a single line`, at, path);
      if (spec.maxLen && [...v].length > spec.maxLen) report('error', `${where(path)}at most ${spec.maxLen} characters (found ${[...v].length})`, at, path);
    }
  }
  walk(schema, root, [], 1);
  // cross-entry rules name their entries themselves: flagged so the caller does not prefix the path again
  schema.cross?.(root, (sev, msg, line, path) => report(sev, msg, line, path, true));
}

/** The choices a field offers in the Form view: fixed choices, or the keys of the table a `ref` names. */
export function choicesFor(spec, root) {
  if (!spec || spec.t !== 'str') return null;
  if (spec.choices) return spec.choices;
  if (spec.ref) { const t = entryOf(root, spec.ref)?.node; return t?.kind === 'map' ? t.entries.map((e) => e.key) : null; }
  return null;
}

// ----------------------------------------------------------------------------------------------- content checks
// What the backend refuses to save (services/common.rs find_secret): checked here so the refusal is not a surprise.
const SECRET_RULES = [
  [/-----BEGIN [A-Z ]*PRIVATE KEY-----/, 'a private key block'],
  [/\bBearer\s+[A-Za-z0-9._~+/=-]{8,}/i, "a 'Bearer <token>' literal"],
  [/(?<![A-Za-z0-9])(?:sk-|nck_|hf_|ghp_|gho_|xox[bap]-|AKIA)[A-Za-z0-9_-]{8,}/, 'a token with a well-known secret prefix (sk-, hf_, ghp_, ...)'],
  [/\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]*/, 'a JWT'],
];
const KEYVAL = /(?<![A-Za-z0-9])([\w.-]{0,40}?(?:api[_-]?key|secret|token|passw(?:or)?d|authorization))["']?\s*[=:]\s*("[^"\n]*"|'[^'\n]*'|[^\s,;#}\]"']+)/gi;
const ENV_REF = /^(?:[A-Z_][A-Z0-9_]{2,}|(?:os\.environ\/|env:|\$\{?)[A-Za-z_][A-Za-z0-9_]*\}?)$/;
const unquote = (v) => (v.length >= 2 && v[0] === v.at(-1) && (v[0] === '"' || v[0] === "'") ? v.slice(1, -1) : v);

export function contentChecks(text) {
  const out = [];
  const lines = text.split('\n');
  lines.forEach((ln, i) => {
    for (const [re, what] of SECRET_RULES) if (re.test(ln)) { out.push({ severity: 'error', line: i + 1, source: 'secret', message: `this line contains ${what}: the console refuses to save secrets; reference an environment variable NAME instead` }); return; }
    for (const m of ln.matchAll(KEYVAL)) {
      const v = unquote(m[2]);
      if (v && !ENV_REF.test(v) && [...v].length >= 8) { out.push({ severity: 'error', line: i + 1, source: 'secret', message: `'${m[1].slice(0, 40)}' has a literal value: the console refuses to save secrets; reference an environment variable NAME instead` }); return; }
    }
  });
  const nul = text.indexOf('\0');
  if (nul >= 0) out.push({ severity: 'error', line: text.slice(0, nul).split('\n').length, source: 'lint', message: 'a NUL character: the console refuses content that contains one' });
  const crlf = (text.match(/\r\n/g) || []).length, lf = (text.match(/\n/g) || []).length - crlf;
  if (crlf && lf) out.push({ severity: 'warn', line: lines.findIndex((l) => l.endsWith('\r')) + 1, source: 'lint', message: `mixed line endings: ${crlf} Windows (CRLF) and ${lf} Unix (LF) lines` });
  const invisible = /[\u00a0\u200b-\u200d\u2060\ufeff]/;
  const bad = lines.findIndex((l, i) => invisible.test(i === 0 ? l.replace(/^\ufeff/, '') : l));
  if (bad >= 0) out.push({ severity: 'warn', line: bad + 1, source: 'lint', message: 'an invisible character (no-break space or zero-width space) that looks like a plain space but is not one' });
  const trailing = [];                       // linear: one character test per line, no backtracking, no array copies
  lines.forEach((l, i) => { const e = l.endsWith('\r') ? l.length - 2 : l.length - 1; if (l[e] === ' ' || l[e] === '\t') trailing.push(i + 1); });
  if (trailing.length) out.push({ severity: 'info', line: trailing[0], source: 'lint', message: `trailing spaces on ${trailing.length === 1 ? 'this line' : `${trailing.length} lines (first here)`}` });
  return out;
}

const SEV_ORDER = { error: 0, warn: 1, info: 2 };
export const sortDiagnostics = (ds) => ds.slice().sort((a, b) => (SEV_ORDER[a.severity] - SEV_ORDER[b.severity]) || ((a.line ?? 1e9) - (b.line ?? 1e9)));

/** Every live check for one draft. */
export function checkConfig({ system, file, format, text }) {
  const diagnostics = contentChecks(text);
  const parse = parseConfig(format, text);
  for (const e of parse.errors) diagnostics.push({ severity: 'error', line: e.line, col: e.col, source: 'syntax', message: `${format === 'toml' ? 'TOML' : 'YAML'} syntax: ${e.message}` });
  for (const w of parse.warnings) diagnostics.push({ severity: 'warn', line: w.line, col: w.col, source: 'syntax', message: w.message });
  const schema = schemaFor(system, file);
  const label = (path) => path.map((p) => (typeof p === 'number' ? `#${p + 1}` : p)).join(' › ');
  if (schema && parse.root) {
    checkSchema(schema, parse.root, (severity, short, line, path, named) => diagnostics.push({ severity, line, path, source: 'schema', short,
      message: !named && path?.length ? `${label(path)}: ${short}` : short }));
  }
  // `schema` is the file's known schema even while a syntax error stops it from running (the panel says so)
  return { parse, diagnostics: sortDiagnostics(diagnostics), schema };
}
export const countBy = (ds) => ds.reduce((a, d) => { a[d.severity] = (a[d.severity] || 0) + 1; return a; }, { error: 0, warn: 0, info: 0 });
