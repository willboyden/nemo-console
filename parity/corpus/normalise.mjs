// EVERY normalisation the parity harness applies, in one place, each with its reason. Only genuinely volatile values
// are touched: time, generated ids, durations, the per-backend temp dirs and ports. Everything else is compared as is.
// A rule never hides a TYPE difference: a volatile number becomes 0 of the SAME kind (int stays int, float stays
// float), a volatile string becomes a placeholder only when both sides hold a string.
import { mapTree, parse, stringify } from '../lib/json.mjs';

/** One normalisation rule, documented. `kind`: where it applies. */
export const RULES = [
  { id: 'tmp-paths', kind: 'string', why: 'each backend runs in its own temp dir; its absolute paths (state dir, repo root, HOME, ...) differ only by that prefix' },
  { id: 'ports', kind: 'string', why: 'each backend has its own console port and its own fake upstream ports; `127.0.0.1:<port>` becomes `127.0.0.1:<console>` / `127.0.0.1:<up:NAME>`' },
  { id: 'generated-ids', kind: 'string', why: 'ids generated per process (uuid4 revision ids, `key:<12 hex>` key ids, run ids, random temp-file names) are alpha-renamed in order of first appearance per backend and per id class (`<rev:N>`, `<kid:N>`, `<run:N>`, ...), so a later reference to the same id still has to match' },
  { id: 'ages', kind: 'string', why: '"N s ago" / "N min ago" text is computed from the wall clock at request time' },
  { id: 'measured-text', kind: 'string', why: 'durations measured during a request and printed in text: the per-probe "<N> ms" of the guardrails check table, "PASS in <N> s" (dcgm diag), "verdict: x · <N> s" (evaluator report), "answers again after <N> s" (dcgm restart), "running for <N> min" (live runs)' },
  { id: 'epoch-now', kind: 'string', why: 'unix times of the current day with a fraction in strings / argv (dynamo `docker logs --since <time.time():.3f>`)' },
  { id: 'volatile-metrics', kind: 'json', why: 'metric points whose value is an age measured from the wall clock (see VOLATILE_METRIC_KEYS): {key, value} objects get value 0 of the same kind' },
  { id: 'run-json-size', kind: 'json', why: 'the size of a stored run.json (Data Designer /runs/{id}/files: {path: ".../run.json", bytes}) changes with the digits of its ts and duration_ms; bytes becomes 0 of the same kind' },
  { id: 'iso-now', kind: 'string', why: 'ISO-8601 timestamps of the current day (2026-..T..) inside strings are wall-clock stamps; dates from the fixtures are older (2026-09/10) and are kept: only stamps within 2 days of the run are replaced' },
  { id: 'time-keys', kind: 'json', why: 'keys ts, checked_at, mtime, at, created, created_at, started_at, finished_at, updated_at, last_seen hold wall-clock time; the value becomes 0 of the same number kind (or "<time>" for a string)' },
  { id: 'duration-keys', kind: 'json', why: 'duration_ms, ms, elapsed_ms, elapsed_s, duration_s are measured durations; the value becomes 0 of the same kind' },
  { id: 'prom-created', kind: 'prometheus', why: 'prometheus_client `*_created` samples are process start times' },
  { id: 'prom-latency', kind: 'prometheus', why: 'nemo_console_http_request_seconds _bucket/_sum values are measured latencies (the _count is compared)' },
  { id: 'prom-order', kind: 'prometheus', why: 'exposition lines are compared as a sorted multiset: scrapers do not depend on the order of families or samples' },
  { id: 'cli-cached', kind: 'argv', why: 'argv the Python adapters cache with a TTL (see CACHED_ARGV) run or not depending on wall-clock time; they are compared over the whole run (distinct set), not per case' },
  { id: 'http-cached', kind: 'http', why: 'upstream requests behind a TTL cache (see CACHED_HTTP) likewise: compared over the whole run, not per case' },
  { id: 'http-query', kind: 'http', why: 'an upstream query string is compared decoded (key/value pairs): `~` vs `%7E` or `+` vs `%20` is HTTP-client encoding, not behaviour' },
];

