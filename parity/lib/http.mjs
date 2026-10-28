// Raw HTTP/1.1 client on node:http: any method, any Host / Origin header, raw (un-normalised) paths, malformed and
// oversized bodies, chunked uploads, and a bounded read of an SSE stream. One fresh connection per request (no agent),
// so connection reuse never differs between the two backends.
import http from 'node:http';

/**
 * @param {number} port
 * @param {{method:string, path:string, headers?:Record<string,string>, body?:string|Buffer, chunked?:boolean,
 *          timeoutMs?:number, readMs?:number}} req
 * readMs: stop reading the body after this many ms (SSE); the socket is then destroyed.
 * @returns {Promise<{status:number, headers:Record<string,string>, body:Buffer, truncated:boolean, error?:string}>}
 */
export function send(port, req) {
  return new Promise((resolve) => {
    const timeoutMs = req.timeoutMs ?? 120_000;
    const headers = { ...(req.headers || {}) };
    const body = req.body === undefined ? undefined : Buffer.isBuffer(req.body) ? req.body : Buffer.from(req.body);
    if (body !== undefined && !req.chunked && !Object.keys(headers).some((h) => h.toLowerCase() === 'content-length')) {
      headers['Content-Length'] = String(body.length);
    }
    let settled = false;
    const done = (r) => { if (!settled) { settled = true; clearTimeout(timer); resolve(r); } };
    const r = http.request({ host: '127.0.0.1', port, method: req.method, path: req.path, headers, agent: false, setHost: false });
    const timer = setTimeout(() => { r.destroy(new Error(`timeout after ${timeoutMs} ms`)); }, timeoutMs);
    r.on('response', (res) => {
      const chunks = [];
      let truncated = false;
      const hdrs = {};
      for (let i = 0; i < res.rawHeaders.length; i += 2) {
        const k = res.rawHeaders[i].toLowerCase();
        hdrs[k] = k in hdrs ? `${hdrs[k]}, ${res.rawHeaders[i + 1]}` : res.rawHeaders[i + 1];
      }
      let readTimer = null;
      const finish = () => { if (readTimer) clearTimeout(readTimer); done({ status: res.statusCode, headers: hdrs, body: Buffer.concat(chunks), truncated }); };
      if (req.readMs) {
        readTimer = setTimeout(() => { truncated = true; finish(); res.destroy(); r.destroy(); }, req.readMs);
      }
      res.on('data', (c) => chunks.push(c));
      res.on('end', finish);
      res.on('error', finish);
      res.on('close', finish);
    });
    r.on('error', (e) => done({ status: 0, headers: {}, body: Buffer.alloc(0), truncated: false, error: `${e.code || ''} ${e.message}`.trim() }));
    if (body !== undefined) {
      if (req.chunked) {
        // several writes without Content-Length: node sends Transfer-Encoding: chunked
        const step = 64 * 1024;
        for (let off = 0; off < body.length; off += step) r.write(body.subarray(off, off + step), () => {});
      } else {
        r.write(body, () => {});
      }
    }
    r.end();
  });
}

/** Parse an SSE byte stream into a list of events: {comment} for `: x` lines, else {event, data, id}. A trailing
 *  partial event (no blank line yet) is reported as {partial: true, ...}. */
export function parseSse(text) {
  const events = [];
  const blocks = text.replace(/\r\n/g, '\n').split('\n\n');
  const tail = blocks.pop();
  const parseBlock = (block, partial) => {
    let ev = null;
    for (const line of block.split('\n')) {
      if (line === '') continue;
      if (line.startsWith(':')) { events.push({ comment: line.slice(1).trimStart(), ...(partial ? { partial } : {}) }); continue; }
      const idx = line.indexOf(':');
      const field = idx === -1 ? line : line.slice(0, idx);
      let value = idx === -1 ? '' : line.slice(idx + 1);
      if (value.startsWith(' ')) value = value.slice(1);
      ev ||= {};
      if (field === 'data') ev.data = ev.data === undefined ? value : `${ev.data}\n${value}`;
      else ev[field] = value;
    }
    if (ev) events.push(partial ? { ...ev, partial } : ev);
  };
  for (const b of blocks) parseBlock(b, false);
  if (tail && tail.trim()) parseBlock(tail, true);
  return events;
}
