"""FastAPI app factory + the `nemo-console` entry point (127.0.0.1:8793)."""
from __future__ import annotations

import asyncio
import contextlib
import logging
import sys
from collections.abc import AsyncIterator
from pathlib import Path
from typing import Annotated
from urllib.parse import unquote

import uvicorn
from fastapi import Depends, FastAPI
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
from nemo_console.services.playground import resolve_secret
from nemo_console.services.telemetry import configure_logging

log = logging.getLogger("nemo_console")
API = "/api/v1"
ROUTERS = (observe.router, configure.router, act.router, admin.router)   # all mounted under /api/v1
STATIC_FILES = {"index.html", "favicon.svg", "favicon.ico"}
STATIC_DIRS = ("css", "js", "assets", "img", "fonts", "i18n")


def create_app(cfg: Settings | None = None, container: Container | None = None) -> FastAPI:
    cfg = cfg or Settings(_env_file=user_env_file())
    c = container or build_container(cfg)

    @contextlib.asynccontextmanager
    async def lifespan(_: FastAPI) -> AsyncIterator[None]:
        key_file = c.auth.ensure_bootstrap_key()
        if key_file:
            log.info("first start: admin API key written to %s (0600); copy it, then delete the file", key_file)
        for name in c.settings.effective_key_env().values():
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
    async def static(path: str, _: Annotated[Principal, Depends(public)]) -> Response:
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
                return FileResponse(target)
        return problem(404, "not_found", "no such file")


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
    uvicorn.run(create_app(cfg), host=cfg.host, port=cfg.port, log_config=None, access_log=False)


if __name__ == "__main__":
    run()
