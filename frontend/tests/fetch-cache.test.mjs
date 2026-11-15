// The cache behind the wave-3 run detail cards (addendum 9.15): one fetch per run, shared by every redraw; a failure is not asked for again at once but can be retried.
import test from 'node:test';
import assert from 'node:assert/strict';
import { fetchCache } from '../js/fetch-cache.js';

const tick = () => new Promise((r) => setTimeout(r, 0));

test('a key is fetched once; the entry is loading, then ok with its value; onChange is called once when it settles', async () => {
  let calls = 0;
  const c = fetchCache(async (k) => {
    calls++;
    return { k };
  });
  assert.equal(c.peek('a').state, 'none');
  let changed = 0;
  const e = c.load('a', () => changed++);
  assert.equal(e.state, 'loading');
  c.load('a', () => changed++); // a second caller while it loads is told too
  await tick();
  await tick();
  assert.equal(calls, 1);
  assert.equal(changed, 2);
  assert.deepEqual(c.peek('a').value, { k: 'a' });
  assert.equal(c.peek('a').state, 'ok');
  assert.equal(c.load('a', () => changed++).state, 'ok', 'a cached value is returned as is');
  await tick();
  assert.equal(changed, 2, 'and nobody is called back for it');
  assert.equal(calls, 1);
  assert.equal(c.size(), 1);
});

test('a failure is kept (the error is readable), not re-fetched within retryAfterMs, fetched again after it, and retry() forgets it at once', async () => {
  let now = 1000,
    calls = 0,
    fail = true;
  const c = fetchCache(
    async () => {
      calls++;
      if (fail) throw Object.assign(new Error('nope'), { status: 404 });
      return 'fine';
    },
    { retryAfterMs: 5000, now: () => now },
  );
  c.load('x');
  await tick();
  await tick();
  assert.equal(c.peek('x').state, 'fail');
  assert.equal(c.peek('x').error.status, 404);
  c.load('x');
  await tick();
  assert.equal(calls, 1, 'not asked again within the window');
  now += 6000;
  fail = false;
  c.load('x');
  await tick();
  await tick();
  assert.equal(calls, 2);
  assert.equal(c.peek('x').state, 'ok');
  assert.equal(c.peek('x').value, 'fine');
  fail = true;
  c.load('y');
  await tick();
  await tick();
  assert.equal(c.peek('y').state, 'fail');
  fail = false;
  c.retry('y');
  await tick();
  await tick();
  assert.equal(c.peek('y').state, 'ok');
  assert.equal(calls, 4);
});

test('a fetcher that throws synchronously is a failure, not an exception in the caller', async () => {
  const c = fetchCache(() => {
    throw new Error('boom');
  });
  assert.doesNotThrow(() => c.load('z'));
  await tick();
  await tick();
  assert.equal(c.peek('z').state, 'fail');
});
