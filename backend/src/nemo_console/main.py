"""FastAPI app factory + the `nemo-console` entry point (127.0.0.1:8793)."""
from __future__ import annotations

import asyncio
import contextlib
import logging
import sys
from collections.abc import AsyncIterator
from pathlib import Path
from types import FrameType
from typing import Annotated
from urllib.parse import unquote

import uvicorn
from fastapi import Depends, FastAPI, Request
from fastapi.responses import FileResponse, Response

from nemo_console.api.deps import public
from nemo_console.api.errors import install_error_handlers, problem
from nemo_console.api.routers import act, admin, configure, observe
from nemo_console.api.security import SecurityMiddleware
from nemo_console.config import Settings, __version__, user_env_file
from nemo_console.container import Container, build_container
from nemo_console.desktop import open_console, stop_console
from nemo_console.services.auth import Principal, write_private_file
from nemo_console.services.common import KNOWN
from nemo_console.services.events import begin_shutdown
from nemo_console.services.playground import resolve_secret
from nemo_console.services.telemetry import configure_logging

log = logging.getLogger("nemo_console")
API = "/api/v1"
ROUTERS = (observe.router, configure.router, act.router, admin.router)   # all mounted under /api/v1
STATIC_FILES = {"index.html", "favicon.svg", "favicon.ico"}
STATIC_DIRS = ("css", "js", "assets", "img", "fonts", "i18n")
STATIC_CACHE = "no-cache"   # every frontend file is revalidated on each load (ETag -> 304), see _file_response


def ensure_user_dirs(cfg: Settings) -> None:
    """Create the Data Designer dirs the console owns (0700; addendum 9.7). ~/.data-designer is deliberately NOT
    created: it is the tool's own config dir and the adapter reports it when missing. Never raises."""
    for p in (cfg.dd_scripts_dir, cfg.dd_datasets_root):
        with contextlib.suppress(OSError):
            p.mkdir(mode=0o700, parents=True, exist_ok=True)


def create_app(cfg: Settings | None = None, container: Container | None = None) -> FastAPI:
    cfg = cfg or Settings(_env_file=user_env_file())
    c = container or build_container(cfg)

    @contextlib.asynccontextmanager
    async def lifespan(_: FastAPI) -> AsyncIterator[None]:
        ensure_user_dirs(cfg)
        key_file = c.auth.ensure_bootstrap_key()
        if key_file:
            log.info("first start: admin API key written to %s (0600); copy it, then delete the file", key_file)
        for name in [*c.settings.effective_key_env().values(), cfg.capable_probe_key_env]:
            resolve_secret({"x": name}, "x")            # register credential VALUES with the redactor
        tasks: list[asyncio.Task[None]] = []
        if cfg.background_tasks:
            tasks.append(asyncio.create_task(c.metrics.run(), name="metrics-poller"))
        try:
            yield
        finally:
            for t in tasks:
                t.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)
            c.metrics.flush()
            if c.capable_probe is not None:
                await c.capable_probe.aclose()
            await c.http.aclose()

    # No Swagger/ReDoc (they load scripts from a CDN); the OpenAPI JSON stays behind auth.
    app = FastAPI(title="Nemo Console", version=__version__, lifespan=lifespan, docs_url=None, redoc_url=None,
                  openapi_url=f"{API}/openapi.json")
    app.state.container = c
    install_error_handlers(app)
    for router in ROUTERS:
        app.include_router(router, prefix=API)
    app.include_router(admin.root_router)
    app.add_middleware(SecurityMiddleware, container=c)
    _mount_frontend(app, cfg.frontend_dir)
    return app


def _unsafe_static_path(path: str) -> bool:
    """Judged on the raw string BEFORE any resolve(): resolve() would collapse `..` into an allowlisted-looking path."""
    for cand in (path, unquote(path), unquote(unquote(path))):
        if any(bad in cand for bad in ("\x00", "..", "//", "\\")) or any(seg == "." for seg in cand.split("/")):
            return True
    return False


