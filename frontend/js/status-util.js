// Pure interpretation of SystemStatus / ActionResult payloads. No DOM.
/** Dynamo (and any system) is "loading" when the status says so; a loading system is not down. */
export function isLoading(st) {
  if (!st || st.health === 'down') return false;
  return /\bload(ing)?\b/i.test(st.summary || '') || (st.components || []).some((c) => /\bload(ing)?\b/i.test(c.detail || '')) || (st.problems || []).some((p) => /\.loading$/.test(p.code || ''));
}
/** Attestation is reported as a component named "attestation" (state running = verified). */
export function attestation(st) {
  const c = (st?.components || []).find((x) => /attest/i.test(x.name));
  if (!c) {
    // Relay reports it inside the summary line ("attestation=signature_required"); show that value verbatim.
    const m = /attestation=([\w-]+)/.exec(st?.summary || '');
    return m ? { state: 'reported', detail: '', value: m[1] } : { state: 'unreported', detail: '' };
  }
  return { state: c.state === 'running' ? 'verified' : c.state === 'failed' ? 'failed' : 'unknown', detail: c.detail || '' };
}
/** Health of a system tile changed? returns [{id, from, to}] between two overview payloads. */
export function healthChanges(prev, next) {
  if (!prev) return [];
  const before = new Map((prev.systems || []).map((s) => [s.system, s.health]));
  return (next.systems || []).filter((s) => before.has(s.system) && before.get(s.system) !== s.health).map((s) => ({ id: s.system, from: before.get(s.system), to: s.health }));
}
/**
 * Preflight output is `key: value` lines. Extracts what the UI must show prominently:
 * the container sharing the card (with its state) and the router aliases that go dark. Unparsed lines stay in `rows`.
 */
export function parsePreflight(output) {
  const rows = [];
  for (const line of String(output || '').split('\n')) {
    const m = /^\s*([A-Za-z][\w .-]*?)\s*:\s*(.+?)\s*$/.exec(line);
    if (m) rows.push({ key: m[1], value: m[2] });
  }
  const find = (re) => rows.find((r) => re.test(r.key));
  const share = find(/shar/i);
  const dark = find(/dark|alias/i);
  const container = share ? (/[a-z][\w.-]*-\d+/i.exec(share.value) || [null])[0] : null;
  const running = share ? /running|up\b/i.test(share.value) && !/not running|stopped/i.test(share.value) : false;
  return {
    rows, container, sharingRunning: !!container && running,
    darkAliases: dark ? dark.value.split(/[,;]/).map((x) => x.trim().replace(/\s*\(.*\)$/, '')).filter(Boolean) : [],
    parsed: rows.length > 0,
  };
}

/** Audit rows have no action field: derive "system: action up" / "system: config apply routes" from the request path. */
export function auditLabel(row) {
  const path = String(row?.path || '').replace(/^\/api\/v1/, '');
  let m = /^\/systems\/(\w+)\/actions\/(\w+)/.exec(path);
  if (m) return `${m[1]}: action ${m[2]}`;
  m = /^\/systems\/(\w+)\/config\/(\w+)\/(apply|rollback|validate)/.exec(path);
  if (m) return `${m[1]}: config ${m[3]} ${m[2]}`;
  m = /^\/systems\/(\w+)\/params/.exec(path);
  if (m) return `${m[1]}: params ${row.method === 'PUT' ? 'apply' : 'preview'}`;
  m = /^\/systems\/(\w+)\/playground/.exec(path);
  if (m) return `${m[1]}: playground chat`;
  if (path === '/settings') return 'settings update';
  return `${row?.method || ''} ${path}`.trim() || '–';
}

/** /health `role`: "admin" | "viewer" | "public" (unauthenticated off-loopback: needs a key). Absent -> admin (the server enforces 403s). */
export function normalizeRole(v) { return v === 'viewer' ? 'viewer' : v === 'public' ? 'public' : 'admin'; }
