from __future__ import annotations

from fastapi.routing import APIRoute

from nemo_console.api.deps import ROLE_DEPS, admin, public, viewer
from nemo_console.api.routers import admin as admin_mod
from nemo_console.main import API, ROUTERS

from .conftest import Env


def _flatten(dep: object) -> list[object]:
    out: list[object] = []
    for d in getattr(dep, "dependencies", []):
        out.append(d.call)
        out.extend(_flatten(d))
    return out


def _all_routes() -> list[tuple[str, APIRoute]]:
    found = [(API + r.path, r) for router in ROUTERS for r in router.routes if isinstance(r, APIRoute)]
    found += [(r.path, r) for r in admin_mod.root_router.routes if isinstance(r, APIRoute)]
    return found


async def test_every_route_carries_exactly_one_role_dependency() -> None:
    routes = _all_routes()
    assert len(routes) >= 25
    for path, route in routes:
        roles = [c for c in _flatten(route.dependant) if c in ROLE_DEPS]
        assert len(roles) == 1, f"{path} {route.methods}: needs exactly one role dependency, has {roles}"
        if path == "/api/v1/health":
            assert roles == [public]
            continue
        assert roles[0] is not public, f"{path} must not be public"
        if "GET" not in route.methods and not path.endswith("/actions/{action}"):
            assert roles == [admin], f"{path}: mutating routes need admin"
        if "GET" in route.methods and path not in ("/api/v1/audit",):
            assert roles == [viewer], path


async def test_openapi_and_app_routes_are_exactly_the_enumerated_ones(env: Env) -> None:
    enumerated = {path for path, _ in _all_routes()}
    assert set(env.app.openapi()["paths"]) == enumerated
    # the only routes outside the routers: openapi.json (auth by middleware + tested) and the public static catch-all
    extra = [r for r in env.app.routes if isinstance(r, APIRoute)]
    assert [r.path for r in extra] == ["/{path:path}"]
    assert [c for c in _flatten(extra[0].dependant) if c in ROLE_DEPS] == [public]
    health = next(r for p, r in _all_routes() if p == "/api/v1/health")
    assert [c for c in _flatten(health.dependant) if c in ROLE_DEPS] == [public]


async def test_openapi_json_is_behind_auth(env: Env) -> None:
    async with env.client(client_host="10.0.0.9") as c:
        assert (await c.get("/api/v1/openapi.json")).status_code == 401
    async with env.client() as c:
        assert (await c.get("/api/v1/openapi.json")).status_code == 200
