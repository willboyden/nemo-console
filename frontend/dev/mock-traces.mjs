// Deterministic mock of the Relay trace explorer routes (docs/ARCHITECTURE.md addendum 9.12): index state, traces,
// sessions, analytics, trajectories, capture and the admin-only content routes. All data is invented: neutral names
// (local-large, local-small, judge-small, opencode, claude-cli, example-agent), no real hosts, paths or people.
// The structure is fixed (seeded generator); only the timestamps float with the clock, anchored so that the newest
// event is a few minutes old. Nothing here is a verified fact about the real backend.

const WINDOWS = { '1h': 3600, '6h': 21600, '24h': 86400, '7d': 604800, '30d': 2592000, all: null };
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const IDENT = /^[A-Za-z0-9._:@/+-]{1,128}$/;
const KIND_ERR = { 401: 'unauthorized', 403: 'forbidden', 404: 'not_found', 409: 'conflict', 422: 'invalid_request' };
const bad = (status, code, message) => ({ status, body: { error: { code: code || KIND_ERR[status], message } } });
const ok = (body) => ({ status: 200, body });

function mulberry32(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
const round = (v, d = 0) => (v == null ? null : Math.round(v * 10 ** d) / 10 ** d);
const pctl = (arr, p) => {
  const a = arr.filter((v) => typeof v === 'number').sort((x, y) => x - y);
  if (!a.length) return null;
  const i = (a.length - 1) * p,
    lo = Math.floor(i),
    hi = Math.ceil(i);
  return a[lo] + (a[hi] - a[lo]) * (i - lo);
};
const sum = (a) => a.reduce((x, y) => x + (y || 0), 0);

// ---------------------------------------------------------------------------------------------- the world
export function buildWorld(latest) {
  const rnd = mulberry32(20261114);
  const pick = (a) => a[Math.floor(rnd() * a.length)];
  const between = (lo, hi) => lo + rnd() * (hi - lo);
  const int = (lo, hi) => Math.floor(between(lo, hi + 1));
  let idn = 0;
  const hex = (n) => Array.from({ length: n }, () => '0123456789abcdef'[Math.floor(rnd() * 16)]).join('');
  const uid = () => `${hex(8)}-${hex(4)}-7${hex(3)}-${'89ab'[idn++ % 4]}${hex(3)}-${hex(12)}`;

  const MODELS = { 'local-large': 'local-large-fp8', 'local-small': 'local-small-int4', 'judge-small': 'judge-small-int4', 'local-code': 'local-code-fp8' };
  const traces = [];
  const sessions = new Map(); // key -> {client, kind}
  const T = (hoursAgo) => latest - hoursAgo * 3600;

  function mkSpan(trace, p) {
    const s = {
      id: uid(),
      parent_id: p.parent?.id ?? null,
      name: p.name,
      category: p.category,
      subtype: p.subtype ?? null,
      role: p.role ?? null,
      flags: p.flags ?? [],
      start_ts: p.start,
      end_ts: p.open ? null : p.start + p.dur / 1000,
      duration_ms: p.open ? null : round(p.dur, 1),
      status: p.open ? 'open' : p.error ? 'error' : 'ok',
      depth: p.parent ? p.parent.depth + 1 : 0,
      turn_index: p.turn ?? null,
      llm: p.llm ?? null,
      tool: p.tool ?? null,
      error: p.error ?? null,
      content_available: p.content ?? true,
      size_bytes: p.size ?? int(900, 24000),
    };
    trace.spans.push(s);
    return s;
  }
  function mkMark(trace, parent, ts, name, extra = {}) {
    trace.marks.push({
      id: uid(),
      parent_id: parent?.id ?? null,
      ts,
      name,
      category: extra.category ?? 'mark',
      schema: extra.schema ?? null,
      severity: extra.severity ?? 'info',
      data: extra.data ?? {},
    });
  }
  function llm(trace, parent, start, o) {
    const model = o.model ?? 'local-large';
    const dur = o.dur ?? between(900, 9000);
    const out = o.out ?? int(40, 900);
    const inp = o.inp ?? int(800, 12000);
    const stream = o.stream ?? true;
    const ttfc = stream ? (o.ttfc ?? between(120, 260) + inp / 60) : null;
    const gen = stream && !o.open && !o.error ? Math.max(1, dur - ttfc) : 0;
    const chunks = stream && !o.error ? Math.max(1, Math.round(out / int(2, 5))) : 0;
    const offsets = [];
    if (stream && !o.error) {
      const n = Math.min(64, chunks);
      const span = o.open ? o.openFor - ttfc : gen;
      for (let i = 0; i < n; i++) offsets.push(round(ttfc + (span * (i + rnd() * 0.8)) / n, 1));
    }
    let maxGap = null;
    for (let i = 1; i < offsets.length; i++) maxGap = Math.max(maxGap ?? 0, offsets[i] - offsets[i - 1]);
    const reasoning = o.reasoning ?? (rnd() < 0.3 ? int(30, 400) : 0);
    const info = {
      model: o.alias ?? model,
      served_model: o.error ? null : MODELS[model],
      provider: 'router',
      api: o.api ?? pick(['openai.chat_completions', 'anthropic.messages']),
      stream,
      input_tokens: o.error ? null : inp,
      output_tokens: o.error || o.open ? (o.open ? out : null) : out,
      cached_tokens: o.error ? null : (o.cached ?? 0),
      cache_write_tokens: o.error ? null : (o.cacheWrite ?? 0),
      reasoning_tokens: o.error ? null : reasoning,
      total_tokens: o.error ? null : inp + out,
      ttfc_ms: o.error ? null : stream ? round(ttfc, 1) : null,
      tokens_per_s: stream && !o.error && !o.open && gen > 0 ? round(out / (gen / 1000), 1) : null,
      finish_reason: o.error ? null : o.open ? null : (o.finish ?? 'stop'),
      messages: o.messages ?? int(2, 40),
      tool_defs: o.toolDefs ?? 12,
      tool_calls_requested: o.toolsRequested?.length ?? 0,
      requested_tools: o.toolsRequested ?? [],
      max_tokens: 4096,
      reasoning_effort: o.effort ?? null,
      chunks: stream && !o.error ? chunks : null,
      chunk_offsets_ms: offsets,
      max_chunk_gap_ms: maxGap == null ? null : round(maxGap, 1),
      correlation: null,
    };
    return mkSpan(trace, {
      parent,
      name: info.api,
      category: 'llm',
      subtype: stream ? 'stream' : 'chat',
      role: null,
      flags: stream ? ['streaming'] : [],
      start,
      dur: o.open ? null : dur,
      open: o.open,
      error: o.error ? { type: o.error.type, status_code: o.error.status } : null,
      llm: info,
      turn: o.turn,
    });
  }
  function tool(trace, parent, start, name, o = {}) {
    const mcp = o.mcp ?? name.includes('__');
    return mkSpan(trace, {
      parent,
      name,
      category: 'tool',
      subtype: mcp ? 'mcp' : 'function',
      flags: mcp ? ['remote'] : [],
      start,
      dur: o.dur ?? between(15, 2500),
      error: o.error ? { type: o.error, status_code: null } : null,
      tool: { name, call_id: `call_${hex(8)}`, type: mcp ? 'mcp' : 'function', mcp },
      turn: o.turn,
      size: int(200, 6000),
    });
  }
  const finishTrace = (tr) => {
    tr.spans.sort((a, b) => (a.start_ts ?? 0) - (b.start_ts ?? 0));
    return tr;
  };
  function newTrace(o) {
    return {
      id: uid(),
      kind: o.kind,
      client: o.client,
      sessionKey: o.session,
      rootName: o.rootName,
      spans: [],
      marks: [],
      routing: null,
      error: null,
      hasTrajectory: false,
      relaySession: `session-${hex(20)}`,
    };
  }

  // -- gateway traces (one request each, some routed through the tiering plugin)
  function gatewayTrace(startTs, o = {}) {
    const tr = newTrace({ kind: 'gateway', client: o.anon ? null : (o.client ?? pick(['opencode', 'claude-cli', 'example-agent'])), session: o.session, rootName: 'gateway-turn' });
    const routed = o.routed ?? rnd() < 0.55;
    const out = o.out ?? int(60, 700);
    const inp = o.inp ?? int(900, 9000);
    const total = o.dur ?? between(1500, 12000);
    const root = mkSpan(tr, {
      name: 'gateway-turn',
      category: 'agent',
      flags: [],
      start: startTs,
      dur: total + (routed ? 600 : 40),
      open: o.openRoot,
      error: o.rootError,
      role: 'session',
    });
    let t = startTs + 0.012;
    if (routed) {
      const jd = o.judgeFail ? 5000 : between(240, 520);
      const judge = llm(tr, root, t, {
        model: 'judge-small',
        alias: 'judge-small',
        stream: false,
        inp: int(500, 1400),
        out: int(4, 30),
        dur: jd,
        error: o.judgeFail ? { type: 'judge_timeout', status: 504 } : null,
        messages: 2,
        toolDefs: 0,
        reasoning: 0,
      });
      t += jd / 1000 + 0.004;
      const escalate = o.escalate ?? rnd() < 0.35;
      const route = o.route ?? 'auto-tiered';
      const decision = o.judgeFail ? 'fallback' : escalate ? 'escalated' : 'kept-local';
      const served = o.judgeFail ? 'local-small' : escalate ? 'local-large' : 'local-small';
      mkMark(tr, root, t, 'switchyard.routing.decision', {
        schema: 'switchyard.routing/1',
        data: { route, decision, tier: escalate ? 'capable' : 'efficient', selected_model: served },
        severity: o.judgeFail ? 'warn' : 'info',
      });
      mkMark(tr, root, t + 0.001, 'switchyard.routing.llm_call.duration', {
        category: 'metric',
        data: { measurements: [{ name: 'switchyard.routing.llm_call.duration', kind: 'histogram', value: round(jd, 1), unit: 'ms', attributes: { call_role: 'routing' } }] },
      });
      tr.routing = { algorithm: 'llm_classifier', route, decision, served_model: served, overhead_ms: round(jd + between(4, 18), 1), errors: o.judgeFail ? 1 : 0 };
      if (o.judgeFail) tr.error = { type: 'judge_timeout', phase: 'routing' };
      const main = llm(tr, root, t + 0.006, {
        model: served,
        alias: route,
        inp,
        out,
        dur: total,
        cached: o.cached ?? Math.round(inp * between(0, 0.6)),
        error: o.error,
        open: o.openLlm,
        openFor: o.openFor,
      });
      void main;
    } else {
      const model = o.model ?? pick(['local-large', 'local-small', 'local-code']);
      llm(tr, root, t, { model, alias: model, inp, out, dur: total, cached: o.cached ?? Math.round(inp * between(0, 0.5)), error: o.error, open: o.openLlm, openFor: o.openFor, stream: o.stream });
    }
    if (o.error) tr.error = { type: o.error.type, phase: 'llm' };
    if (!o.openRoot) {
      const kids = tr.spans.filter((s) => s !== root);
      root.end_ts = Math.max(...kids.map((s) => s.end_ts ?? s.start_ts)) + 0.01;
      root.duration_ms = round((root.end_ts - root.start_ts) * 1000, 1);
    }
    return finishTrace(tr);
  }

  // -- coding-agent traces: root agent -> turns -> (llm, tools); optional subagent
  function agentTrace(startTs, o) {
    const tr = newTrace({ kind: o.kind, client: o.client, session: o.session, rootName: 'agent-run' });
    const turns = o.turns ?? int(2, 4);
    let t = startTs;
    const root = mkSpan(tr, { name: 'agent-run', category: 'agent', flags: [], start: startTs, dur: 1, role: 'session' });
    mkMark(tr, root, startTs + 0.001, 'session.start', { data: { hook_event_name: 'SessionStart' } });
    let ctx = o.ctxStart;
    const per = (o.ctxEnd - o.ctxStart) / Math.max(1, turns);
    let first = true;
    let nCalls = 0;
    for (let k = 0; k < turns; k++) {
      const turnStart = t;
      const turn = mkSpan(tr, { parent: root, name: `turn-${k + 1}`, category: 'turn', start: t, dur: 1, turn: k, role: 'turn' });
      const isLast = k === turns - 1;
      const reqTools = isLast ? [] : pick([['read_file'], ['bash'], ['read_file', 'grep'], ['edit_file', 'bash']]);
      const cachedShare = o.cacheShare ?? 0.8;
      const call = llm(tr, turn, t + 0.01, {
        model: o.model ?? 'local-large',
        alias: o.model ?? 'local-large',
        inp: Math.round(ctx),
        cached: first ? Math.round(ctx * 0.1) : Math.round(ctx * cachedShare),
        out: int(60, reqTools.length ? 260 : 700),
        dur: between(1500, 7000),
        toolsRequested: reqTools,
        finish: reqTools.length ? 'tool_calls' : 'stop',
        turn: k,
        effort: pick([null, 'high']),
        messages: Math.round(ctx / 600),
      });
      nCalls++;
      first = false;
      let tt = call.end_ts + 0.01;
      const toolNames = reqTools.map((n) => (rnd() < 0.15 ? 'docs__search' : n));
      let longest = 0;
      for (const [i, name] of toolNames.entries()) {
        const failing = o.toolError && k === 0 && i === 0;
        const ts = tool(tr, turn, tt + i * 0.004, name, { turn: k, error: failing ? 'exit_code_1' : null, dur: name === 'bash' ? between(300, 4000) : between(15, 260) });
        longest = Math.max(longest, ts.duration_ms);
        if (name === 'bash') mkMark(tr, ts, ts.start_ts + 0.0005, 'hook.PreToolUse', { data: { hook_event_name: 'PreToolUse', tool_name: name } });
      }
      if (o.subagent && k === 0) {
        const sub = tool(tr, turn, tt + 0.02, 'task', { turn: k, dur: 1 });
        const sa = mkSpan(tr, { parent: sub, name: 'subagent-explore', category: 'agent', flags: ['parallel'], start: sub.start_ts + 0.03, dur: 1, role: 'subagent' });
        let st = sa.start_ts + 0.02;
        for (let j = 0; j < 2; j++) {
          const sturn = mkSpan(tr, { parent: sa, name: `turn-${j + 1}`, category: 'turn', start: st, dur: 1, turn: j, role: 'turn' });
          const sc = llm(tr, sturn, st + 0.01, {
            model: 'local-small',
            alias: 'local-small',
            inp: int(1500, 5000),
            out: int(50, 200),
            dur: between(700, 2200),
            toolsRequested: j ? [] : ['grep'],
            finish: j ? 'stop' : 'tool_calls',
            turn: j,
          });
          let e = sc.end_ts;
          if (!j) {
            const g = tool(tr, sturn, e + 0.01, 'grep', { turn: j, dur: between(30, 200) });
            e = g.end_ts;
          }
          sturn.end_ts = e + 0.005;
          sturn.duration_ms = round((sturn.end_ts - sturn.start_ts) * 1000, 1);
          st = sturn.end_ts + 0.01;
        }
        sa.end_ts = st;
        sa.duration_ms = round((sa.end_ts - sa.start_ts) * 1000, 1);
        sub.end_ts = sa.end_ts + 0.01;
        sub.duration_ms = round((sub.end_ts - sub.start_ts) * 1000, 1);
        tt = sub.end_ts;
        longest = Math.max(longest, sub.duration_ms);
      }
      const tEnd = Math.max(call.end_ts + 0.01 + longest / 1000, tt) + 0.02;
      turn.end_ts = tEnd;
      turn.duration_ms = round((tEnd - turnStart) * 1000, 1);
      t = tEnd + 0.05;
      ctx += per;
    }
    root.end_ts = t;
    root.duration_ms = round((t - startTs) * 1000, 1);
    void nCalls;
    return finishTrace(tr);
  }

  // -- the open trace: an agent that is still streaming
  function openTrace(startTs, sessionKey) {
    const tr = newTrace({ kind: 'claude-code', client: 'claude-cli', session: sessionKey, rootName: 'agent-run' });
    const root = mkSpan(tr, { name: 'agent-run', category: 'agent', start: startTs, open: true, role: 'session' });
    const turn = mkSpan(tr, { parent: root, name: 'turn-1', category: 'turn', start: startTs + 0.02, dur: 1, turn: 0, role: 'turn' });
    const c1 = llm(tr, turn, startTs + 0.03, { inp: 21000, cached: 18500, out: 120, dur: 2400, toolsRequested: ['bash'], finish: 'tool_calls', turn: 0 });
    const tl = tool(tr, turn, c1.end_ts + 0.01, 'bash', { turn: 0, dur: 1800 });
    turn.end_ts = tl.end_ts + 0.02;
    turn.duration_ms = round((turn.end_ts - turn.start_ts) * 1000, 1);
    const turn2 = mkSpan(tr, { parent: root, name: 'turn-2', category: 'turn', start: turn.end_ts + 0.05, open: true, turn: 1, role: 'turn' });
    llm(tr, turn2, turn2.start_ts + 0.01, { inp: 22400, cached: 21800, out: 310, open: true, openFor: 4200, ttfc: 380, turn: 1 });
    return finishTrace(tr);
  }

  // -- plan -----------------------------------------------------------------------------------------
  const S1 = { key: 'ses_3kq9v2m7xa1c', client: 'opencode', kind: 'claude-code' };
  const S2 = { key: '7c1e5d02-4b8a-4f3e-9d61-a0f2c8b1e754', client: 'claude-cli', kind: 'claude-code' };
  const S3 = { key: 'sess-example-0042', client: 'example-agent', kind: 'codex' };
  for (const s of [S1, S2, S3]) sessions.set(s.key, s);

  // S1: 12 traces over ~26 hours, the context grows from 6k to ~92k tokens and the cache share rises
  for (let i = 0; i < 12; i++) {
    const ctx0 = 6000 + i * 7200,
      ctx1 = ctx0 + 7000;
    const at = T(26 - i * 2.1);
    const tr = agentTrace(at, {
      kind: S1.kind,
      client: S1.client,
      session: S1.key,
      turns: i === 4 ? 3 : int(2, 4),
      ctxStart: ctx0,
      ctxEnd: ctx1,
      cacheShare: Math.min(0.93, 0.35 + i * 0.05),
      subagent: i === 3,
      toolError: i === 6,
    });
    if (i === 6) tr.error = { type: 'exit_code_1', phase: 'tool' };
    traces.push(tr);
  }
  // S2: 8 traces (the newest is still open)
  for (let i = 0; i < 8; i++) {
    const at = T(9 - i * 1.1);
    traces.push(
      agentTrace(at, {
        kind: S2.kind,
        client: S2.client,
        session: S2.key,
        turns: int(2, 3),
        ctxStart: 4000 + i * 3500,
        ctxEnd: 8000 + i * 3500,
        cacheShare: 0.7 + i * 0.02,
        model: i % 3 === 2 ? 'local-code' : 'local-large',
      }),
    );
  }
  traces.push(openTrace(latest - 95, S2.key));
  // S3: 5 traces
  for (let i = 0; i < 5; i++)
    traces.push(
      agentTrace(T(5.5 - i * 0.9), { kind: S3.kind, client: S3.client, session: S3.key, turns: 2, ctxStart: 2500 + i * 2000, ctxEnd: 4500 + i * 2000, cacheShare: 0.5, model: 'local-small' }),
    );
  // a big trace (virtualised in the waterfall): 60 turns
  {
    const tr = agentTrace(T(14.3), { kind: 'claude-code', client: 'claude-cli', session: S2.key, turns: 90, ctxStart: 9000, ctxEnd: 96000, cacheShare: 0.9 });
    traces.push(tr);
  }
  // gateway traces: shared sessions for opencode, a few singletons
  const gwSessions = ['ses_gw_a81f3c', 'ses_gw_b27d90', 'ses_gw_c55e14'];
  for (const k of gwSessions) sessions.set(k, { client: 'opencode', kind: 'gateway' });
  for (let i = 0; i < 20; i++) {
    const ses = i % 3 === 2 ? undefined : gwSessions[i % 3];
    traces.push(gatewayTrace(T(30 - i * 1.45 - rnd()), { client: ses ? 'opencode' : undefined, session: ses }));
  }
  // errors: an upstream timeout, a bad request, a routing failure (judge timeout), a gateway root error
  traces.push(gatewayTrace(T(2.4), { routed: false, model: 'local-large', error: { type: 'upstream_timeout', status: 504 }, dur: 30000, out: 0 }));
  traces.push(gatewayTrace(T(1.7), { routed: false, model: 'local-code', error: { type: 'bad_request', status: 400 }, dur: 180, out: 0, stream: false }));
  traces.push(gatewayTrace(T(0.9), { routed: true, judgeFail: true }));
  traces.push(gatewayTrace(T(0.3), { routed: true, escalate: true, route: 'auto-tiered', dur: 8800 }));
  // a request from a client that sent no recognisable user agent (client null)
  traces.push(gatewayTrace(T(3.3), { anon: true, routed: false, model: 'local-small' }));
  // a gateway request that is still streaming
  traces.push(gatewayTrace(latest - 20, { routed: false, model: 'local-large', openRoot: true, openLlm: true, openFor: 3000, out: 180, inp: 6400 }));

  // finalise summaries -------------------------------------------------------------------------------
  const sessionOf = (tr) => tr.sessionKey ?? tr.id;
  for (const tr of traces) {
    const root = tr.spans.find((s) => s.parent_id == null);
    const llms = tr.spans.filter((s) => s.category === 'llm');
    const tools = tr.spans.filter((s) => s.category === 'tool');
    const ends = tr.spans.map((s) => s.end_ts).filter((v) => v != null);
    tr.start_ts = Math.min(...tr.spans.map((s) => s.start_ts));
    const anyOpen = tr.spans.some((s) => s.status === 'open');
    tr.end_ts = root.status === 'open' ? null : Math.max(...ends);
    tr.status = tr.spans.some((s) => s.status === 'error') ? 'error' : root.status === 'open' ? 'open' : 'ok';
    if (anyOpen && tr.status !== 'error') tr.status = 'open';
    tr.models = [...new Set(llms.map((s) => s.llm.model))].slice(0, 8);
    tr.sessionKey = sessionOf(tr);
    if (!sessions.has(tr.sessionKey)) sessions.set(tr.sessionKey, { client: tr.client, kind: tr.kind });
    // routing-failure marks etc. are already attached; fill the failure error from spans
    if (!tr.error) {
      const e = tr.spans.find((s) => s.status === 'error');
      if (e) tr.error = { type: e.error.type, phase: e.category === 'tool' ? 'tool' : 'llm' };
    }
  }
  traces.sort((a, b) => a.start_ts - b.start_ts);
  return { traces, sessions, MODELS, uid, rnd, hex, latest };
}

// ---------------------------------------------------------------------------------------------- summaries
function summary(tr, world) {
  const llms = tr.spans.filter((s) => s.category === 'llm');
  const tools = tr.spans.filter((s) => s.category === 'tool');
  const tk = (k) => (llms.some((s) => s.llm[k] != null) ? sum(llms.map((s) => s.llm[k])) : null);
  const firstStream = llms.find((s) => s.llm.ttfc_ms != null);
  const end = tr.end_ts;
  return {
    trace_id: tr.id,
    root_name: tr.rootName,
    kind: tr.kind,
    client: tr.client,
    session_key: tr.sessionKey,
    relay_session_id: tr.relaySession,
    start_ts: tr.start_ts,
    end_ts: end,
    duration_ms: end == null ? null : round((end - tr.start_ts) * 1000, 1),
    status: tr.status,
    spans: tr.spans.length,
    llm_calls: llms.length,
    tool_calls: tools.length,
    tool_errors: tools.filter((s) => s.status === 'error').length,
    marks: tr.marks.length,
    subagents: tr.spans.filter((s) => s.role === 'subagent').length,
    models: tr.models,
    input_tokens: tk('input_tokens'),
    output_tokens: tk('output_tokens'),
    cached_tokens: tk('cached_tokens'),
    reasoning_tokens: tk('reasoning_tokens'),
    total_tokens: tk('total_tokens'),
    max_input_tokens: llms.some((s) => s.llm.input_tokens != null) ? Math.max(...llms.map((s) => s.llm.input_tokens ?? 0)) : null,
    ttfc_ms: firstStream?.llm.ttfc_ms ?? null,
    routing: tr.routing,
    error: tr.error,
    has_trajectory: tr.hasTrajectory,
  };
}

function range(world, q) {
  const win = q.get('window') || '24h',
    anchor = q.get('anchor') || 'latest';
  if (!(win in WINDOWS)) return { err: bad(422, 'invalid_request', 'window must be one of 1h, 6h, 24h, 7d, 30d, all') };
  if (!['latest', 'now'].includes(anchor)) return { err: bad(422, 'invalid_request', 'anchor must be latest or now') };
  const to = anchor === 'now' ? Date.now() / 1000 : world.latest;
  const first = world.traces[0].start_ts;
  const from = WINDOWS[win] == null ? first : to - WINDOWS[win];
  return { from, to, win, anchor };
}
const inRange = (tr, r) => (tr.end_ts ?? tr.start_ts) >= r.from && tr.start_ts <= r.to;

function intParam(q, name, lo, hi, dflt) {
  const raw = q.get(name);
  if (raw == null || raw === '') return { v: dflt };
  const n = Number(raw);
  if (!Number.isInteger(n) || n < lo || n > hi) return { err: bad(422, 'invalid_request', `${name} must be an integer from ${lo} to ${hi}`) };
  return { v: n };
}
function facets(list, keys) {
  const out = {};
  for (const [name, get] of Object.entries(keys)) {
    const m = new Map();
    for (const tr of list) for (const v of [].concat(get(tr))) m.set(v ?? null, (m.get(v ?? null) ?? 0) + 1);
    out[name] = [...m.entries()]
      .sort((a, b) => b[1] - a[1] || String(a[0]).localeCompare(String(b[0])))
      .slice(0, 50)
      .map(([value, count]) => ({ value, count }));
  }
  return out;
}

function sessionSummaries(world, list) {
  const m = new Map();
  for (const tr of list) {
    const e = m.get(tr.sessionKey) ?? {
      session_key: tr.sessionKey,
      client: tr.client,
      kind: tr.kind,
      first_ts: Infinity,
      last_ts: -Infinity,
      traces: 0,
      llm_calls: 0,
      tool_calls: 0,
      errors: 0,
      models: new Set(),
      input_tokens: 0,
      output_tokens: 0,
      cached_tokens: 0,
      max_input_tokens: 0,
    };
    const s = summary(tr, world);
    e.first_ts = Math.min(e.first_ts, s.start_ts);
    e.last_ts = Math.max(e.last_ts, s.end_ts ?? s.start_ts);
    e.traces++;
    e.llm_calls += s.llm_calls;
    e.tool_calls += s.tool_calls;
    if (s.status === 'error') e.errors++;
    s.models.forEach((x) => e.models.add(x));
    e.input_tokens += s.input_tokens ?? 0;
    e.output_tokens += s.output_tokens ?? 0;
    e.cached_tokens += s.cached_tokens ?? 0;
    e.max_input_tokens = Math.max(e.max_input_tokens, s.max_input_tokens ?? 0);
    m.set(tr.sessionKey, e);
  }
  return [...m.values()].map((e) => ({ ...e, span_ms: round((e.last_ts - e.first_ts) * 1000, 1), models: [...e.models] }));
}

// ---------------------------------------------------------------------------------------------- analytics
const EDGES = {
  duration_ms: [10, 25, 50, 100, 250, 500, 1000, 2500, 5000, 10000, 25000, 60000],
  ttfc_ms: [10, 25, 50, 100, 250, 500, 1000, 2500, 5000, 15000],
  input_tokens: [0, 250, 500, 1000, 2000, 4000, 8000, 16000, 32000, 64000, 128000],
  output_tokens: [0, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096],
  tokens_per_s: [0, 5, 10, 20, 30, 40, 60, 80, 120, 200],
  chunk_gap_ms: [0, 50, 100, 200, 400, 800, 1600, 3200, 6400],
};
function hist(values, edges) {
  const counts = Array(edges.length - 1).fill(0);
  for (const v of values) {
    if (typeof v !== 'number') continue;
    let i = edges.findIndex((e, k) => k < edges.length - 1 && v >= e && v < edges[k + 1]);
    if (i < 0) i = v < edges[0] ? 0 : edges.length - 2;
    counts[i]++;
  }
  return { edges, counts };
}

function analytics(world, q) {
  const r = range(world, q);
  if (r.err) return r.err;
  const b = intParam(q, 'buckets', 12, 240, 60);
  if (b.err) return b.err;
  const traces = world.traces.filter((tr) => inRange(tr, r) && (!q.get('client') || tr.client === q.get('client')) && (!q.get('kind') || tr.kind === q.get('kind')));
  const modelFilter = q.get('model');
  const rows = [];
  for (const tr of traces)
    for (const s of tr.spans) {
      if (s.start_ts == null || s.start_ts < r.from || s.start_ts > r.to) continue;
      if (s.category === 'llm' && modelFilter && s.llm.model !== modelFilter) continue;
      rows.push({ s, tr });
    }
  const llms = rows.filter((x) => x.s.category === 'llm');
  const toolRows = rows.filter((x) => x.s.category === 'tool');
  const span = Math.max(60, r.to - r.from);
  const steps = [60, 120, 300, 600, 900, 1800, 3600, 7200, 14400, 21600, 43200, 86400];
  const bucketS = steps.find((s) => span / s <= b.v) ?? 86400;
  const t0 = Math.floor(r.from / bucketS) * bucketS;
  const n = Math.min(240, Math.max(1, Math.ceil((r.to - t0) / bucketS)));
  const ts = Array.from({ length: n }, (_, i) => t0 + i * bucketS);
  const idx = (t) => Math.min(n - 1, Math.max(0, Math.floor((t - t0) / bucketS)));
  const z = () => Array(n).fill(0);
  const series = { ts, traces: z(), llm_calls: z(), errors: z(), input_tokens: z(), output_tokens: z(), cached_tokens: z(), p50_ms: [], p95_ms: [], ttfc_p50_ms: [], by_model: [] };
  const perBucketDur = Array.from({ length: n }, () => []),
    perBucketTtfc = Array.from({ length: n }, () => []);
  for (const tr of traces) series.traces[idx(tr.start_ts)]++;
  for (const { s } of llms) {
    const i = idx(s.start_ts);
    series.llm_calls[i]++;
    if (s.status === 'error') series.errors[i]++;
    series.input_tokens[i] += s.llm.input_tokens ?? 0;
    series.output_tokens[i] += s.llm.output_tokens ?? 0;
    series.cached_tokens[i] += s.llm.cached_tokens ?? 0;
    if (s.duration_ms != null) perBucketDur[i].push(s.duration_ms);
    if (s.llm.ttfc_ms != null) perBucketTtfc[i].push(s.llm.ttfc_ms);
  }
  for (let i = 0; i < n; i++) {
    series.p50_ms.push(round(pctl(perBucketDur[i], 0.5), 1));
    series.p95_ms.push(round(pctl(perBucketDur[i], 0.95), 1));
    series.ttfc_p50_ms.push(round(pctl(perBucketTtfc[i], 0.5), 1));
  }
  const modelNames = [...new Set(llms.map((x) => x.s.llm.model))];
  const byModel = modelNames.map((model) => {
    const mm = llms.filter((x) => x.s.llm.model === model).map((x) => x.s);
    const d = mm.map((s) => s.duration_ms),
      tt = mm.map((s) => s.llm.ttfc_ms),
      tps = mm.map((s) => s.llm.tokens_per_s);
    return {
      model,
      llm_calls: mm.length,
      errors: mm.filter((s) => s.status === 'error').length,
      input_tokens: sum(mm.map((s) => s.llm.input_tokens)),
      output_tokens: sum(mm.map((s) => s.llm.output_tokens)),
      cached_tokens: sum(mm.map((s) => s.llm.cached_tokens)),
      reasoning_tokens: sum(mm.map((s) => s.llm.reasoning_tokens)),
      p50_ms: round(pctl(d, 0.5), 1),
      p95_ms: round(pctl(d, 0.95), 1),
      ttfc_p50_ms: round(pctl(tt, 0.5), 1),
      ttfc_p95_ms: round(pctl(tt, 0.95), 1),
      tokens_per_s_p50: round(pctl(tps, 0.5), 1),
      tokens_per_s_p95: round(pctl(tps, 0.95), 1),
    };
  });
  byModel.sort((a, c) => c.llm_calls - a.llm_calls);
  series.by_model = byModel.slice(0, 8).map((m) => {
    const calls = z(),
      tokens = z();
    for (const { s } of llms)
      if (s.llm.model === m.model) {
        const i = idx(s.start_ts);
        calls[i]++;
        tokens[i] += s.llm.total_tokens ?? 0;
      }
    return { model: m.model, llm_calls: calls, tokens };
  });
  const dur = llms.map((x) => x.s.duration_ms).filter((v) => v != null),
    ttfc = llms.map((x) => x.s.llm.ttfc_ms).filter((v) => v != null);
  const inT = sum(llms.map((x) => x.s.llm.input_tokens)),
    cT = sum(llms.map((x) => x.s.llm.cached_tokens));
  const errs = llms.filter((x) => x.s.status === 'error').length + toolRows.filter((x) => x.s.status === 'error').length;
  const totals = {
    traces: traces.length,
    sessions: new Set(traces.map((tr) => tr.sessionKey)).size,
    llm_calls: llms.length,
    tool_calls: toolRows.length,
    errors: errs,
    error_rate: llms.length + toolRows.length ? round(errs / (llms.length + toolRows.length), 4) : null,
    input_tokens: inT,
    output_tokens: sum(llms.map((x) => x.s.llm.output_tokens)),
    cached_tokens: cT,
    reasoning_tokens: sum(llms.map((x) => x.s.llm.reasoning_tokens)),
    cache_hit_ratio: inT ? round(cT / inT, 4) : null,
    p50_ms: round(pctl(dur, 0.5), 1),
    p95_ms: round(pctl(dur, 0.95), 1),
    p99_ms: round(pctl(dur, 0.99), 1),
    ttfc_p50_ms: round(pctl(ttfc, 0.5), 1),
    ttfc_p95_ms: round(pctl(ttfc, 0.95), 1),
    tokens_per_s_p50: round(
      pctl(
        llms.map((x) => x.s.llm.tokens_per_s),
        0.5,
      ),
      1,
    ),
  };
  const group = (key) => {
    const m = new Map();
    for (const tr of traces) {
      const e = m.get(tr[key]) ?? { [key]: tr[key], traces: 0, llm_calls: 0, tokens: 0, errors: 0 };
      const s = summary(tr, world);
      e.traces++;
      e.llm_calls += s.llm_calls;
      e.tokens += s.total_tokens ?? 0;
      if (s.status === 'error') e.errors++;
      m.set(tr[key], e);
    }
    return [...m.values()].sort((a, c) => c.traces - a.traces);
  };
  // heatmaps
  const days = [...new Set(llms.map((x) => new Date(x.s.start_ts * 1000).toISOString().slice(0, 10)))].sort().slice(-31);
  const act = days.map(() => Array(24).fill(0));
  for (const { s } of llms) {
    const d = new Date(s.start_ts * 1000),
      di = days.indexOf(d.toISOString().slice(0, 10));
    if (di >= 0) act[di][d.getUTCHours()]++;
  }
  const latTs = ts.length > 48 ? ts.filter((_, i) => i % Math.ceil(ts.length / 48) === 0) : ts;
  const step = ts.length > 48 ? Math.ceil(ts.length / 48) : 1;
  const latCounts = latTs.map(() => Array(EDGES.duration_ms.length - 1).fill(0));
  for (const { s } of llms) {
    if (s.duration_ms == null) continue;
    const ti = Math.min(latTs.length - 1, Math.floor(idx(s.start_ts) / step));
    const h = hist([s.duration_ms], EDGES.duration_ms);
    latCounts[ti][h.counts.findIndex((c) => c > 0)]++;
  }
  // scatter: evenly sampled
  const withDur = llms.filter((x) => x.s.duration_ms != null);
  const stride = Math.max(1, Math.ceil(withDur.length / 600));
  const scatter = withDur
    .filter((_, i) => i % stride === 0)
    .map(({ s, tr }) => ({
      trace_id: tr.id,
      span_id: s.id,
      ts: s.start_ts,
      duration_ms: s.duration_ms,
      input_tokens: s.llm.input_tokens,
      output_tokens: s.llm.output_tokens,
      model: s.llm.model,
      status: s.status,
    }));
  // routing
  const routed = traces.filter((tr) => tr.routing);
  const decisions = new Map();
  for (const tr of routed) {
    const k = `${tr.routing.route}|${tr.routing.decision}|${tr.routing.served_model}`;
    decisions.set(k, (decisions.get(k) ?? 0) + 1);
  }
  const decisionRows = [...decisions.entries()].map(([k, count]) => {
    const [route, decision, served_model] = k.split('|');
    return { route, decision, served_model, count };
  });
  const flowMap = new Map();
  for (const d of decisionRows) {
    flowMap.set(`${d.route}>${d.decision}`, (flowMap.get(`${d.route}>${d.decision}`) ?? 0) + d.count);
    flowMap.set(`${d.decision}>${d.served_model}`, (flowMap.get(`${d.decision}>${d.served_model}`) ?? 0) + d.count);
  }
  const judges = llms.filter((x) => x.s.llm.model === 'judge-small');
  const failures = new Map();
  for (const tr of routed)
    if (tr.routing.errors)
      failures.set(
        `${tr.error?.type ?? 'routing_error'}|${tr.error?.phase ?? 'routing'}`,
        (failures.get(`${tr.error?.type ?? 'routing_error'}|${tr.error?.phase ?? 'routing'}`) ?? 0) + tr.routing.errors,
      );
  const overheads = routed.map((tr) => tr.routing.overhead_ms);
  const counts = (arr) => {
    const m = new Map();
    arr.forEach((v) => v != null && m.set(v, (m.get(v) ?? 0) + 1));
    return [...m.entries()].sort((a, c) => c[1] - a[1]);
  };
  const errRows = rows.filter((x) => x.s.status === 'error');
  const toolAgg = new Map();
  for (const { s } of toolRows) {
    const e = toolAgg.get(s.tool.name) ?? { name: s.tool.name, calls: 0, errors: 0, d: [] };
    e.calls++;
    if (s.status === 'error') e.errors++;
    if (s.duration_ms != null) e.d.push(s.duration_ms);
    toolAgg.set(s.tool.name, e);
  }
  const reqAgg = new Map();
  for (const { s } of llms) for (const nm of s.llm.requested_tools) reqAgg.set(nm, (reqAgg.get(nm) ?? 0) + 1);
  return ok({
    range: { from: r.from, to: r.to },
    bucket_s: bucketS,
    totals,
    series,
    by_model: byModel,
    by_client: group('client').map((g) => ({ client: g.client, traces: g.traces, llm_calls: g.llm_calls, tokens: g.tokens, errors: g.errors })),
    by_kind: group('kind').map((g) => ({ kind: g.kind, traces: g.traces, llm_calls: g.llm_calls, tokens: g.tokens, errors: g.errors })),
    histograms: {
      duration_ms: hist(dur, EDGES.duration_ms),
      ttfc_ms: hist(ttfc, EDGES.ttfc_ms),
      input_tokens: hist(
        llms.map((x) => x.s.llm.input_tokens),
        EDGES.input_tokens,
      ),
      output_tokens: hist(
        llms.map((x) => x.s.llm.output_tokens),
        EDGES.output_tokens,
      ),
      tokens_per_s: hist(
        llms.map((x) => x.s.llm.tokens_per_s),
        EDGES.tokens_per_s,
      ),
      chunk_gap_ms: hist(
        llms.map((x) => x.s.llm.max_chunk_gap_ms),
        EDGES.chunk_gap_ms,
      ),
    },
    heatmap: { activity: { days, hours: Array.from({ length: 24 }, (_, i) => i), counts: act }, latency: { ts: latTs, edges_ms: EDGES.duration_ms, counts: latCounts } },
    tools: [...toolAgg.values()]
      .sort((a, c) => c.calls - a.calls)
      .slice(0, 30)
      .map((e) => ({ name: e.name, calls: e.calls, errors: e.errors, p50_ms: round(pctl(e.d, 0.5), 1), p95_ms: round(pctl(e.d, 0.95), 1) })),
    tool_requests: [...reqAgg.entries()].sort((a, c) => c[1] - a[1]).map(([name, count]) => ({ name, count })),
    routing: {
      requests: routed.length,
      by_algorithm: routed.length ? [{ algorithm: 'llm_classifier', count: routed.length }] : [],
      decisions: decisionRows.sort((a, c) => c.count - a.count),
      judge: {
        calls: judges.length,
        errors: judges.filter((x) => x.s.status === 'error').length,
        p50_ms: round(
          pctl(
            judges.map((x) => x.s.duration_ms),
            0.5,
          ),
          1,
        ),
        p95_ms: round(
          pctl(
            judges.map((x) => x.s.duration_ms),
            0.95,
          ),
          1,
        ),
      },
      overhead_p50_ms: round(pctl(overheads, 0.5), 1),
      overhead_p95_ms: round(pctl(overheads, 0.95), 1),
      failures: [...failures.entries()].map(([k, count]) => {
        const [kind, phase] = k.split('|');
        return { kind, phase, count };
      }),
      flows: [...flowMap.entries()].map(([k, value]) => {
        const [source, target] = k.split('>');
        return { source, target, value };
      }),
    },
    errors: {
      by_type: counts(errRows.map((x) => x.s.error.type)).map(([type, count]) => ({ type, count })),
      by_status: counts(errRows.map((x) => x.s.error.status_code)).map(([status, count]) => ({ status, count })),
      finish_reasons: counts(llms.map((x) => x.s.llm.finish_reason)).map(([reason, count]) => ({ reason, count })),
      recent: errRows
        .sort((a, c) => c.s.start_ts - a.s.start_ts)
        .slice(0, 20)
        .map(({ s, tr }) => ({ ts: s.start_ts, trace_id: tr.id, span_id: s.id, model: s.llm?.model ?? null, type: s.error.type })),
    },
    scatter,
  });
}

// ---------------------------------------------------------------------------------------------- content
const part = (type, text, extra = {}) => ({
  type,
  text: text ?? null,
  name: extra.name ?? null,
  call_id: extra.call_id ?? null,
  chars: extra.chars ?? (text ? text.length : null),
  truncated: !!extra.truncated,
});
function spanContent(world, tr, s, q) {
  const maxChars = Number(q.get('max_chars') || 32768);
  const head = q.get('messages') === 'head';
  const base = {
    span_id: s.id,
    category: s.category,
    source: { file: 'relay-events.jsonl', compressed: false },
    request: null,
    response: null,
    tool: null,
    headers: {},
    truncated: false,
    redactions: 0,
  };
  if (s.category === 'tool') {
    return {
      ...base,
      tool: {
        arguments: part(
          'text',
          JSON.stringify(
            {
              command: s.tool.name === 'bash' ? 'cargo test --workspace' : undefined,
              path: s.tool.name === 'read_file' ? 'src/lib.rs' : undefined,
              pattern: s.tool.name === 'grep' ? 'fn main' : undefined,
            },
            null,
            2,
          ),
        ),
        result: s.status === 'error' ? part('text', 'error: process exited with status 1\n  --> src/lib.rs:41:9') : part('text', 'running 14 tests\ntest result: ok. 14 passed; 0 failed; 0 ignored\n'),
      },
    };
  }
  if (s.category !== 'llm') return { ...base, request: null };
  const total = Math.min(40, s.llm.messages);
  const returned = Math.min(8, total);
  const all = [];
  for (let i = 0; i < total; i++) {
    if (i % 4 === 0)
      all.push({
        role: 'user',
        parts: [part('text', i === 0 ? 'Please fix the failing test in the parser module and explain what was wrong.' : `Follow-up ${i}: also check <b>the build</b> and run the suite again.`)],
      });
    else if (i % 4 === 1)
      all.push({
        role: 'assistant',
        parts: [
          part('reasoning', 'The user wants the parser test fixed. I should read the file first, then run the tests to confirm.'),
          part('tool_call', JSON.stringify({ path: 'src/parser.rs' }), { name: 'read_file', call_id: `call_${i}` }),
        ],
      });
    else if (i % 4 === 2) all.push({ role: 'tool', parts: [part('tool_result', `fn parse(input: &str) -> Result<Ast, Error> {\n    // ...\n}\n// call_${i - 1}`, { call_id: `call_${i - 1}` })] });
    else all.push({ role: 'assistant', parts: [part('text', 'I found it: the tokenizer drops the final character. Applying the fix now.')] });
  }
  if (tr.id === world.traces[3]?.id && total > 2)
    all[total - 1] = { role: 'user', parts: [part('text', '<script>alert("not run")</script> <img src=x onerror=alert(1)> and a screenshot'), part('image', null, { chars: 48210 })] };
  const msgs = head ? all.slice(0, returned) : all.slice(total - returned);
  const cut = (txt) => (txt.length > maxChars ? { text: txt.slice(0, maxChars), truncated: true, chars: txt.length } : null);
  void cut;
  const resp = [];
  if (s.llm.finish_reason === 'tool_calls') resp.push(part('tool_call', JSON.stringify({ command: 'cargo test' }), { name: s.llm.requested_tools[0] ?? 'bash', call_id: 'call_resp' }));
  else if (s.llm.finish_reason) resp.push(part('text', 'The parser dropped the final character when the input ended without a newline. I changed the loop bound and the test passes.'));
  const failed = s.status === 'error';
  return {
    ...base,
    request: {
      model: s.llm.model,
      params: { temperature: 0.2, max_tokens: s.llm.max_tokens, stream: s.llm.stream, reasoning_effort: s.llm.reasoning_effort },
      system: [part('text', 'You are a careful coding assistant. Keep answers short and cite file paths. Never print secrets: [redacted]. Answer in <b>plain text</b>; never emit <script> tags.')],
      messages: msgs,
      tools: ['read_file', 'edit_file', 'bash', 'grep'].map((name) => ({ name, description: `Tool ${name}: example description of what it does and when to call it.` })),
      messages_total: total,
      messages_returned: msgs.length,
    },
    response: failed
      ? null
      : {
          model: MODELS_OF(world, s.llm.model),
          finish_reason: s.llm.finish_reason,
          parts: resp,
          usage: { input_tokens: s.llm.input_tokens, output_tokens: s.llm.output_tokens, cached_tokens: s.llm.cached_tokens },
        },
    headers: {
      'user-agent': `${tr.client}/1.4.2`,
      'content-type': 'application/json',
      traceparent: '00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01',
      'x-nemo-relay-session': tr.sessionKey,
      'x-session-id': tr.sessionKey,
    },
    truncated: total > returned,
    redactions: 2,
  };
}
const MODELS_OF = (world, m) => world.MODELS[m] ?? m;

function stepContent(step) {
  const msg =
    step.source === 'user'
      ? 'Summarise the open issues and propose the next step.'
      : step.source === 'system'
        ? 'Context loaded: 3 files, 2 notes.'
        : 'I checked the tracker. Two issues are open; the second blocks the release, so I would start there.';
  return {
    step_id: step.step_id,
    message: part('text', msg),
    reasoning: step.reasoning_chars ? part('reasoning', 'Weigh the blocker against the effort: the release depends on the second issue.') : null,
    tool_calls: step.tool_calls.map((c) => ({ id: c.id, name: c.name, arguments: part('text', '{"query":"open issues","limit":10}') })),
    observations: Array.from({ length: step.observations }, (_, i) => ({
      call_id: step.tool_calls[i]?.id ?? `call_${i}`,
      content: part('text', '2 results: #118 flaky retry test, #121 release blocker'),
    })),
    truncated: false,
    redactions: 0,
  };
}

// ---------------------------------------------------------------------------------------------- trajectories
function buildTrajectories(world) {
  const out = [];
  const linkable = world.traces.filter((tr) => tr.kind === 'gateway' && tr.spans.some((s) => s.category === 'llm' && s.status === 'ok')).slice(0, 3);
  const defs = [
    { source: 'gateway', agent: 'gateway', model: 'local-large', n: 6 },
    { source: 'gateway', agent: 'gateway', model: 'local-small', n: 4 },
    { source: 'gateway', agent: 'gateway', model: 'local-large', n: 8 },
    { source: 'example-agent', agent: 'example-agent', model: 'local-large', n: 12 },
    { source: 'example-agent', agent: 'example-agent', model: 'local-code', n: 9 },
    { source: 'example-agent', agent: 'example-agent', model: 'local-small', n: 5 },
  ];
  defs.forEach((d, k) => {
    const id = world.uid();
    const link = k < 3 ? linkable[k] : null;
    if (link) link.hasTrajectory = true;
    const t0 = world.latest - (k + 1) * 5400;
    const steps = [];
    let ts = t0;
    let step_id = 1;
    for (let i = 0; i < d.n; i++) {
      const src = i === 0 ? 'system' : i % 3 === 1 ? 'user' : 'agent';
      const agentStep = src === 'agent';
      const calls = agentStep && i % 2 === 0 ? [{ id: `call_${i}`, name: ['search_issues', 'read_file', 'run_tests'][i % 3], arguments_chars: 40 + i * 7 }] : [];
      const dur = agentStep ? Math.round(900 + world.rnd() * 5200) : null;
      const llm = link?.spans.filter((s) => s.category === 'llm')[0];
      steps.push({
        step_id: step_id++,
        ts,
        source: src,
        model: agentStep ? d.model : null,
        message_chars: Math.round(60 + world.rnd() * 900),
        reasoning_chars: agentStep && i % 2 === 1 ? Math.round(120 + world.rnd() * 400) : 0,
        tool_calls: calls,
        observations: calls.length ? 1 : 0,
        metrics: agentStep
          ? {
              prompt_tokens: Math.round(900 + i * 700 + world.rnd() * 300),
              completion_tokens: Math.round(50 + world.rnd() * 300),
              cached_tokens: Math.round(i * 500),
              reasoning_tokens: i % 2 === 1 ? 80 : 0,
            }
          : { prompt_tokens: null, completion_tokens: null, cached_tokens: null, reasoning_tokens: null },
        duration_ms: dur,
        span_id: agentStep && llm ? llm.id : null,
        status: i === 7 ? 'error' : 'ok',
      });
      ts += (dur ?? 400) / 1000 + 1.5;
    }
    const m = (k2) => sum(steps.map((s) => s.metrics[k2]));
    out.push({
      summary: {
        id,
        source: d.source,
        file: `nemo-relay-atif-${id}.json`,
        bytes: 1500 + d.n * 640,
        mtime: ts,
        schema_version: 'ATIF-v1.7',
        agent: { name: d.agent, version: '0.9.2', model: d.model },
        session_id: link?.relaySession ?? `session-${world.hex(20)}`,
        steps: steps.length,
        tool_calls: sum(steps.map((s) => s.tool_calls.length)),
        prompt_tokens: m('prompt_tokens'),
        completion_tokens: m('completion_tokens'),
        cached_tokens: m('cached_tokens'),
        first_ts: t0,
        last_ts: ts,
        trace_id: link?.id ?? null,
      },
      steps,
    });
  });
  return out;
}

// ---------------------------------------------------------------------------------------------- capture
function capture(variant, world) {
  const full = variant !== 'gen_ai';
  const last = world.latest;
  // The shapes are pinned in docs/ARCHITECTURE.md 9.12 ("Pinned details"): a level that is off still carries every key (unknown = null, lists empty).
  const traceEndpoints = full
    ? [
        { projection: 'gen_ai', target: 'otel-collector:4318', transport: 'http', service_name: 'relay-gateway', reachable: null },
        { projection: 'openinference', target: 'localhost:6006', transport: 'http', service_name: 'relay-gateway', reachable: true },
        { projection: 'full', target: 'localhost:3000', transport: 'grpc', service_name: 'relay-gateway', reachable: false },
      ]
    : [{ projection: 'gen_ai', target: 'otel-collector:4318', transport: 'http', service_name: 'relay-gateway', reachable: null }];
  const derived = traceEndpoints.map((e) => ({ target: e.target, transport: e.transport, reachable: e.reachable }));
  const lv = (id, label, enabled, state, detail, observed) => ({ id, label, enabled, state, detail, observed });
  return {
    relay_version: '0.9.2',
    levels: [
      lv(
        'atof',
        'ATOF event log (JSONL on disk)',
        true,
        'active',
        { sinks: [{ type: 'file', file: 'relay-events.jsonl', mode: 'append' }] },
        { last_ts: last, events: 412000, bytes: 893000000, files: 3 },
      ),
      lv('atif', 'ATIF trajectories (one file per session)', true, 'active', { agent_name: 'gateway' }, { last_ts: last - 1800, trajectories: 6, bytes: 24000 }),
      lv('otel_traces', 'OpenTelemetry traces', true, 'unknown', { endpoints: traceEndpoints }, {}),
      full
        ? lv('otel_logs', 'OpenTelemetry logs (marks as log records)', true, 'unknown', { minimum_severity: 'info', derived: true, endpoints: derived }, {})
        : lv('otel_logs', 'OpenTelemetry logs (marks as log records)', false, 'off', { minimum_severity: null, derived: null, endpoints: [] }, {}),
      lv('otel_metrics', 'OpenTelemetry metrics', true, 'unknown', { export_interval_ms: 60000, derived: true, endpoints: derived }, {}),
      lv('payloads', 'Prompt and completion payloads', true, 'active', { mode: full ? 'full' : 'turn_filtered' }, { last_ts: last, events: 38800 }),
      full
        ? lv('redaction', 'Redaction of secrets and PII', true, 'active', { detectors: ['secrets', 'pii', 'email'] }, {})
        : lv('redaction', 'Redaction of secrets and PII', false, 'off', { detectors: [] }, {}),
      full
        ? lv('resource_metrics', 'Host resource metrics (CPU, memory)', true, 'unknown', { polling: true, interval_ms: 15000 }, {})
        : lv('resource_metrics', 'Host resource metrics (CPU, memory)', false, 'off', { polling: null, interval_ms: null }, {}),
      lv('pricing', 'Pricing and cost attribution', false, 'off', {}, {}),
    ],
    projections: { gen_ai: true, openinference: full, full },
    hints: full
      ? [
          { code: 'capture.otlp.unreachable', message: 'The "full" projection endpoint does not answer from this machine: nothing reaches the collector behind it.' },
          { code: 'capture.otlp.derived_duplicate', message: 'Logs and metrics have no endpoint of their own, so Relay exports them once per trace endpoint: 3 times each, a duplicate export.' },
          { code: 'capture.pricing.off', message: 'Pricing is not configured: token counts are shown but no cost.' },
        ]
      : [
          { code: 'capture.projection.gen_ai_only', message: 'Only the gen_ai projection is exported: tool arguments and agent structure are not in the OTLP traces.' },
          { code: 'capture.logs.marks_not_exported', message: 'Marks are not exported as logs, so routing decisions are only in the local ATOF files.' },
          { code: 'capture.payloads.turn_filtered', message: 'Payloads are turn-filtered: earlier messages of a long conversation are not recorded.' },
          { code: 'capture.redaction.off', message: 'Redaction is off: payload files can contain secrets pasted into a prompt.' },
          { code: 'capture.metrics.resource_off', message: 'Resource metrics are not collected.' },
          { code: 'capture.pricing.off', message: 'Pricing is not configured: token counts are shown but no cost.' },
        ],
  };
}

// ---------------------------------------------------------------------------------------------- the API
export function createTracesMock({ now = () => Date.now() / 1000 } = {}) {
  let latest, world, trajectories, byId, rotated;
  // The newest event is a few minutes old; `stale` moves the whole capture three days back (an idle lab).
  const load = (ageS) => {
    latest = Math.floor(now() / 60) * 60 - ageS;
    world = buildWorld(latest);
    trajectories = buildTrajectories(world);
    byId = new Map(world.traces.map((tr) => [tr.id, tr]));
    rotated = world.traces[0].spans.find((s) => s.category === 'llm').id; // its source line "can no longer be read"
  };
  const flags = { index: process.env.MOCK_TRACES || 'ready', content: 'admin', capture: process.env.MOCK_CAPTURE || 'full', truncated: false, stale: process.env.MOCK_TRACES_STALE === '1' };
  const STALE_S = 3 * 86400;
  load(flags.stale ? STALE_S + 7 * 60 : 7 * 60);
  const setStale = (on) => {
    if (!!on === flags.stale) return;
    flags.stale = !!on;
    load(on ? STALE_S + 7 * 60 : 7 * 60);
  };
  const buildStart = Date.now();

  function indexState() {
    const t = world.traces;
    const spans = sum(t.map((x) => x.spans.length));
    const marks = sum(t.map((x) => x.marks.length));
    const empty = flags.index === 'empty' || flags.index === 'disabled';
    const building = flags.index === 'building';
    const frac = building ? Math.min(0.94, 0.12 + ((Date.now() - buildStart) % 90000) / 90000) : 1;
    const f = (name, bytes, compressed, firstTs, lastTs, events, key) => ({
      name,
      bytes,
      scanned_bytes: empty ? 0 : building ? Math.round(bytes * (key === 0 ? frac : key === 1 ? Math.max(0, frac - 0.3) : 0)) : bytes,
      compressed,
      events,
      skipped_lines: key === 1 ? 3 : 0,
      state: building ? (key === 0 ? 'partial' : 'indexed') : 'indexed',
      first_ts: firstTs,
      last_ts: lastTs,
    });
    return {
      state: flags.index === 'disabled' ? 'disabled' : empty ? 'empty' : building ? 'building' : 'ready',
      generation: building ? 1 : 14,
      last_refresh_ts: empty ? null : now() - 2,
      build_ms: empty || building ? null : 840,
      files: empty
        ? []
        : [
            f('relay-events.jsonl', 357475000, false, latest - 40000, latest, 196000, 0),
            f('relay-events.jsonl.1', 476076000, false, latest - 160000, latest - 40000, 252000, 1),
            f('relay-events.jsonl.2.gz', 60947000, true, latest - 300000, latest - 160000, 24000, 2),
          ],
      events: empty ? 0 : 412000,
      spans: empty ? 0 : spans,
      open_spans: empty ? 0 : sum(t.map((x) => x.spans.filter((s) => s.status === 'open').length)),
      marks: empty ? 0 : marks,
      chunk_marks: empty ? 0 : 118000,
      traces: empty ? 0 : t.length,
      sessions: empty ? 0 : world.sessions.size,
      trajectories: flags.index === 'disabled' ? 0 : trajectories.length,
      coverage: empty ? { from_ts: null, to_ts: null } : { from_ts: t[0].start_ts, to_ts: latest },
      truncated: flags.index === 'building' ? false : flags.index === 'ready' && flags.truncated === true,
      limits: { scan_bytes: 17179869184, max_spans: 300000, max_line_bytes: 67108864 },
      content: flags.content === 'off' ? 'off' : 'admin',
      problems:
        flags.index === 'disabled'
          ? [{ code: 'relay.traces.disabled', message: 'The trace index is switched off (NEMO_CONSOLE_RELAY_TRACE_INDEX=off).' }]
          : flags.index === 'ready'
            ? [{ code: 'relay.traces.lines_skipped', message: '3 lines in relay-events.jsonl.1 were longer than the line limit and were skipped.' }]
            : [],
    };
  }
  const visible = () => (flags.index === 'empty' || flags.index === 'disabled' ? [] : flags.index === 'building' ? world.traces.slice(-14) : world.traces);

  function listTraces(q) {
    const r = range(world, q);
    if (r.err) return r.err;
    const lim = intParam(q, 'limit', 1, 200, 50),
      off = intParam(q, 'offset', 0, 1e9, 0),
      minMs = intParam(q, 'min_ms', 0, 1e9, 0);
    for (const x of [lim, off, minMs]) if (x.err) return x.err;
    const sort = q.get('sort') || 'start',
      order = q.get('order') || 'desc';
    if (!['start', 'duration', 'tokens', 'llm_calls'].includes(sort)) return bad(422, 'invalid_request', 'sort must be start, duration, tokens or llm_calls');
    if (!['asc', 'desc'].includes(order)) return bad(422, 'invalid_request', 'order must be asc or desc');
    const status = q.get('status');
    if (status && !['ok', 'error', 'open'].includes(status)) return bad(422, 'invalid_request', 'status must be ok, error or open');
    const text = q.get('q');
    if (text && (text.length > 80 || !IDENT.test(text))) return bad(422, 'invalid_request', 'q must be at most 80 characters of letters, digits and . _ : @ / + -');
    const base = visible()
      .filter((tr) => inRange(tr, r))
      .map((tr) => ({ tr, s: summary(tr, world) }));
    let list = base.filter(
      ({ tr, s }) =>
        (!q.get('client') || tr.client === q.get('client')) &&
        (!q.get('kind') || tr.kind === q.get('kind')) &&
        (!q.get('model') || s.models.includes(q.get('model'))) &&
        (!status || s.status === status) &&
        (!q.get('session') || tr.sessionKey === q.get('session')) &&
        (!minMs.v || (s.duration_ms ?? 0) >= minMs.v) &&
        (!text || [tr.id, tr.sessionKey, tr.rootName, ...s.models].some((x) => String(x).toLowerCase().includes(text.toLowerCase()))),
    );
    const key = { start: (x) => x.s.start_ts, duration: (x) => x.s.duration_ms, tokens: (x) => x.s.total_tokens, llm_calls: (x) => x.s.llm_calls }[sort];
    const m = order === 'asc' ? 1 : -1;
    list = list.sort((a, b) => {
      const x = key(a),
        y = key(b);
      if (x == null || y == null) return x == null && y == null ? 0 : x == null ? 1 : -1;
      return (x - y) * m;
    });
    return ok({
      range: { from: r.from, to: r.to },
      total: list.length,
      offset: off.v,
      limit: lim.v,
      items: list.slice(off.v, off.v + lim.v).map((x) => x.s),
      facets: facets(
        base.map((x) => x.tr),
        { client: (t) => t.client, model: (t) => t.models, kind: (t) => t.kind, status: (t) => t.status },
      ),
    });
  }

  function route(sub, q, { admin }) {
    let m;
    if (sub === '/traces/index') return ok(indexState());
    if (sub === '/traces') return listTraces(q);
    if ((m = sub.match(/^\/traces\/([^/]+)$/))) {
      if (!UUID.test(m[1])) return bad(422, 'invalid_request', 'trace id must be UUID-shaped');
      const tr = visible().find((x) => x.id === m[1]);
      if (!tr) return bad(404, 'not_found', 'unknown trace');
      return ok({ trace: summary(tr, world), spans: tr.spans.slice(0, 5000), marks: tr.marks.slice(0, 5000), truncated: tr.spans.length > 5000 });
    }
    if ((m = sub.match(/^\/traces\/([^/]+)\/spans\/([^/]+)\/content$/))) {
      if (!UUID.test(m[1]) || !UUID.test(m[2])) return bad(422, 'invalid_request', 'ids must be UUID-shaped');
      if (!admin) return bad(403, 'forbidden', 'span content needs the admin role');
      if (flags.content === 'off') return bad(403, 'content_disabled', 'content access is switched off (NEMO_CONSOLE_RELAY_TRACE_CONTENT=off)');
      const mx = intParam(q, 'max_chars', 1000, 200000, 32768);
      if (mx.err) return mx.err;
      if (q.get('messages') && !['tail', 'head'].includes(q.get('messages'))) return bad(422, 'invalid_request', 'messages must be tail or head');
      const tr = byId.get(m[1]);
      const s = tr?.spans.find((x) => x.id === m[2]);
      if (!tr || !s) return bad(404, 'not_found', 'unknown span');
      if (s.id === rotated) return bad(409, 'content_unavailable', 'the source line can no longer be read (the file was rotated or removed)');
      return ok(spanContent(world, tr, s, q));
    }
    if (sub === '/sessions') {
      const r = range(world, q);
      if (r.err) return r.err;
      const lim = intParam(q, 'limit', 1, 200, 50),
        off = intParam(q, 'offset', 0, 1e9, 0);
      for (const x of [lim, off]) if (x.err) return x.err;
      const sort = q.get('sort') || 'last',
        order = q.get('order') || 'desc';
      if (!['last', 'traces', 'tokens'].includes(sort) || !['asc', 'desc'].includes(order)) return bad(422, 'invalid_request', 'bad sort or order');
      const list = visible().filter((tr) => inRange(tr, r) && (!q.get('client') || tr.client === q.get('client')) && (!q.get('kind') || tr.kind === q.get('kind')));
      let rows = sessionSummaries(world, list);
      const text = q.get('q');
      if (text) rows = rows.filter((x) => x.session_key.toLowerCase().includes(text.toLowerCase()));
      const key = { last: (x) => x.last_ts, traces: (x) => x.traces, tokens: (x) => x.input_tokens + x.output_tokens }[sort];
      rows.sort((a, b) => (key(a) - key(b)) * (order === 'asc' ? 1 : -1));
      return ok({ range: { from: r.from, to: r.to }, total: rows.length, offset: off.v, limit: lim.v, items: rows.slice(off.v, off.v + lim.v) });
    }
    if ((m = sub.match(/^\/sessions\/([^/]+)$/))) {
      const key = decodeURIComponent(m[1]);
      if (!IDENT.test(key)) return bad(422, 'invalid_request', 'session key has characters outside the identifier alphabet');
      const trs = visible().filter((tr) => tr.sessionKey === key);
      if (!trs.length) return bad(404, 'not_found', 'unknown session');
      const calls = [];
      for (const tr of trs)
        for (const s of tr.spans)
          if (s.category === 'llm')
            calls.push({
              ts: s.start_ts,
              trace_id: tr.id,
              span_id: s.id,
              model: s.llm.model,
              input_tokens: s.llm.input_tokens,
              cached_tokens: s.llm.cached_tokens,
              output_tokens: s.llm.output_tokens,
              reasoning_tokens: s.llm.reasoning_tokens,
              duration_ms: s.duration_ms,
              ttfc_ms: s.llm.ttfc_ms,
              status: s.status,
            });
      calls.sort((a, b) => a.ts - b.ts);
      return ok({ session: sessionSummaries(world, trs)[0], traces: trs.map((tr) => summary(tr, world)).slice(0, 500), calls: calls.slice(0, 2000) });
    }
    if (sub === '/analytics') return flags.index === 'empty' || flags.index === 'disabled' ? analyticsEmpty(q) : analytics({ ...world, traces: visible() }, q);
    if (sub === '/trajectories') {
      const lim = intParam(q, 'limit', 1, 200, 50),
        off = intParam(q, 'offset', 0, 1e9, 0);
      for (const x of [lim, off]) if (x.err) return x.err;
      let rows = flags.index === 'disabled' ? [] : trajectories.map((t) => t.summary);
      if (q.get('trace')) {
        if (!UUID.test(q.get('trace'))) return bad(422, 'invalid_request', 'trace must be a UUID-shaped trace id');
        rows = rows.filter((x) => x.trace_id === q.get('trace'));
      }
      if (q.get('source')) rows = rows.filter((x) => x.source === q.get('source'));
      if (q.get('q')) rows = rows.filter((x) => `${x.id} ${x.session_id} ${x.agent.name} ${x.agent.model}`.toLowerCase().includes(q.get('q').toLowerCase()));
      rows = rows.slice().sort((a, b) => b.mtime - a.mtime);
      return ok({ total: rows.length, offset: off.v, limit: lim.v, items: rows.slice(off.v, off.v + lim.v) });
    }
    if ((m = sub.match(/^\/trajectories\/([^/]+)$/))) {
      if (!UUID.test(m[1])) return bad(422, 'invalid_request', 'trajectory id must be UUID-shaped');
      const t = trajectories.find((x) => x.summary.id === m[1] && (!q.get('source') || x.summary.source === q.get('source')));
      if (!t) return bad(404, 'not_found', 'unknown trajectory');
      return ok({ trajectory: t.summary, steps: t.steps, truncated: false });
    }
    if ((m = sub.match(/^\/trajectories\/([^/]+)\/steps\/([^/]+)\/content$/))) {
      if (!UUID.test(m[1]) || !/^\d{1,9}$/.test(m[2])) return bad(422, 'invalid_request', 'ids are malformed');
      if (!admin) return bad(403, 'forbidden', 'step content needs the admin role');
      if (flags.content === 'off') return bad(403, 'content_disabled', 'content access is switched off (NEMO_CONSOLE_RELAY_TRACE_CONTENT=off)');
      const t = trajectories.find((x) => x.summary.id === m[1]);
      const st = t?.steps.find((x) => String(x.step_id) === m[2]);
      if (!st) return bad(404, 'not_found', 'unknown step');
      return ok(stepContent(st));
    }
    if (sub === '/capture') return ok(capture(flags.capture, world));
    return null;
  }

  function analyticsEmpty(q) {
    const r = range(world, q);
    if (r.err) return r.err;
    const a = analytics({ ...world, traces: world.traces.slice(0, 1) }, new URLSearchParams({ window: '1h', anchor: 'now' }));
    return ok({ ...a.body, range: { from: r.from, to: r.to } });
  }

  return {
    route,
    flags,
    setStale,
    get world() {
      return world;
    },
    get trajectories() {
      return trajectories;
    },
    indexState,
  };
}
