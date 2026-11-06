#!/usr/bin/env node
// Config editor end-to-end (part of `make e2e`, after run.mjs). Two phases against the REAL backend, hermetic like
// run.mjs (closed upstream ports, empty PATH, throwaway HOME), but with a repo root that holds four config files
// (the dev mock copies: AIPerf profiles, Model Optimizer recipes, Evaluator suites, Switchyard routes):
//
//  1. Mirror: the browser's live checks (js/cfg-checks.js) must agree with the backend's validator about whether a
//     draft has errors, on seeded random mutations of each file (values swapped for out-of-range numbers, wrong
//     types, unknown names; keys renamed; lines deleted or duplicated). The browser rules copy the backend's; this
//     fails the gate when one side changes without the other. For Switchyard the dry-run needs docker (not on the
//     hermetic PATH), so only its semantic diagnostics are compared.
//  2. Browser: the AIPerf Config tab in headless Chrome: the Form view renders, a value out of range is flagged on
//     its field and in Edit checks, a value of the wrong type is refused (never written), the Text view marks the
//     line, the server check runs, and an unsaved draft survives leaving the page and coming back.
//
//   cd frontend && node e2e/config-editor.mjs     (needs the built backend and a Chrome, like run.mjs)
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { startBackend, startChrome, Cdp } from './run.mjs';
import { SEVEN_FILES } from '../dev/mock-seven.mjs';
import { SWITCHYARD_ROUTES } from '../dev/mock-data.mjs';
import { checkConfig } from '../js/cfg-checks.js';
import { parseConfig } from '../js/cfg-parse.js';
import { spliceValue, flattenFields } from '../js/cfg-edit.js';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const MUTATIONS = Number(process.env.E2E_MIRROR_N || 150);
const FILES = [
  { system: 'aiperf', file: 'profiles', format: 'yaml', rel: 'clients/aiperf/profiles.yaml', text: SEVEN_FILES['aiperf/profiles'] },
  { system: 'modelopt', file: 'recipes', format: 'yaml', rel: 'clients/modelopt/recipes.yaml', text: SEVEN_FILES['modelopt/recipes'] },
  { system: 'evaluator', file: 'suites.yaml', format: 'yaml', rel: 'clients/nemo-evaluator/suites.yaml', text: SEVEN_FILES['evaluator/suites.yaml'] },
  { system: 'switchyard', file: 'routes', format: 'toml', rel: 'inference/switchyard/routes.toml', text: SWITCHYARD_ROUTES },
];
const YAML_VALUES = [
  '0',
  '1',
  '2',
  '8',
  '64',
  '65',
  '-1',
  '100',
  '2000',
  '2001',
  '4096',
  '5000',
  '5001',
  '32768',
  '40000',
  '0.5',
  '1.0',
  '1.5',
  '2.5',
  'true',
  'false',
  'null',
  '~',
  '',
  'chat',
  'completions',
  'chatty',
  'reap',
  'quantize',
  'minitron',
  'prune',
  'gsm8k',
  'mmlu',
  'humaneval',
  'ifeval',
  'org/name',
  'bad name',
  '/runs/x',
  '/runs/',
  '/tmp/x',
  'a'.repeat(250),
  '"x\\ny"',
  '1e3',
  '"quoted"',
  'Bad-Name',
  'yes',
];
const TOML_VALUES = [
  '"lightning"',
  '"judge"',
  '"nope"',
  '"litellm"',
  '"litellm_x"',
  '"flash-next"',
  '1',
  '0.5',
  'true',
  '"auto-local"',
  '"x"',
  '"http://litellm:4000/v1"',
  '"https://example.com/v1"',
  '"ftp://litellm/v1"',
  '"http://10.0.0.1:4000"',
  '"http://127.0.0.1:4000/v1"',
  '""',
];
// Diagnostics that come from a CLI the hermetic run cannot start (no docker on PATH), not from the file.
const NOT_THE_FILE = /could not run the authoritative dry-run|not found on PATH/;

function mutate(f, rnd) {
  let text = f.text;
  const pick = (a) => a[Math.floor(rnd() * a.length)];
  for (let o = 0, ops = 1 + Math.floor(rnd() * 2); o < ops; o++) {
    const res = parseConfig(f.format, text);
    if (!res.root) break;
    const fields = flattenFields(res.root);
    const field = pick(fields),
      lines = text.split('\n'),
      ln = field.node.line - 1,
      op = rnd();
    if (op < 0.6) text = spliceValue(text, field.node, pick(f.format === 'toml' ? TOML_VALUES : YAML_VALUES));
    else if (op < 0.75) {
      const re = f.format === 'toml' ? /^(\s*)([A-Za-z_][\w-]*)(\s*=)/ : /^(\s*(?:- )?)([A-Za-z_][\w-]*)(:)/;
      lines[ln] = lines[ln].replace(re, (_, a, k, e) => `${a}${pick([`${k}x`, k.slice(0, -1), `Name_${k}`, k.toUpperCase()])}${e}`);
      text = lines.join('\n');
    } else if (op < 0.9) {
      if (!/^\s*-/.test(lines[ln])) {
        lines.splice(ln, 1);
        text = lines.join('\n');
      }
    } else {
      lines.splice(ln, 0, lines[ln]);
      text = lines.join('\n');
    }
  }
  return text;
}

