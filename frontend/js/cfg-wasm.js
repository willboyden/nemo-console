// The config editor's live checks run the backend's own validators, compiled to WebAssembly (ADR-0022): the tomllib
// and PyYAML ports, the config-file schemas and the save path's secret scan (backend/crates/nemo-console-core, built
// by `make wasm` into js/wasm/, which is generated and never committed). This module loads it once, when a Config
// tab opens, and wraps its four functions. Contract: backend/PORTING.md A-10 (every answer carries `api`).
//
//   loadWasm({ bytes }?) -> Promise<state>   in the browser it fetches js/wasm/; Node tests pass the bytes
//   wasmState() -> { state: 'idle' | 'loading' | 'ready' | 'failed', error, readyOnce }
//   wasmRetry()  start a fresh instance after a failed one (the editor calls it once per draft change)
//   onWasmState(fn) -> unsubscribe
//   wasmCheck(system, file, format, text) / wasmPlainType(raw) / wasmRoundtrip(...) / wasmConstants() -> object | null
//
// There is no fallback copy of the rules: when the module cannot load, or traps (a Rust panic aborts), every function
// returns null and the editor says "live checks unavailable". The server's Check and every Apply still validate. After
// a trap the module is instantiated again from the compiled code, so the next draft is checked by a fresh instance.
export const API = 1;
const GLUE = './wasm/nemo_console_wasm.js';
const BIN = './wasm/nemo_console_wasm_bg.wasm';

let glue = null; // the wasm-bindgen glue of the live instance
let compiled = null; // the compiled module, kept to instantiate again after a trap
let state = 'idle';
let error = null;
let pending = null;
let gen = 0;
let constants = null;
let readyOnce = false; // an instance has answered at least once (before that, 'loading' is the first load)
let failNext = false; // test seam: the next instantiation traps
const plainCache = new Map();
const listeners = new Set();

function set(s, e = null) {
  state = s;
  error = e;
  for (const fn of listeners) fn(wasmState());
}
export const wasmState = () => ({ state, error, readyOnce });
export function onWasmState(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

async function compile(bytes) {
  if (bytes) return WebAssembly.compile(bytes);
  const res = await fetch(new URL(BIN, import.meta.url));
  if (!res.ok) throw new Error(`${BIN} answered ${res.status}: build it with \`make wasm\``);
  return WebAssembly.compileStreaming ? WebAssembly.compileStreaming(res) : WebAssembly.compile(await res.arrayBuffer());
}

// A fresh copy of the glue module per instance (its `wasm` handle is module-level and cannot be replaced otherwise).
async function instantiate(bytes) {
  compiled ||= await compile(bytes);
  if (failNext) {
    failNext = false;
    throw new WebAssembly.RuntimeError('unreachable');
  }
  const g = await import(`${GLUE}?instance=${++gen}`);
  g.initSync({ module: compiled });
  const v = g.api_version();
  if (v !== API) throw new Error(`the WebAssembly checks speak contract ${v}, this page ${API}: rebuild with \`make wasm\``);
  glue = g;
  plainCache.clear();
  readyOnce = true;
  set('ready');
}

/** Load (once) and instantiate the module. Resolves to the state; never rejects. */
export function loadWasm({ bytes } = {}) {
  if (state === 'ready') return Promise.resolve(wasmState());
  if (!pending) {
    set('loading');
    pending = instantiate(bytes)
      .catch((e) => {
        glue = null;
        set('failed', String(e?.message || e));
      })
      .then(() => {
        pending = null;
        return wasmState();
      });
  }
  return pending;
}

// Start a fresh instance from the compiled module (after a trap); at most one at a time.
function reinstantiate() {
  if (!compiled || pending || glue) return;
  set('loading');
  pending = instantiate()
    .catch((e) => set('failed', String(e?.message || e)))
    .then(() => {
      pending = null;
    });
}

/**
 * One more attempt after a failed re-instantiation (a trap while starting the fresh instance would otherwise leave
 * the checks off for good). The editor calls it once per draft change, so the retries are bounded by edits.
 */
export function wasmRetry() {
  if (state === 'failed') reinstantiate();
}

function call(name, ...args) {
  if (!glue) return null;
  try {
    const out = JSON.parse(glue[name](...args));
    if (out.api !== API) throw new Error(`contract ${out.api} from ${name}, expected ${API}`);
    return out;
  } catch (e) {
    // a trap (unreachable after a Rust panic) leaves the instance unusable: drop it and start a fresh one
    glue = null;
    set('failed', `${name}: ${String(e?.message || e)}`);
    reinstantiate();
    return null;
  }
}

/** Every live check for one draft: { schema, server_adds, syntax: 'ok'|'error'|'n/a', diagnostics } or null. */
export const wasmCheck = (system, file, format, text) => call('check', String(system), String(file), String(format), String(text));

/** How PyYAML types a plain scalar: { type, text } or null. Cached: a draft repeats the same words. */
export function wasmPlainType(raw) {
  const s = String(raw);
  const hit = plainCache.get(s);
  if (hit) return hit;
  const out = call('yaml_plain_type', s);
  if (out && plainCache.size < 5000) plainCache.set(s, out);
  return out;
}

/** After a Form edit: { ok, problem, got } or null (see PORTING.md A-10 for `expected`). */
export const wasmRoundtrip = (format, before, after, path, expected) => call('roundtrip', format, before, after, JSON.stringify(path), JSON.stringify(expected));

/** The schema constants the Form describes fields with (cached per instance), or null. */
export function wasmConstants() {
  if (!glue) return null;
  if (!constants) constants = call('schema_constants');
  return constants;
}

/** Test seam: make the live instance trap on its next call, as a Rust panic would (tests/cfg-wasm.test.mjs). */
export const _test = {
  breakInstance() {
    const trap = () => {
      throw new WebAssembly.RuntimeError('unreachable');
    };
    glue = { check: trap, yaml_plain_type: trap, roundtrip: trap, schema_constants: trap };
    plainCache.clear();
    constants = null;
  },
  /** Make the next instantiation trap too (a second trap while the fresh instance starts). */
  failNextInstantiate() {
    failNext = true;
  },
};
