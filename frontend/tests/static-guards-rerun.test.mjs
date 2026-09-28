// Runs the static guard suite in a child process so a regression in it fails this file too (and proves the guards still pass after dev/ changes).
import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
const dir = new URL('.', import.meta.url).pathname;
test('static-guards.test.mjs passes when run on its own', () => {
  const r = spawnSync(process.execPath, ['--test', '--test-reporter=tap', `${dir}static-guards.test.mjs`], { encoding: 'utf8', timeout: 60000, env: { ...process.env, NODE_TEST_CONTEXT: undefined } });
  assert.equal(r.status, 0, r.stdout.slice(-800)); assert.match(r.stdout, /# fail 0/); assert.match(r.stdout, /# pass [1-9]/);
});