async function mirror(backend) {
  const H = { Authorization: `Bearer ${backend.key}`, 'X-Console': '1', 'Content-Type': 'application/json' };
  const failures = [],
    counts = [];
  for (const f of FILES) {
    let seed = Number(process.env.E2E_MIRROR_SEED || 1234) + f.system.length;
    const rnd = () => {
      seed = (seed * 1103515245 + 12345) % 2147483648;
      return seed / 2147483648;
    };
    let agree = 0;
    for (let m = 0; m < MUTATIONS; m++) {
      const text = m === 0 ? f.text : mutate(f, rnd);
      const r = await fetch(`${backend.base}/api/v1/systems/${f.system}/config/${f.file}/validate`, { method: 'POST', headers: H, body: JSON.stringify({ content: text }) });
      if (!r.ok) {
        failures.push(`${f.system}: validate answered ${r.status}`);
        break;
      }
      const v = await r.json();
      const server = v.diagnostics.filter((d) => d.severity === 'error' && !NOT_THE_FILE.test(d.message));
      const serverErr = server.length > 0 || (!v.ok && f.system !== 'switchyard');
      const browser = checkConfig({ system: f.system, file: f.file, format: f.format, text }).diagnostics.filter((d) => d.severity === 'error');
      if (serverErr === browser.length > 0) agree++;
      else if (failures.length < 12)
        failures.push(
          `${f.system} mutation ${m}: server ${
            serverErr
              ? `errors (${server
                  .map((d) => d.message)
                  .join('; ')
                  .slice(0, 160)})`
              : 'passes'
          }, browser ${
            browser.length
              ? `errors (${browser
                  .map((d) => d.message)
                  .join('; ')
                  .slice(0, 160)})`
              : 'passes'
          }`,
        );
    }
    counts.push(`${f.system} ${agree}/${MUTATIONS}`);
  }
  return { failures, summary: counts.join(', ') };
}

