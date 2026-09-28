"""One pure-ASGI middleware (so SSE is never buffered): Host allowlist, Origin/CSRF, auth, body caps (chunked too),
security headers on EVERY response, audit of every mutating call, Prometheus metrics."""
from __future__ import annotations

import json
import logging
import time
from urllib.parse import urlsplit

from starlette.exceptions import HTTPException
from starlette.types import ASGIApp, Message, Receive, Scope, Send

from nemo_console.api.errors import problem
from nemo_console.container import Container
from nemo_console.services.audit import summarize_body
from nemo_console.services.auth import PUBLIC, Principal

log = logging.getLogger(__name__)
SAFE = {"GET", "HEAD", "OPTIONS"}
KNOWN_METHODS = {"GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"}
OPEN_PATHS = {"/api/v1/health"}
CSRF_HEADER = "x-console"
AUDIT_BODY_CAP = 64 * 1024
CSP = ("default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; "
       "base-uri 'none'; form-action 'none'; frame-ancestors 'none'")
SECURITY_HEADERS: list[tuple[bytes, bytes]] = [
    (b"content-security-policy", CSP.encode()),
    (b"x-content-type-options", b"nosniff"),
    (b"referrer-policy", b"no-referrer"),
    (b"x-frame-options", b"DENY"),
    (b"permissions-policy", b"camera=(), microphone=(), geolocation=(), interest-cohort=()"),
    (b"cross-origin-opener-policy", b"same-origin"),
    (b"cross-origin-resource-policy", b"same-origin"),
]


class BodyTooLarge(HTTPException):
    """An HTTPException so FastAPI's body parsing re-raises it as-is (-> 413) instead of a 400."""

    def __init__(self) -> None:
        super().__init__(413, "request body too large")


def guarded(path: str) -> bool:
    return (path.startswith("/api/") or path == "/metrics") and path not in OPEN_PATHS


class SecurityMiddleware:
    def __init__(self, app: ASGIApp, container: Container) -> None:
        self.app = app
        self.c = container
        cfg = container.cfg
        hosts = {f"127.0.0.1:{cfg.port}", f"localhost:{cfg.port}", f"[::1]:{cfg.port}", f"{cfg.host}:{cfg.port}"}
        self.hosts = {h.lower() for h in hosts} | {h.lower() for h in cfg.allowed_hosts}

    def _precheck(self, scope: Scope, headers: dict[str, str]) -> tuple[int, str, str] | None:
        method = scope["method"]
        host = headers.get("host", "").lower()
        if host not in self.hosts:
            return 421, "misdirected_request", "unrecognised Host header"
        if method not in SAFE:
            origin = headers.get("origin")
            if origin is not None and (origin == "null" or urlsplit(origin).netloc.lower() != host):
                return 403, "cross_origin", "cross-origin requests are not allowed"
            if headers.get(CSRF_HEADER) != "1":
                return 403, "csrf_header_required", "send the header 'X-Console: 1' on non-GET requests"
            cl = headers.get("content-length")
            if cl and cl.isdigit() and int(cl) > self.c.cfg.body_cap_bytes:
                return 413, "payload_too_large", "request body too large"
        return None

    def _principal(self, scope: Scope, headers: dict[str, str]) -> Principal | None:
        client = scope.get("client")
        return self.c.auth.principal_for(headers.get("authorization"), client[0] if client else None,
                                         "x-forwarded-for" in headers or "forwarded" in headers)

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        if scope["type"] != "http":
            await self.app(scope, receive, send)
            return
        path, method = scope["path"], scope["method"]
        headers = {k.decode("latin-1").lower(): v.decode("latin-1") for k, v in scope["headers"]}
        principal = PUBLIC
        deny = self._precheck(scope, headers)
        if deny is None and (guarded(path) or path in OPEN_PATHS):
            p = self._principal(scope, headers)
            if p is not None:
                principal = p
            elif guarded(path):
                deny = (401, "unauthorized", "a valid bearer API key is required")
        scope.setdefault("state", {})["principal"] = principal

        status, started, total, captured = 500, False, 0, bytearray()
        cap = self.c.cfg.body_cap_bytes

        async def recv() -> Message:
            nonlocal total
            msg = await receive()
            if msg["type"] == "http.request" and method not in SAFE:
                chunk = msg.get("body", b"")
                total += len(chunk)
                if total > cap:
                    raise BodyTooLarge
                if len(captured) < AUDIT_BODY_CAP:
                    captured.extend(chunk[: AUDIT_BODY_CAP - len(captured)])
            return msg

        async def snd(message: Message) -> None:
            nonlocal status, started
            if message["type"] == "http.response.start":
                status, started = message["status"], True
                api = path.startswith("/api/") or path == "/metrics"
                drop = {n for n, _ in SECURITY_HEADERS} | ({b"cache-control"} if api else set())
                hdrs = [(k, v) for k, v in message.get("headers", []) if k.lower() not in drop]
                hdrs += SECURITY_HEADERS
                if api:
                    hdrs.append((b"cache-control", b"no-store"))
                message = {**message, "headers": hdrs}
            await send(message)

        t0 = time.perf_counter()
        try:
            if deny is not None:
                await problem(deny[0], deny[1], deny[1].replace("_", " ") + ": " + deny[2])(scope, recv, snd)
            else:
                await self.app(scope, recv, snd)
        except BodyTooLarge:
            if not started:
                await problem(413, "payload_too_large", "request body too large")(scope, receive, snd)
        except Exception:  # noqa: BLE001 - last line of defence so even a 500 carries the security headers
            log.error("unhandled error in %s %s", method if method in KNOWN_METHODS else "OTHER", path.split("?")[0][:80])
            if not started:
                await problem(500, "internal_error", "internal error")(scope, receive, snd)
        finally:
            route = getattr(scope.get("route"), "path", None) or ("unmatched" if guarded(path) else "static")
            m = method if method in KNOWN_METHODS else "OTHER"     # bounded label cardinality
            self.c.telemetry.requests.labels(m, route, str(status)).inc()
            self.c.telemetry.latency.labels(m, route).observe(time.perf_counter() - t0)
            denied = deny is not None or (principal.role == "viewer" and status == 403 and method not in SAFE)
            if denied and (path.startswith("/api/") or path == "/metrics"):
                # counted + one aggregated log line per minute; never a table row (volume is attacker-chosen)
                self.c.telemetry.denied.labels(str(status if status in (401, 403, 421, 429) else 403)).inc()
                self.c.denials.note(status, method)
            elif path.startswith("/api/") and method not in SAFE and principal.role != "public":
                self._audit(principal, method, path, status, headers, bytes(captured), total)

    def _audit(self, principal: Principal, method: str, path: str, status: int, headers: dict[str, str],
               raw: bytes, size: int) -> None:
        params: object = None
        if raw and "json" in headers.get("content-type", ""):
            try:
                params = summarize_body(json.loads(raw))
            except ValueError:
                params = f"[unparseable json, {size} bytes]"
        elif size:
            params = f"[{size} bytes, non-json]"
        try:
            self.c.audit.record(actor=principal.name, role=principal.role, method=method, path=path, status=status,
                                params=params)
        except Exception:  # noqa: BLE001 - never fail a request because audit storage hiccuped
            log.error("audit write failed")
