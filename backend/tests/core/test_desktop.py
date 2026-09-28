"""`nemo-console open`: probe, background start and browser hand-off (all faked: nothing is started or opened)."""
from __future__ import annotations

import stat
import sys
from pathlib import Path
from typing import Any

import httpx
import pytest
import respx

from nemo_console import desktop
from nemo_console.config import Settings

URL = "http://127.0.0.1:8793/"
HEALTH = {"status": "degraded", "version": "0.1.1", "trust": "bearer", "role": "public", "trust_notice": None,
          "systems": {}}


class FakeProc:
    pid = 4242

    def __init__(self, exit_code: int | None = None) -> None:
        self.exit_code = exit_code

    def poll(self) -> int | None:
        return self.exit_code


@pytest.fixture
def launcher(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> dict[str, Any]:
    rec: dict[str, Any] = {"probes": [], "started": 0, "opened": [], "notes": [], "proc": FakeProc(),
                           "cfg": Settings(host="127.0.0.1", port=8793, state_dir=tmp_path)}

    def fake_probe(url: str) -> str:
        assert url == URL
        return str(rec["probes"].pop(0) if len(rec["probes"]) > 1 else rec["probes"][0])

    def fake_start(cfg: Settings) -> FakeProc:
        rec["started"] += 1
        proc: FakeProc = rec["proc"]
        return proc

    def fake_open(url: str) -> bool:
        rec["opened"].append(url)
        return True

    monkeypatch.setattr(desktop, "probe", fake_probe)
    monkeypatch.setattr(desktop, "start_background", fake_start)
    monkeypatch.setattr(desktop, "open_browser", fake_open)
    monkeypatch.setattr(desktop, "notify", rec["notes"].append)
    monkeypatch.setattr(desktop, "POLL_S", 0)
    return rec


def test_console_url_uses_loopback_for_wildcard_binds() -> None:
    assert desktop.console_url(Settings(host="127.0.0.1", port=8793)) == URL
    assert desktop.console_url(Settings(host="0.0.0.0", port=9000)) == "http://127.0.0.1:9000/"
    assert desktop.console_url(Settings(host="::1", port=8793)) == "http://[::1]:8793/"


@respx.mock
def test_probe_tells_console_foreign_and_down_apart() -> None:
    route = respx.get(f"{URL}api/v1/health")
    route.mock(return_value=httpx.Response(200, json=HEALTH))
    assert desktop.probe(URL) == "console"
    route.mock(return_value=httpx.Response(200, text="<html>some other service</html>"))
    assert desktop.probe(URL) == "foreign"
    route.mock(return_value=httpx.Response(200, json={"status": "ok"}))
    assert desktop.probe(URL) == "foreign"
    route.mock(side_effect=httpx.ConnectError("connection refused"))
    assert desktop.probe(URL) == "down"


def test_open_when_already_running_starts_nothing(launcher: dict[str, Any]) -> None:
    launcher["probes"] = ["console"]
    assert desktop.open_console([], launcher["cfg"]) == 0
    assert launcher["started"] == 0 and launcher["opened"] == [URL] and launcher["notes"] == []


def test_open_starts_the_console_then_waits_for_health(launcher: dict[str, Any]) -> None:
    launcher["probes"] = ["down", "down", "console"]
    assert desktop.open_console([], launcher["cfg"]) == 0
    assert launcher["started"] == 1 and launcher["opened"] == [URL]


def test_open_refuses_a_foreign_service_on_the_port(launcher: dict[str, Any]) -> None:
    launcher["probes"] = ["foreign"]
    assert desktop.open_console([], launcher["cfg"]) == 1
    assert launcher["started"] == 0 and launcher["opened"] == []
    assert "something other than Nemo Console" in launcher["notes"][0]


def test_open_reports_a_console_that_exits_during_start(launcher: dict[str, Any]) -> None:
    launcher["probes"] = ["down"]
    launcher["proc"] = FakeProc(exit_code=1)
    assert desktop.open_console([], launcher["cfg"]) == 1
    assert launcher["opened"] == [] and "console.log" in launcher["notes"][0]


def test_open_gives_up_after_the_start_timeout(launcher: dict[str, Any], monkeypatch: pytest.MonkeyPatch) -> None:
    launcher["probes"] = ["down"]
    monkeypatch.setattr(desktop, "START_TIMEOUT_S", 0)
    assert desktop.open_console([], launcher["cfg"]) == 1
    assert launcher["opened"] == []


def test_open_rejects_arguments(launcher: dict[str, Any]) -> None:
    assert desktop.open_console(["--port", "1"], launcher["cfg"]) == 2
    assert launcher["started"] == 0


def test_start_background_detaches_and_keeps_its_log_private(monkeypatch: pytest.MonkeyPatch,
                                                              tmp_path: Path) -> None:
    calls: list[tuple[list[str], dict[str, Any]]] = []
    monkeypatch.setattr(desktop.subprocess, "Popen", lambda args, **kw: calls.append((args, kw)) or FakeProc())
    state = tmp_path / "state"
    desktop.start_background(Settings(state_dir=state))
    args, kw = calls[0]
    assert args == [sys.executable, "-m", "nemo_console.main"] and kw["start_new_session"] is True
    assert stat.S_IMODE(state.stat().st_mode) == 0o700
    assert stat.S_IMODE((state / "console.log").stat().st_mode) == 0o600