// TTL-cached read-only probes in the Python adapters (the TTL is wall-clock based, so whether the call happens inside
// a given case depends on timing). Each entry: [regex over the argv joined by spaces, where cached + TTL].
export const CACHED_ARGV = [
  [/^\S+ --version$/, 'version probes of the CLIs: VERSION_TTL 300 s (relay, skillevaluator, datadesigner, openshell, aiperf, guardrails, evaluator)'],
  [/^docker inspect -f \{\{\.State\.Status\}\} /, 'services/engines.py EngineProbe: 5 s per container (topology + Switchyard capable tier)'],
  [/^docker inspect --format \{\{range \$k, \$v := \.NetworkSettings\.Networks\}\}/, 'guardrails isolation probe: ISOLATION_TTL 60 s'],
  [/^docker network inspect --format \{\{\.Internal\}\} /, 'guardrails isolation probe (networks): ISOLATION_TTL 60 s'],
  [/^docker image inspect --format \{\{\.Id\}\} /, 'dcgm diag-image check: 60 s'],
  [/^openshell -g openshell sandbox list /, 'openshell sandbox snapshot: SNAPSHOT_TTL 2 s'],
  [/^docker inspect --format \{\{\.Name\}\}\|\{\{\.State\.Status\}\}/, 'openshell gateway-running check of the event poll: 10 s'],
  [/^evaluator-local cached --json$/, 'evaluator cached datasets: CACHED_TTL 20 s'],
  [/^docker exec ai-lab-switchyard-switchyard-1 tail -n 2000 /, 'switchyard sy.route.calls (ROUTE_COUNT_WINDOW lines): 15 s'],
  [/^docker compose .* config --format json$/, 'dynamo worker GPU UUIDs from the merged compose config: UUID_TTL 300 s'],
  [/^nemo-relay plugins list --json$/, 'relay _cached("plugins_list"): 20 s'],
];
export const CACHED_HTTP = [
  [/^litellm GET \/health\/liveliness$/, 'topology: TTL 5 s (and the per-adapter router probes share the fake)'],
];

// metric points ({key, value, ...}) whose value is now() minus a fixed time: an uptime or an age
export const VOLATILE_METRIC_KEYS = new Set(['os.gateway_uptime_s', 'os.sandbox_oldest_age_s']);

const TIME_KEYS = new Set(['ts', 'checked_at', 'mtime', 'at', 'created', 'created_at', 'started_at', 'finished_at', 'updated_at', 'last_seen']);
const DURATION_KEYS = new Set(['duration_ms', 'ms', 'elapsed_ms', 'elapsed_s', 'duration_s']);

// generated-id patterns, alpha-renamed per backend in order of first appearance, one counter PER CLASS (so a missing
// or extra id of one class does not shift the placeholders of the others): [class, regex]
const ID_PATTERNS = [
  ['kid', /(?<=\bkey:)[0-9a-f]{12}\b/g],                                  // services/auth.py key ids (uuid4().hex[:12])
  ['minted', /(?<=\bminted-(?:admin|viewer)-)[0-9a-f]{12}(?=\.key\b)/g],      // mint-key output file names
  ['rev', /(?<![0-9a-f])[0-9a-f]{16}(?![0-9a-f])/g],                  // config_store revision ids (uuid4().hex[:16])
  ['run', /(?<!\d)\d{8}[Tt]\d{6}-\d{3}(?!\d)/g],                     // run ids: strftime("%Y%m%dT%H%M%S")-<ms> (+ "-name"), also lower-cased in container names
  ['tmp', /\.[0-9a-f]{12}\.(?:ya?ml|json|jsonl|csv|toml|tmp|py|txt)\b/g], // os.urandom(6).hex() temp files: suites.<hex>.yaml, credentials.<hex>.yaml, .records.<hex>.jsonl, .batch.N.<hex>.csv, <script>.<hex>.py
  ['tmpdir', /\bvalidate-[a-z0-9_]{8}\b/g],                          // guardrails: tempfile.mkdtemp(prefix="validate-")
  ['candidate', /candidate-\d+-\d+\.yml/g],                         // dynamo: candidate-<pid>-<time_ns>.yml
  ['disc', /\bnemo-console-dcgm-disc-\d+\b/g],                       // dcgm discovery container: int(time.time()*1000) % 10**9
];

function isoNowRe() {
  // ISO stamps within two days of now (the fixtures' dates are older and stay)
  const days = [-2, -1, 0, 1, 2].map((d) => new Date(Date.now() + d * 86400_000).toISOString().slice(0, 10));
  return new RegExp(`(?:${days.join('|')})[T ]\\d{2}:\\d{2}:\\d{2}(?:\\.\\d+)?(?:Z|[+-]\\d{2}:?\\d{2})?`, 'g');
}

/**
 * Per-backend normaliser. side: {root, port, upstreamPorts:{name:port}}.
 * The id table lives as long as the side (one scenario run): ids get the same placeholder every time they recur.
 */
