// Live edit checks for the config editor. The verdicts come from the backend's own code, compiled to WebAssembly
// (cfg-wasm.js, ADR-0022): the save path's refusals (secrets, NUL), the real tomllib / PyYAML loaders, and the file's
// schema validator where the console has one (AIPerf profiles, Model Optimizer recipes, Evaluator suites, Switchyard
// and Relay routes). This module adds only what has no server rule (line endings, invisible characters, trailing
// spaces, duplicate or boolean-looking keys) and the field descriptions the Form view shows. Pure logic, no DOM.
//
//   checkConfig({ system, file, format, text }) -> { parse, diagnostics, schema, live, syntax, serverAdds }
//   diagnostic: { severity: 'error' | 'warn' | 'info', line, col?, message, source }
//   live: 'ok' | 'loading' | 'unavailable'   syntax: 'ok' | 'error' | 'n/a' | null (null: unknown, the checks did not run)
//
// While the WebAssembly module is not available there is no verdict and no silent pass: `live` is 'unavailable', a
// warning says so, and the server's Check (and every Apply) still validates. The backend stays the authority.
import { parseConfig, entryOf } from './cfg-parse.js';
import { wasmCheck, wasmConstants, wasmRetry, wasmState } from './cfg-wasm.js';
/** A schema table's OWN entry for a file-controlled key: `constructor`, `__proto__` or `toString` never hit Object.prototype. */
const own = (obj, k) => (obj && Object.hasOwn(obj, k) ? obj[k] : undefined);

// ---------------------------------------------------------------------------------------------- field descriptions
// What the Form view shows next to a field: its kind, range or choices, and a hint. The numbers and lists come from
// the validators themselves (wasmConstants); nothing here decides whether a value is valid.
// map:  fixed keys, `pattern` keys matched by a regex   dict: user-named entries   list: items
// int / num / bool / str: a scalar; `ref` names a top-level table whose keys are the choices (Switchyard targets)
const map = (keys, o = {}) => ({ t: 'map', keys, ...o });
const dict = (values, o = {}) => ({ t: 'dict', values, ...o });
const list = (items, o = {}) => ({ t: 'list', items, ...o });
const int = (min, max, o = {}) => ({ t: 'int', min, max, ...o });
const num = (o = {}) => ({ t: 'num', ...o });
const bool = (o = {}) => ({ t: 'bool', ...o });
const str = (o = {}) => ({ t: 'str', ...o });
const one = (o = {}) => ({ t: 'const', value: 1, help: 'must be 1', ...o });

const AIPERF_HELP = {
  concurrency: 'requests kept in flight',
  requests: 'measured requests',
  isl: 'synthetic input tokens (mean)',
  osl: 'output tokens requested (mean)',
  warmup_requests: 'default 0',
  timeout_s: 'per-request timeout in seconds, default 120',
  seed: 'synthetic-prompt seed, default 42',
};
function aiperfSchema(c) {
  return map(
    {
      version: one(),
      profiles: dict(
        map({
          description: str({ help: 'one line, at most 300 characters' }),
          model: str({ reHint: 'a router model alias (letters, digits and . _ : / @ + -)' }),
          endpoint_type: str({ choices: c.ENDPOINT_TYPES, help: 'default chat' }),
          streaming: bool({ help: 'default true; time to first token and inter-token latency need streaming' }),
          ...Object.fromEntries(c.INT_KEYS.map(([k, lo, hi]) => [k, int(lo, hi, { help: AIPERF_HELP[k] })])),
        }),
      ),
    },
    { id: 'aiperf_profiles' },
  );
}
function modeloptSchema(c) {
  const fields = {
    method: str({ choices: c.METHODS, help: 'what the job does' }),
    base_model: str({ reHint: 'a Hugging Face repo id (org/name)', help: 'read from the offline cache' }),
    output: str({ prefix: '/runs/', help: "a path under /runs (the job's run dir)" }),
    description: str(),
    preset: str({ help: 'quantization preset, e.g. NVFP4' }),
    sparsity: num({ min: 0, maxExcl: 1, help: 'from 0 (inclusive) to 1 (exclusive): the fraction of weights REAP keeps' }),
    config: str({ help: 'a file name in the run dir (Minitron config)' }),
  };
  // only the keys the validator accepts (RECIPE_KEYS), in its order
  const keys = Object.fromEntries(c.RECIPE_KEYS.map((k) => [k, own(fields, k) || str()]));
  return map({ version: one(), recipes: dict(map(keys)) }, { id: 'modelopt_recipes' });
}
function evaluatorSchema(c) {
  return map(
    {
      version: one(),
      suites: dict(
        map({
          description: str({ help: 'at most 200 characters' }),
          model: str({ reHint: 'a router model alias (letters, digits and . _ / : @ -)' }),
          benchmarks: list(
            map({
              bench: str({ choices: c.SUPPORTED, help: 'datasets must be prefetched first' }),
              max_problems: int(1, c.MAX_PROBLEMS, { help: 'a bounded, shuffled sample (seed 42)' }),
              max_tokens: int(16, 32768, { nullable: true }),
              temperature: num({ min: 0, max: 2, nullable: true }),
              repeats: int(1, 8, { help: 'default 1' }),
            }),
          ),
        }),
      ),
    },
    { id: 'evaluator_suites' },
  );
}
// Switchyard routes (also the Relay plugin's copy): llm clients, targets and routes that name each other.
const targetRef = (help) => str({ ref: 'targets', help });
const any = (help) => ({ t: 'any', help });
const switchyardSchema = map({
  llm_clients: dict(map({ base_url: str({ help: 'loopback or an in-network service name (http or https)' }) })),
  targets: dict(map({ llm_client: str({ ref: 'llm_clients', help: 'one of the [llm_clients.*] blocks' }), id: any('the model id sent upstream') })),
  routes: dict(map({ classifier: map({ target: targetRef('the judge target') }) }, { pattern: [[/^(?:target|.+_target)$/, targetRef('one of the [targets.*] entries')]] })),
});