def _mount_frontend(app: FastAPI, root: Path) -> None:
    """Static ALLOWLIST (index.html + a few asset dirs), never dev/ or tests/. Unknown paths (including /docs) are a JSON 404."""
    base_root = root.resolve()

    @app.get("/{path:path}", include_in_schema=False)
    async def static(path: str, request: Request, _: Annotated[Principal, Depends(public)]) -> Response:
        if _unsafe_static_path(path):
            return problem(404, "not_found", "no such file")
        rel = "index.html" if path == "" else path
        if not base_root.is_dir():
            return problem(404, "not_found", "frontend not installed")
        top = rel.split("/", 1)[0]
        if rel in STATIC_FILES or (top in STATIC_DIRS and "/" in rel):
            try:
                target = (base_root / rel).resolve()
                ok = target.is_file() and (rel in STATIC_FILES or target.is_relative_to((base_root / top).resolve()))
            except (ValueError, OSError):
                ok = False
            if ok:      # defeats ../ and symlink escapes
                return _file_response(request, target)
        return problem(404, "not_found", "no such file")


def _file_response(request: Request, target: Path) -> Response:
    """The file with `Cache-Control: no-cache`: the browser revalidates it on every load and gets a 304 while it is
    unchanged. Without the header browsers cache the ES modules heuristically for days, so after an update a page
    could pair a new view with an old shared module and fail its import (the 0.3.0 "Something went wrong" pages)."""
    try:
        st = target.stat()
    except OSError:
        return problem(404, "not_found", "no such file")
    resp = FileResponse(target, stat_result=st, headers={"Cache-Control": STATIC_CACHE})
    etag = resp.headers["etag"]
    sent = {tag.strip().removeprefix("W/") for tag in request.headers.get("if-none-match", "").split(",")}
    if etag in sent:
        return Response(status_code=304, headers={"ETag": etag, "Cache-Control": STATIC_CACHE})
    return resp


def mint_key(argv: list[str]) -> int:
    role = argv[0] if argv else "viewer"
    if role not in ("admin", "viewer"):
        print("usage: nemo-console mint-key [admin|viewer]", file=sys.stderr)
        return 2
    cfg = Settings(_env_file=user_env_file())
    c = build_container(cfg)
    kid, key = c.auth.mint(role, "minted via CLI")   # type: ignore[arg-type]
    path = cfg.state_dir / f"minted-{role}-{kid}.key"
    write_private_file(path, key + "\n")
    KNOWN.add(key)
    print(f"{role} key written to {path} (0600); it is not shown here")
    return 0


def run() -> None:
    if len(sys.argv) > 1 and sys.argv[1] == "mint-key":
        raise SystemExit(mint_key(sys.argv[2:]))
    if len(sys.argv) > 1 and sys.argv[1] == "open":
        raise SystemExit(open_console(sys.argv[2:]))
    if len(sys.argv) > 1 and sys.argv[1] == "stop":
        raise SystemExit(stop_console(sys.argv[2:]))
    cfg = Settings(_env_file=user_env_file())
    configure_logging()
    ensure_user_dirs(cfg)
    config = uvicorn.Config(create_app(cfg), host=cfg.host, port=cfg.port, log_config=None, access_log=False,
                            timeout_graceful_shutdown=cfg.shutdown_grace_s)
    server = ConsoleServer(config)
    with contextlib.suppress(KeyboardInterrupt):
        server.run()
    if not server.started:
        raise SystemExit(3)                               # what uvicorn.run does when startup fails (STARTUP_FAILURE)


class ConsoleServer(uvicorn.Server):
    """uvicorn's server, plus: on SIGTERM/SIGINT every open SSE stream ends cleanly first (`event: shutdown`), so the
    process exits at once instead of waiting `shutdown_grace_s` for a browser that never closes its stream."""

    def handle_exit(self, sig: int, frame: FrameType | None) -> None:
        begin_shutdown()
        super().handle_exit(sig, frame)


if __name__ == "__main__":
    run()