export function makeNormaliser(side) {
  const ids = new Map();
  const counts = new Map();
  const iso = isoNowRe();
  const roots = [...new Set([side.root, side.realRoot].filter(Boolean))].sort((a, b) => b.length - a.length);
  const portMap = new Map([[String(side.port), '<console>'], ...Object.entries(side.upstreamPorts || {}).map(([n, p]) => [String(p), `<up:${n}>`])]);

  function str(s) {
    if (typeof s !== 'string' || !s) return s;
    if (side.version && s === side.version) return '<version>';                                  // own-release
    let out = s;
    for (const r of roots) out = out.split(r).join('<TMP>');                                    // tmp-paths
    out = out.replace(/(127\.0\.0\.1|localhost|\[::1\]):(\d{2,5})\b/g, (m, h, p) => (portMap.has(p) ? `${h}:${portMap.get(p)}` : m));  // ports
    for (const [cls, re] of ID_PATTERNS) {                                                      // generated-ids
      out = out.replace(re, (m) => {
        if (!ids.has(m)) {
          counts.set(cls, (counts.get(cls) || 0) + 1);
          ids.set(m, `<${cls}:${counts.get(cls)}>`);
        }
        return ids.get(m);
      });
    }
    out = out.replace(/\b\d+(?:\.\d+)? (s|min|h) ago\b/g, '<N> $1 ago');                        // ages
    out = out.replace(/(expected \S+\s+)\d+ ms\b/g, '$1<N> ms')                                   // measured-text
      .replace(/ in \d+\.\d s\b/g, ' in <N> s').replace(/ · \d+\.\d s\b/g, ' · <N> s')
      .replace(/answers again after \d+ s\b/g, 'answers again after <N> s').replace(/([Rr]unning for )\d+ min\b/g, '$1<N> min');
    out = out.replace(iso, '<now>');                                                             // iso-now
    out = out.replace(/\b1[789]\d{8}\.\d{1,6}\b/g, (m) => (Math.abs(Number(m) - Date.now() / 1000) < 2 * 86400 ? '<epoch>' : m)); // epoch-now
    return out;
  }

  function json(tree) {
    return mapTree(tree, (node, _path, key) => {
      if (node.k === 'str') return { k: 'str', v: str(node.v) };
      if (node.k === 'obj') {
        const p = node.v.get('path');                                                            // run-json-size
        const by = node.v.get('bytes');
        if (p && p.k === 'str' && /(^|\/)run\.json$/.test(p.v) && by && by.k === 'int') {
          const m = new Map(node.v);
          m.set('bytes', { k: 'int', v: 0n });
          return { k: 'obj', v: m };
        }
        const mk = node.v.get('key');                                                            // volatile-metrics
        const val = node.v.get('value');
        if (mk && mk.k === 'str' && VOLATILE_METRIC_KEYS.has(mk.v) && val && (val.k === 'float' || val.k === 'int')) {
          const m = new Map(node.v);
          m.set('value', val.k === 'int' ? { k: 'int', v: 0n } : { k: 'float', v: 0, raw: '0.0' });
          return { k: 'obj', v: m };
        }
      }
      if (typeof key === 'string' && TIME_KEYS.has(key)) {                                       // time-keys
        if (node.k === 'int') return { k: 'int', v: 0n };
        if (node.k === 'float') return { k: 'float', v: 0, raw: '0.0' };
        if (node.k === 'str') return { k: 'str', v: '<time>' };
      }
      if (typeof key === 'string' && DURATION_KEYS.has(key)) {                                   // duration-keys
        if (node.k === 'int') return { k: 'int', v: 0n };
        if (node.k === 'float') return { k: 'float', v: 0, raw: '0.0' };
      }
      return node;
    }, '', null);
  }

  /** Prometheus text exposition -> sorted list of normalised lines. */
  function prometheus(text) {
    const lines = [];
    for (const raw of text.split('\n')) {
      if (!raw.trim()) continue;
      let line = str(raw);
      if (!line.startsWith('#')) {
        const m = /^([a-zA-Z_:][a-zA-Z0-9_:]*)(\{.*\})?\s+(\S+)(\s+\S+)?$/.exec(line);
        if (m) {
          const name = m[1];
          if (name.endsWith('_created')) line = `${name}${m[2] || ''} <created>`;                            // prom-created
          else if (/^nemo_console_http_request_seconds_(bucket|sum)$/.test(name)) line = `${name}${m[2] || ''} <latency>`; // prom-latency
        }
      }
      lines.push(line);
    }
    return lines.sort();                                                                         // prom-order
  }

  function cliRecord(rec) {
    const argv = (rec.argv || []).map(str);
    return JSON.stringify({ argv, cwd: str(rec.cwd || ''), ...(rec.stdin ? { stdin: str(rec.stdin) } : {}),
      envKeys: rec.envKeys, env: Object.fromEntries(Object.entries(rec.env || {}).map(([k, v]) => [k, str(v)])),
      ...(rec.unknown ? { unknown: true } : {}) });
  }

  function httpRecord(rec) {
    const q = new URLSearchParams(rec.query || '');
    let body = rec.body;
    if (body) {
      try { body = stringify(json(parse(body))); } catch { body = str(body); }   // kinds kept: 0 vs 0.0 differ
    }
    return JSON.stringify({ server: rec.server, method: rec.method, path: str(rec.path), query: [...q].map(([k, v]) => [k, str(v)]),
      headers: Object.fromEntries(Object.entries(rec.headers || {}).map(([k, v]) => [k, str(v)])),
      ...(body !== undefined ? { body } : {}), ...(rec.unknown ? { unknown: true } : {}) });
  }

  return { str, json, prometheus, cliRecord, httpRecord, ids };
}

export const isCachedArgv = (argvLine) => CACHED_ARGV.some(([re]) => re.test(argvLine));
export const isCachedHttp = (line) => CACHED_HTTP.some(([re]) => re.test(line));