/** The field descriptions the console knows for one config file (system id, file id), or null. */
export function schemaFor(system, file) {
  if ((system === 'switchyard' || system === 'relay') && file === 'routes') return { ...switchyardSchema, id: `${system}_routes` };
  const c = wasmConstants();
  if (!c) return null;
  if (system === 'aiperf' && file === 'profiles') return aiperfSchema(c.aiperf);
  if (system === 'modelopt' && file === 'recipes') return modeloptSchema(c.modelopt);
  if (system === 'evaluator' && file === 'suites.yaml') return evaluatorSchema(c.evaluator);
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

/** The choices a field offers in the Form view: fixed choices, or the keys of the table a `ref` names. */
export function choicesFor(spec, root) {
  if (!spec || spec.t !== 'str') return null;
  if (spec.choices) return spec.choices;
  if (spec.ref) {
    const t = entryOf(root, spec.ref)?.node;
    return t?.kind === 'map' ? t.entries.map((e) => e.key) : null;
  }
  return null;
}

// ----------------------------------------------------------------------------------------------- hygiene hints
/** Things no server rule refuses but that bite later: mixed line endings, look-alike spaces, trailing spaces. */
export function lintChecks(text) {
  const out = [];
  const lines = text.split('\n');
  const crlf = (text.match(/\r\n/g) || []).length,
    lf = (text.match(/\n/g) || []).length - crlf;
  if (crlf && lf) out.push({ severity: 'warn', line: lines.findIndex((l) => l.endsWith('\r')) + 1, source: 'lint', message: `mixed line endings: ${crlf} Windows (CRLF) and ${lf} Unix (LF) lines` });
  const invisible = /[ ​-‍⁠﻿]/;
  const bad = lines.findIndex((l, i) => invisible.test(i === 0 ? l.replace(/^﻿/, '') : l));
  if (bad >= 0) out.push({ severity: 'warn', line: bad + 1, source: 'lint', message: 'an invisible character (no-break space or zero-width space) that looks like a plain space but is not one' });
  const trailing = []; // linear: one character test per line, no backtracking, no array copies
  lines.forEach((l, i) => {
    const e = l.endsWith('\r') ? l.length - 2 : l.length - 1;
    if (l[e] === ' ' || l[e] === '\t') trailing.push(i + 1);
  });
  if (trailing.length)
    out.push({ severity: 'info', line: trailing[0], source: 'lint', message: `trailing spaces on ${trailing.length === 1 ? 'this line' : `${trailing.length} lines (first here)`}` });
  return out;
}

const SEV_ORDER = { error: 0, warn: 1, info: 2 };
export const sortDiagnostics = (ds) => ds.slice().sort((a, b) => SEV_ORDER[a.severity] - SEV_ORDER[b.severity] || (a.line ?? 1e9) - (b.line ?? 1e9));
/** The warning shown while the WebAssembly checks are not available: never a silent pass. */
export const LIVE_UNAVAILABLE = 'live checks unavailable: the browser could not run the console validators (WebAssembly). The server check still runs on Check and before every Apply.';

/** Every live check for one draft. */
export function checkConfig({ system, file, format, text }) {
  wasmRetry(); // once per draft change: a module that failed to start again gets one more try
  const parse = parseConfig(format, text);
  const diagnostics = lintChecks(text);
  // the layout parser's own hints (duplicate keys, keys PyYAML reads as booleans); its syntax errors are not shown:
  // the real loader's verdict below is the one that counts
  for (const w of parse.warnings) diagnostics.push({ severity: 'warn', line: w.line, col: w.col, source: 'lint', message: w.message });
  const w = wasmCheck(system, file, format, text);
  if (!w) {
    // the first load (the module is being fetched and compiled): not a failure yet, and no verdict either
    const s = wasmState();
    const live = !s.readyOnce && (s.state === 'idle' || s.state === 'loading') ? 'loading' : 'unavailable';
    if (live === 'unavailable') diagnostics.push({ severity: 'warn', line: null, source: 'live', message: LIVE_UNAVAILABLE });
    return { parse, diagnostics: sortDiagnostics(diagnostics), schema: schemaFor(system, file), live, syntax: null, serverAdds: [] };
  }
  for (const d of w.diagnostics) diagnostics.push({ severity: d.severity, line: d.line ?? null, source: d.source, message: d.message });
  const known = schemaFor(system, file);
  const schema = w.schema ? { ...(known || {}), id: w.schema } : known;
  return { parse, diagnostics: sortDiagnostics(diagnostics), schema, live: 'ok', syntax: w.syntax, serverAdds: w.server_adds || [] };
}
export const countBy = (ds) =>
  ds.reduce(
    (a, d) => {
      a[d.severity] = (a[d.severity] || 0) + 1;
      return a;
    },
    { error: 0, warn: 0, info: 0 },
  );
