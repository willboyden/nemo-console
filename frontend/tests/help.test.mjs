import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { helpProblems, richRuns, filterFaq, CONSOLE_KEY } from '../js/help.js';
import { SYSTEM_IDS, SEVEN_IDS } from '../js/systems.js';
import { ROUTES } from '../js/routes.js';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const doc = JSON.parse(fs.readFileSync(path.join(root, 'i18n/help.en.json'), 'utf8'));

// The tab ids each system page can show (views/*.js; the generic view builds Config and Playground only when the
// backend offers them). Every page also gets `start` and `faq` from system-page.js.
const HELP_TABS = ['start', 'faq'];
const TABS = {
  switchyard: ['routing', 'config', 'playground', 'actions'],
  relay: ['status', 'config', 'diagnostics', 'playground'],
  dynamo: ['status', 'params', 'lifecycle', 'playground'],
  skillevaluator: ['status', 'evaluate'],
  datadesigner: ['status', 'scripts', 'actions', 'runs'],
  ...Object.fromEntries(SEVEN_IDS.map((id) => [id, ['status', 'config', 'playground', 'actions']])),
};
const tabsBySystem = Object.fromEntries(Object.entries(TABS).map(([k, v]) => [k, [...v, ...HELP_TABS]]));

test('the tab table covers every managed system', () => {
  assert.deepEqual(Object.keys(TABS).sort(), [...SYSTEM_IDS].sort());
});

test('the tab table matches the tab ids the views define', () => {
  const read = (f) => fs.readFileSync(path.join(root, f), 'utf8');
  const ids = (src) => [...src.matchAll(/\{\s*id:\s*'([a-z_]+)',\s*label:\s*t\('tab\./g)].map((m) => m[1]);
  for (const id of ['switchyard', 'relay', 'dynamo', 'skillevaluator', 'datadesigner']) assert.deepEqual(ids(read(`js/views/${id}.js`)).sort(), [...TABS[id]].sort(), id);
  // The generic view: Status, then Config / Playground when offered, then Actions.
  const gen = read('js/components/generic-system.js');
  assert.deepEqual(ids(gen).sort(), ['actions', 'config', 'playground', 'status']);
  // The help tabs come from the page frame, after the view's own.
  assert.deepEqual(ids(read('js/components/system-page.js')), HELP_TABS);
});

test('help.en.json has a valid guide and FAQ for every system and the console questions', () => {
  assert.deepEqual(helpProblems(doc, tabsBySystem, ROUTES.map((r) => r.path)), []);
});

test('helpProblems catches the mistakes it is meant to catch', () => {
  const bad = {
    [CONSOLE_KEY]: { faq: [{ q: 'Q', a: 'A', tab: 'status' }] },
    switchyard: { lead: 'x', steps: [{ title: 't', body: 'b', tab: 'nope' }], faq: [{ q: 'q', a: 'see https://example.org' }, { q: 'q', a: 'odd `tick' }, { q: 'r', a: 'a', route: '/nowhere' }, { q: 's', a: 'a', extra: 1 }] },
    nosuch: {},
  };
  const out = helpProblems(bad, { switchyard: ['routing', 'start', 'faq'], relay: ['status'] }, ['/settings']);
  for (const want of ['nosuch: not a system', 'only for system pages', 'fewer than 3 steps', 'external URL', 'unbalanced backtick', 'unknown route /nowhere', 'unknown key extra', 'duplicate question', 'relay: no entry']) {
    assert.ok(out.some((p) => p.includes(want)), `${want} in ${JSON.stringify(out)}`);
  }
});

test('richRuns splits paragraphs and inline code, and never yields markup', () => {
  assert.deepEqual(richRuns('Run `make all` now.\n\nThen <b>stop</b>.'), [
    [{ code: false, text: 'Run ' }, { code: true, text: 'make all' }, { code: false, text: ' now.' }],
    [{ code: false, text: 'Then <b>stop</b>.' }],
  ]);
  assert.deepEqual(richRuns(''), []);
  assert.deepEqual(richRuns(undefined), []);
});

test('filterFaq matches every word in question, answer or command, case-insensitively', () => {
  const items = [{ q: 'Up timed out', a: 'Raise the timeout.' }, { q: 'Why 409?', a: 'stale base', cmd: 'docker ps' }];
  assert.equal(filterFaq(items, '').length, 2);
  assert.deepEqual(filterFaq(items, 'TIMEOUT').map((i) => i.q), ['Up timed out']);
  assert.deepEqual(filterFaq(items, '409 stale').map((i) => i.q), ['Why 409?']);
  assert.deepEqual(filterFaq(items, 'docker').map((i) => i.q), ['Why 409?']);
  assert.deepEqual(filterFaq(items, 'timeout 409'), []);
});
