"""Dump the Python app's route table to parity/routes.python.json (run with the backend venv, isolated):

    REF=$(node parity/ref.mjs) && cd backend && NEMO_CONSOLE_NO_USER_ENV=1 HOME=$(mktemp -d) "$REF/.venv/bin/python" -I ../parity/tools/dump_routes.py

One row per (method, path). `auth` is the role dependency the route declares (public / viewer / admin); routes under
/api/ or /metrics without one are still guarded by the middleware (bearer required), which the row says as
`middleware: "bearer"`. `body` names the pydantic request model and its JSON schema; `query` lists query params.
Generated output only: nothing here imports or changes the server's behaviour."""
from __future__ import annotations

import json
import os
import sys
import tempfile
from pathlib import Path

from fastapi.routing import APIRoute

from nemo_console.api import deps
from nemo_console.config import Settings
from nemo_console.main import create_app

OUT = Path(__file__).resolve().parents[1] / "routes.python.json"


def role_of(route: APIRoute) -> str | None:
    names = {d.call.__name__ for d in route.dependant.dependencies if d.call is not None}
    for sub in route.dependant.dependencies:
        for d in sub.dependencies:
            if d.call is not None:
                names.add(d.call.__name__)
    for role in ("admin", "viewer", "public"):
        if role in names:
            return role
    return None


def main() -> int:
    tmp = Path(tempfile.mkdtemp(prefix="nc-routes-"))
    cfg = Settings(state_dir=tmp / "state", repo_root=tmp / "repo", background_tasks=False)
    app = create_app(cfg)
    rows = []
    from nemo_console.api.routers import admin
    from nemo_console.main import API, ROUTERS
    # FastAPI >= 0.140 wraps included routers (_IncludedRouter); enumerate the original routers with their prefix,
    # in the order the app matches them: the /api/v1 routers, the root router, then the app's own routes.
    found: list[tuple[str, APIRoute]] = [(API, r) for router in ROUTERS for r in router.routes]
    found += [("", r) for r in admin.root_router.routes]
    found += [("", r) for r in app.router.routes if isinstance(r, APIRoute)]
    for r in app.router.routes:      # plain Starlette routes (the OpenAPI document)
        if type(r).__name__ == "Route":
            for m in sorted(r.methods or ()):
                rows.append({"method": m, "path": r.path, "name": r.name, "handler": "fastapi.openapi",
                             "auth": None, "middleware": "bearer", "path_params": [], "query": [], "body_model": None,
                             "body_schema": None, "response_model": None, "status_code": 200})
    for prefix, r in found:
        if not isinstance(r, APIRoute):
            continue
        body = r.body_field
        body_model = None
        body_schema = None
        if body is not None:
            ann = body.field_info.annotation
            body_model = getattr(ann, "__name__", str(ann))
            try:
                body_schema = ann.model_json_schema()  # type: ignore[union-attr]
            except Exception:  # noqa: BLE001
                body_schema = None
        query = []
        for q in r.dependant.query_params:
            fi = q.field_info
            query.append({"name": q.name, "required": q.field_info.is_required(),
                          "default": None if fi.is_required() else fi.default,
                          "annotation": str(fi.annotation).replace("typing.", ""),
                          "constraints": [str(m) for m in fi.metadata]})
        path_params = sorted({p.name for p in r.dependant.path_params} |
                             {p.name for s in r.dependant.dependencies for p in s.path_params})
        resp_model = getattr(r.response_model, "__name__", None) if r.response_model is not None else None
        for m in sorted(r.methods):
            full = prefix + r.path
            rows.append({
                "method": m, "path": full, "name": r.name, "handler": f"{r.endpoint.__module__}.{r.endpoint.__name__}",
                "auth": role_of(r),
                "middleware": "bearer" if ((full.startswith("/api/") or full == "/metrics") and full != "/api/v1/health")
                else "open",
                "path_params": path_params, "query": query, "body_model": body_model, "body_schema": body_schema,
                "response_model": resp_model or (str(r.response_model) if r.response_model is not None else None),
                "status_code": r.status_code,
            })
    rows.sort(key=lambda x: (x["path"], x["method"]))
    OUT.write_text(json.dumps({"generated_by": "parity/tools/dump_routes.py", "app_version": app.version,
                               "routes": rows}, indent=1, default=str) + "\n")
    print(f"{len(rows)} routes -> {OUT}")
    return 0


if __name__ == "__main__":
    os.environ.setdefault("NEMO_CONSOLE_NO_USER_ENV", "1")
    sys.exit(main())
