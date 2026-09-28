import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const en = JSON.parse(fs.readFileSync(path.join(root, 'i18n/en.json'), 'utf8'));
const walk = (d) => fs.readdirSync(d, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? walk(path.join(d, e.name)) : e.name.endsWith('.js') ? [path.join(d, e.name)] : []));
const files = walk(path.join(root, 'js'));
const rel = (f) => path.relative(root, f);

test('every literal t("key") and key-like string used in js/ exists in en.json', () => {
  const missing = [];
  const ns = new Set(Object.keys(en).map((k) => k.split('.')[0]));
  const dynamic = /^(status|params|hist|audit|settings|ev|routing|relay|dyn|cfg|val|apply|act|pg|diff|conflict|topo|overview|sys|tab|chart|tokens|window|unit|common|auth|topbar|role|nav|err|health|comp|kind|sev|danger|problem|postcheck|confirm|group)\./;
  for (const f of files) {
    const src = fs.readFileSync(f, 'utf8'), keys = new Set();
    for (const m of src.matchAll(/\bt\(\s*['"`]([a-z_]+\.[a-z_0-9.]+)['"`]/g)) keys.add(m[1]);
    for (const m of src.matchAll(/['"]([a-z_]+\.[a-z_0-9]+)['"]/g)) if (ns.has(m[1].split('.')[0]) && dynamic.test(m[1])) keys.add(m[1]);
    // Metric keys / event kinds share the namespace prefixes but are not message keys.
    const notMessage = /^(routing\.decision|dyn\.(ttft|itl)(_ms)?|dyn\.inflight|dyn\.kv_cache_usage|dyn\.requests_total|relay\.(atof_bytes|atif_trajectories|exporters_enabled))$/;
    for (const k of keys) if (!notMessage.test(k) && !(k in en) && !(`${k}_one` in en) && !/\.(js|json|md)$/.test(k)) missing.push(`${rel(f)}: ${k}`);
  }
  assert.deepEqual(missing, []);
});
test('dynamic key families are complete', () => {
  const fam = { 'nav.': ['overview', 'switchyard', 'relay', 'dynamo', 'audit', 'settings'], 'sys.': ['switchyard', 'relay', 'dynamo'], 'health.': ['up', 'degraded', 'down', 'unknown'], 'comp.': ['running', 'stopped', 'failed', 'absent', 'unknown'],
    'kind.': ['container', 'systemd', 'process', 'http'], 'sev.': ['info', 'warn', 'error'], 'danger.': ['safe', 'disruptive', 'destructive'], 'postcheck.': ['healthy', 'unhealthy', 'skipped'], 'window.': ['15m', '1h', '24h'],
    'tokens.': ['prompt', 'completion', 'reasoning', 'cached', 'total'], 'act.danger_': ['disruptive', 'destructive'], 'dyn.effect_': ['up', 'down', 'restart'], 'ev.state_': ['live', 'connecting', 'reconnecting', 'paused'], 'relay.att_': ['verified', 'failed', 'unknown', 'unreported', 'reported'],
    'diff.': ['add', 'del', 'eq'], 'role.': ['admin', 'viewer'], 'chart.dyn.': ['ttft', 'itl', 'inflight', 'kv'], 'err.': ['401', '403', '409', '413', '415', '421', '429', '502', '503', 'network'], 'auth.': ['none', 'bearer', 'forwarded'] };
  for (const [p, ks] of Object.entries(fam)) for (const k of ks) assert.ok(`${p}${k}` in en, `${p}${k}`);
});
test('no en.json value is empty', () => { for (const [k, v] of Object.entries(en)) assert.ok(typeof v === 'string' && v.length, k); });

test('CSP hygiene: no inline handlers, style attributes, eval, document.write or external origins in js/css/html', () => {
  const sources = [...files, path.join(root, 'index.html'), ...fs.readdirSync(path.join(root, 'css')).map((x) => path.join(root, 'css', x))];
  for (const f of sources) {
    const src = fs.readFileSync(f, 'utf8');
    const bad = [...src.matchAll(/https?:\/\/[^\s'"`)]+/g)].map((m) => m[0]).filter((u) => !u.startsWith('http://www.w3.org/2000/svg'));
    assert.deepEqual(bad, [], `${rel(f)} references an external origin`);
    if (f.endsWith('.js')) {
      assert.ok(!/\beval\s*\(|new Function\s*\(|document\.write|insertAdjacentHTML|outerHTML\s*=|setAttribute\(\s*['"]style['"]/.test(src), `${rel(f)} uses a CSP-forbidden API`);
      assert.ok(!/\bstyle\s*:\s*['"`{]/.test(src.replace(/\/\/.*$/gm, '')) || !/h\(\s*'[a-z]+'\s*,\s*\{[^}]*\bstyle\s*:/.test(src), `${rel(f)} passes a style attribute to h()`);
      assert.ok(!/['"]on[a-z]+=/i.test(src.replace(/onerror=|onload=|onclick=|onerror=1/g, (m) => m)) || rel(f) === 'js/dom.js', `${rel(f)} has an inline handler string`);
    }
  }
  const html = fs.readFileSync(path.join(root, 'index.html'), 'utf8');
  assert.ok(!/<script(?![^>]*\bsrc=)[^>]*>/i.test(html), 'inline <script> in index.html'); assert.ok(!/\sstyle=/.test(html) && !/<style/i.test(html) && !/\son\w+=/i.test(html));
});
test('innerHTML is used only by setTrustedHtml in dom.js', () => {
  for (const f of files) { const src = fs.readFileSync(f, 'utf8'); const n = (src.match(/\.innerHTML\s*=/g) || []).length; assert.equal(n, rel(f) === 'js/dom.js' ? 1 : 0, rel(f)); }
  const callers = files.filter((f) => /setTrustedHtml\(/.test(fs.readFileSync(f, 'utf8')) && !f.endsWith('dom.js')).map(rel).sort();
  // Every caller feeds it only chart-builder output (charts.js escapes all dynamic text; see metrics.test.mjs XSS corpus).
  for (const f of callers) for (const line of fs.readFileSync(path.join(root, f), 'utf8').split('\n').filter((l) => /setTrustedHtml\(/.test(l) && !/import/.test(l))) assert.match(line, /lineChart\(|shareBars\(|legend\(/, `${f}: ${line.trim().slice(0, 80)}`);
});
test('the h() helper refuses a style attribute', () => { assert.match(fs.readFileSync(path.join(root, 'js/dom.js'), 'utf8'), /k === 'style'\) throw/); });
test('every JS import resolves to an existing file', () => {
  for (const f of files) for (const m of fs.readFileSync(f, 'utf8').matchAll(/from\s+'(\.[^']+)'|import\s+'(\.[^']+)'/g)) assert.ok(fs.existsSync(path.resolve(path.dirname(f), m[1] || m[2])), `${rel(f)} -> ${m[1] || m[2]}`);
});