async function browser(backend, profile) {
  const failures = [];
  const chrome = await startChrome(profile);
  const cdp = await Cdp.connect(chrome.wsUrl);
  try {
    const { targetId } = await cdp.send('Target.createTarget', { url: 'about:blank' });
    const { sessionId } = await cdp.send('Target.attachToTarget', { targetId, flatten: true });
    const s = (m, p) => cdp.send(m, p, sessionId);
    await Promise.all([s('Page.enable'), s('Runtime.enable')]);
    let signedIn = false;
    cdp.on((m) => {
      if (m.sessionId !== sessionId || !signedIn) return;
      if (m.method === 'Runtime.exceptionThrown') failures.push(`uncaught exception: ${m.params.exceptionDetails.exception?.description || m.params.exceptionDetails.text}`);
      if (m.method === 'Runtime.consoleAPICalled' && m.params.type === 'error')
        failures.push(
          `console.error: ${m.params.args
            .map((a) => a.value ?? a.description ?? '')
            .join(' ')
            .slice(0, 200)}`,
        );
    });
    const ev = async (expr) => {
      const r = await s('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true });
      if (r.exceptionDetails) throw new Error(`${expr.slice(0, 60)}: ${r.exceptionDetails.exception?.description || r.exceptionDetails.text}`);
      return r.result.value;
    };
    // a page that is still rendering throws on a missing element: that is "not yet", not a failure
    const until = async (expr, what, ms = 8000) => {
      const t0 = Date.now();
      while (Date.now() - t0 < ms) {
        if (await ev(`(() => { try { return !!(${expr}); } catch { return false; } })()`)) return true;
        await sleep(100);
      }
      failures.push(`timed out waiting for ${what}`);
      return false;
    };
    const expect = (ok, what) => {
      if (!ok) failures.push(what);
    };
    await s('Page.navigate', { url: `${backend.base}/` });
    await sleep(500);
    await ev(`sessionStorage.setItem('nc.bearer', ${JSON.stringify(backend.key)}); true`);
    signedIn = true;
    await s('Page.reload', { ignoreCache: false });
    await sleep(800); // the app reads the key when it boots
    await ev(`location.hash = '#/aiperf?tab=config'; true`);
    if (!(await until(`document.querySelectorAll('.cfg-form .ff').length > 10`, 'the Form view', 12000))) {
      failures.push(`page shows: ${((await ev(`document.querySelector('#main')?.innerText`)) || '').replace(/\s+/g, ' ').slice(0, 400)}`);
      return failures;
    }
    expect(await ev(`document.querySelector('[data-view=form]').getAttribute('aria-pressed') === 'true'`), 'Form is not the default view for a YAML file');
    expect(
      await ev(`/syntax OK/.test(document.querySelector('.chk-summary').innerText) && /no problems/.test(document.querySelector('.chk-summary').innerText)`),
      'the unchanged file does not report "syntax OK, no problems"',
    );
    // Every field has an info button; its pop-up says what the key is and what a change does, and Esc closes it.
    expect(await ev(`document.querySelectorAll('.cfg-form .ff').length === document.querySelectorAll('.cfg-form .ff .ff-info').length`), 'a Form field has no info button');
    const info = JSON.stringify(JSON.stringify(['profiles', 'chat-short', 'concurrency']));
    await ev(`document.querySelector('.ff[data-path=' + JSON.stringify(${info}) + '] .ff-info').click(); true`);
    (await until(
      `document.querySelector('.ff-pop').matches(':popover-open') && /What it is/i.test(document.querySelector('.ff-pop').innerText) && /If you change it/i.test(document.querySelector('.ff-pop').innerText)`,
      'the info pop-up for concurrency to open with its description',
    )) || failures.push(`  pop-up shows: ${(await ev(`document.querySelector('.ff-pop')?.innerText || '(none)'`)).replace(/\s+/g, ' ').slice(0, 300)}`);
    expect(await ev(`/next profile run/.test(document.querySelector('.ff-pop').innerText)`), 'the info pop-up does not say when a change takes effect');
    await s('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 });
    await s('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 });
    await until(`!document.querySelector('.ff-pop').matches(':popover-open') && document.activeElement?.classList.contains('ff-info')`, 'Esc to close the info pop-up and return focus to its button');
    const conc = JSON.stringify(JSON.stringify(['profiles', 'chat-short', 'concurrency']));
    const set = (v) =>
      ev(
        `(() => { const i = document.querySelector('.ff[data-path=' + JSON.stringify(${conc}) + '] input'); i.value = ${JSON.stringify(v)}; i.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`,
      );
    await set('100');
    await until(`document.querySelector('.ff[data-path=' + JSON.stringify(${conc}) + ']').classList.contains('has-problem')`, 'the out-of-range field to be flagged');
    expect(await ev(`/1 error/.test(document.querySelector('.chk-summary').innerText)`), 'Edit checks does not count the out-of-range value');
    expect(await ev(`/unsaved/.test(document.querySelector('.cfg-status').innerText)`), 'the draft is not marked unsaved after a Form edit');
    await set('abc');
    await sleep(250);
    expect(await ev(`document.querySelector('.ff[data-path=' + JSON.stringify(${conc}) + ']').classList.contains('bad-input')`), 'a non-number in a whole-number field is not refused');
    await ev(`document.querySelector('[data-view=text]').click(); true`);
    await until(`/concurrency: 100/.test(document.querySelector('textarea.code-edit').value)`, 'the Text view to hold the last valid value (100), not the refused one');
    expect(await ev(`[...document.querySelectorAll('.code-gutter .gl.err')].length >= 1`), 'the Text view gutter does not mark the line');
    await ev(`[...document.querySelectorAll('.cfg-toolbar button')].find((b) => b.textContent === 'Check').click(); true`);
    await until(`/Validation failed/.test(document.querySelector('.chk-server').innerText)`, 'the server check to fail on concurrency 100');
    await ev(`location.hash = '#/overview'; true`);
    await sleep(800); // in-app navigation: the editor is disposed
    await ev(`location.hash = '#/aiperf?tab=config'; true`);
    await until(`/edits from earlier in this session/.test(document.querySelector('.cfg').innerText)`, 'the unsaved draft to be restored after leaving the page');
  } finally {
    cdp.close();
    chrome.proc.kill('SIGKILL');
  }
  return failures;
}

export async function main() {
  const tmp = mkdtempSync(join(tmpdir(), 'nc-e2e-cfg-'));
  for (const f of FILES) {
    mkdirSync(join(tmp, 'repo', dirname(f.rel)), { recursive: true });
    writeFileSync(join(tmp, 'repo', f.rel), f.text);
  }
  let backend,
    failures = [],
    summary = '';
  try {
    backend = await startBackend(tmp);
    const m = await mirror(backend);
    summary = m.summary;
    failures.push(...m.failures.map((x) => `mirror: ${x}`));
    failures.push(...(await browser(backend, join(tmp, 'profile'))).map((x) => `browser: ${x}`));
  } catch (e) {
    failures.push(`harness: ${e.message}`);
  } finally {
    if (backend) {
      backend.proc.kill('SIGTERM');
      for (let i = 0; i < 50 && backend.proc.exitCode === null; i++) await sleep(100);
      if (backend.proc.exitCode === null) backend.proc.kill('SIGKILL');
    }
    rmSync(tmp, { recursive: true, force: true });
  }
  if (failures.length) {
    console.error(`e2e config editor: FAILED (${failures.length})`);
    for (const f of failures) console.error(`  - ${f}`);
    if (backend) console.error(`backend log tail:\n${backend.log().slice(-1200)}`);
    return 1;
  }
  console.log(`e2e config editor: browser checks agree with the backend validators (${summary}); Form, Text, checks, info pop-ups and draft restore work in Chrome`);
  return 0;
}

if (import.meta.url === `file://${process.argv[1]}`) process.exit(await main());
