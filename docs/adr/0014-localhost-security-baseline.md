# ADR-0014: Localhost-service security baseline applied in full

Status: Accepted (v0.1.0)

## Context

"It only listens on loopback" is not a trust boundary: any web page in the operator's browser can send requests
to `127.0.0.1` (CSRF, DNS rebinding), and any local process can connect. The service can control containers.

## Decision

Apply the whole checklist, in one pure-ASGI middleware (so SSE is never buffered) plus per-route role
dependencies:

- `Host` allowlist built from host and port (421); unsafe methods require `Origin` absent or same-origin
  (403 `cross_origin`, including `Origin: null`); `X-Console: 1` on every non-GET even with a bearer (403
  `csrf_header_required`); no CORS.
- Bearer keys (`nck_` + 32 random bytes), stored as SHA-256, compared with `hmac.compare_digest` over all rows;
  key files created `O_EXCL` with `fchmod 0600`. Roles `admin` and `viewer`. A caller with no
  `Authorization` header is role `public` (401 on every route but `GET /api/v1/health`). **Loopback trust is opt-in**
  (`trust_loopback=true`, default false) and even then is refused when `X-Forwarded-For` or `Forwarded` is present,
  when the host is not loopback, or when `allowed_hosts` is set (`/health.trust_notice` says which). A wrong bearer is always 401 (no fallback to loopback trust).
- Every route carries exactly one of `public`, `viewer`, `admin` dependencies; a test enumerates the OpenAPI
  routes. Only `GET /api/v1/health` (and the static shell) are open. `/metrics` requires viewer.
- Security headers on all responses including errors and static files; `Cache-Control: no-store` on `/api` and
  `/metrics`; no Swagger/ReDoc; the OpenAPI JSON is behind auth.
- Body cap 1 MiB (Content-Length and chunked), config cap 256 KiB, SSE cap 8, playground `max_tokens` clamp,
  log/file read caps, bounded metric label cardinality.
- Secrets by env-var name; a global redactor scrubs known values and secret shapes from outputs, logs and audit.
- Outbound: httpx with `trust_env=False`, no redirects, and URLs restricted to `allowed_url_hosts`
  (`127.0.0.1`, `localhost`, `::1`).

## Consequences

- Each control has a named test; see THREAT-MODEL for the mapping.
- The default changed from loopback-admin to key-required: an earlier revision trusted key-less loopback callers as
  admin, which made any local process (and any same-host reverse proxy) admin. The cost is that the operator must
  paste a key into the browser once per tab (sessionStorage). Operators who accept the risk can opt back in.
- Tests: `test_loopback_trust_defaults_off`, `test_default_is_public_role_and_needs_a_key`,
  `test_loopback_trust_refused_when_exposure_plausible`, `test_explicit_opt_in_still_works_on_plain_loopback`.
- The `Host` check compares the header to `host:port` strings; a request without a port is rejected unless added
  to `allowed_hosts`.
- The DNS-rebinding defence relies on the `Host` allowlist; verified live with a forged `Host` (421).
- Not covered: TLS (loopback HTTP), per-key rate limits, key expiry, brute-force lockout (keys are 256-bit random,
  so online guessing is impractical, but nothing throttles it).

## Alternatives considered

- **Trust key-less loopback callers by default.** Was the first design; reversed because any local process and any
  same-host proxy would be admin. Kept as an opt-in setting.
- **Unix domain socket.** Would remove the browser-CSRF class but breaks plain browser access.
- **Session cookies.** Rejected: CSRF surface; a bearer header plus `X-Console` avoids ambient credentials.
