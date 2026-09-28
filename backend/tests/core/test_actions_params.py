from __future__ import annotations

from .conftest import Env

A = "/api/v1/systems/dynamo/actions"


async def test_confirm_enforced_for_disruptive_and_destructive(env: Env) -> None:
    async with env.client() as c:
        for action in ("restart", "reset"):
            r = await c.post(f"{A}/{action}", json={"params": {}})
            assert r.status_code == 400 and r.json()["error"]["code"] == "confirm_required"
            r = await c.post(f"{A}/{action}", json={"params": {}, "confirm": False})
            assert r.status_code == 400
        assert env.adapters["dynamo"].actions_run == []
        r = await c.post(f"{A}/restart", json={"params": {}, "confirm": True})
        assert r.status_code == 200 and r.json()["ok"]
        r = await c.post(f"{A}/safe_admin", json={})
        assert r.status_code == 200
    assert [a for a, _ in env.adapters["dynamo"].actions_run] == ["restart", "safe_admin"]


async def test_param_validation_against_action_spec(env: Env) -> None:
    async with env.client() as c:
        for params in ({"timeout": 999}, {"timeout": "5"}, {"timeout": True}, {"nope": 1}, {"timeout": 0}):
            r = await c.post(f"{A}/restart", json={"params": params, "confirm": True})
            assert r.status_code == 422, params
            assert r.json()["error"]["code"] == "invalid_param"
        ok = await c.post(f"{A}/restart", json={"params": {"timeout": 30}, "confirm": True})
        assert ok.status_code == 200
    assert env.adapters["dynamo"].actions_run == [("restart", {"timeout": 30})]


async def test_unknown_action_and_bad_id(env: Env) -> None:
    async with env.client() as c:
        assert (await c.post(f"{A}/nope", json={})).status_code == 404
        assert (await c.post(f"{A}/Bad%20Id", json={})).status_code == 404
        assert (await c.post("/api/v1/systems/zzz/actions/restart", json={})).status_code == 404


async def test_viewer_may_only_run_viewer_role_actions(env: Env) -> None:
    _, key = env.container.auth.mint("viewer")
    async with env.client(client_host="10.0.0.2", token=key) as c:
        assert (await c.post(f"{A}/status", json={})).status_code == 200
        assert (await c.post(f"{A}/safe_admin", json={})).status_code == 403
        assert (await c.post(f"{A}/restart", json={"confirm": True})).status_code == 403
        assert len((await c.get(A)).json()) == 4


async def test_action_output_is_redacted_and_failures_are_data(env: Env) -> None:
    env.adapters["dynamo"].action_output = "key=sk-FAKEFAKEFAKE1234567890abcdef and Bearer abcdefghijklmnop"
    async with env.client() as c:
        body = (await c.post(f"{A}/safe_admin", json={})).json()
    assert "sk-FAKE" not in body["output"] and "abcdefghijklmnop" not in body["output"]

    async def boom(*_a: object) -> None:
        raise RuntimeError("kaboom")

    env.adapters["dynamo"].run_action = boom  # type: ignore[assignment,method-assign]
    async with env.client() as c:
        r = await c.post(f"{A}/safe_admin", json={})
    assert r.status_code == 200 and r.json()["ok"] is False and r.json()["problems"][0]["code"] == "console.action.failed"


async def test_second_mutation_while_busy_is_409(env: Env) -> None:
    async with env.container.lock("dynamo"), env.client() as c:
        r = await c.post(f"{A}/safe_admin", json={})
    assert r.status_code == 409 and r.json()["error"]["code"] == "busy"


async def test_params_catalog_preview_put(env: Env) -> None:
    base = "/api/v1/systems/dynamo/params"
    async with env.client() as c:
        got = (await c.get(base)).json()
        assert len(got["catalog"]) == 3 and got["values"] == {}
        r = await c.post(f"{base}/preview", json={"values": {"gpu_util": 0.5}})
        assert r.status_code == 200 and "0.5" in r.json()["rendered"]
        for bad in ({"gpu_util": 2.0}, {"mode": "z"}, {"n": 1.5}, {"unknown": 1}, {"n": True}):
            assert (await c.post(f"{base}/preview", json={"values": bad})).status_code == 422, bad
        r = await c.put(base, json={"values": {"n": 3}, "confirm": False})
        assert r.status_code == 400
        r = await c.put(base, json={"values": {"n": 3}, "confirm": True})
        assert r.status_code == 200 and r.json()["ok"]
        assert (await c.get(base)).json()["values"] == {"n": 3}
    assert env.adapters["dynamo"].params_applied == [{"n": 3}]


async def test_params_unsupported_is_empty_catalog(env: Env) -> None:
    env.adapters["relay"].param_catalog = list  # type: ignore[method-assign]
    async with env.client() as c:
        assert (await c.get("/api/v1/systems/relay/params")).json() == {"catalog": [], "values": {}}
        assert (await c.post("/api/v1/systems/relay/params/preview", json={"values": {}})).status_code == 404
