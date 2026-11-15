// Load the config validators' WebAssembly build (`make wasm`) for the node tests. A missing build FAILS the tests that
// import this (it never skips them): the live checks have no JS fallback to test instead.
import { readFileSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { loadWasm } from '../js/cfg-wasm.js';

export const WASM_BIN = fileURLToPath(new URL('../js/wasm/nemo_console_wasm_bg.wasm', import.meta.url));

export async function wasmReady() {
  if (!existsSync(WASM_BIN)) throw new Error(`${WASM_BIN} is missing: run \`make wasm\` (the tests need the real validators)`);
  const s = await loadWasm({ bytes: readFileSync(WASM_BIN) });
  if (s.state !== 'ready') throw new Error(`the WebAssembly checks did not load: ${s.error}`);
  return s;
}
