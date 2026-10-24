"""`nemo-console open` / `stop`: URL, ownership check, background start and stop (all faked: nothing is started,
opened or signalled)."""
from __future__ import annotations

import os
import signal
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
TCP_HEADER = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n"


class FakeProc:
    pid = 4242

    def __init__(self, exit_code: int | None = None) -> None:
        self.exit_code = exit_code
        self.terminated = False

    def poll(self) -> int | None:
        return self.exit_code

    def terminate(self) -> None:
        self.terminated = True

    def wait(self, timeout: float) -> int:
        return 0


@pytest.fixture
def launcher(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> dict[str, Any]:
    rec: dict[str, Any] = {"checks": [], "started": 0, "opened": [], "notes": [], "proc": FakeProc(),
                           "cfg": Settings(host="127.0.0.1", port=8793, state_dir=tmp_path)}

    def fake_check(url: str, port: int) -> str:
        assert (url, port) == (URL, 8793)
        return str(rec["checks"].pop(0) if len(rec["checks"]) > 1 else rec["checks"][0])

    def fake_start(cfg: Settings) -> FakeProc:
        rec["started"] += 1
        proc: FakeProc = rec["proc"]
        return proc

    def fake_open(url: str) -> bool:
        rec["opened"].append(url)
        return True

    monkeypatch.setattr(desktop, "check", fake_check)
    monkeypatch.setattr(desktop, "start_background", fake_start)
    monkeypatch.setattr(desktop, "open_browser", fake_open)
    monkeypatch.setattr(desktop, "notify", rec["notes"].append)
    monkeypatch.setattr(desktop, "POLL_S", 0)
    return rec


# --- URL -------------------------------------------------------------------------------------------------------

def test_console_url_maps_wildcard_binds_to_loopback() -> None:
    assert desktop.console_url(Settings(host="127.0.0.1", port=8793)) == URL
    assert desktop.console_url(Settings(host="0.0.0.0", port=9000)) == "http://127.0.0.1:9000/"
    assert desktop.console_url(Settings(host="::", port=8793)) == "http://[::1]:8793/"
    assert desktop.console_url(Settings(host="::1", port=8793)) == "http://[::1]:8793/"
    assert desktop.console_url(Settings(host="localhost", port=8793)) == "http://localhost:8793/"


@pytest.mark.parametrize("host", ["127.0.0.1@evil.example", "evil.example/#", "evil.example", "192.168.1.7", "10.0.0.1"])
def test_console_url_refuses_anything_off_the_box(host: str) -> None:
    with pytest.raises(desktop.LauncherError, match="not a loopback address"):
        desktop.console_url(Settings(host=host, port=8793))


def test_console_url_refuses_an_out_of_range_port() -> None:
    with pytest.raises(desktop.LauncherError, match="out of range"):
        desktop.console_url(Settings(host="127.0.0.1", port=70000))


# --- who holds the port ----------------------------------------------------------------------------------------

def test_listen_uids_reads_every_listener_on_the_port(tmp_path: Path) -> None:
    tcp, tcp6 = tmp_path / "tcp", tmp_path / "tcp6"
    tcp.write_text(TCP_HEADER
                   + "   0: 0100007F:2259 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 11\n"
                   + "   1: 0100007F:2259 0100007F:9C40 01 00000000:00000000 00:00000000 00000000     0        0 12\n"
                   + "   2: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 13\n")
    tcp6.write_text(TCP_HEADER
                    + "   0: 00000000000000000000000000000000:2259 00000000000000000000000000000000:0000 0A "
                      "00000000:00000000 00:00000000 00000000   999        0 14\n")
    assert desktop.listen_uids(8793, (str(tcp),)) == {1000}                  # ESTABLISHED and other ports ignored
    assert desktop.listen_uids(8793, (str(tcp), str(tcp6))) == {1000, 999}   # a wildcard IPv6 squatter counts
    assert desktop.listen_uids(8793, (str(tmp_path / "missing"),)) == set()


def test_check_flags_a_console_shaped_answer_from_another_users_socket(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(desktop, "probe", lambda url: "console")
    monkeypatch.setattr(desktop, "listen_uids", lambda port: {0})
    assert desktop.check(URL, 8793) == "other-user"
    monkeypatch.setattr(desktop, "listen_uids", lambda port: {os.getuid(), 0})
    assert desktop.check(URL, 8793) == "other-user"
    monkeypatch.setattr(desktop, "listen_uids", lambda port: None)            # unreadable: fail closed
    assert desktop.check(URL, 8793) == "other-user"
    monkeypatch.setattr(desktop, "listen_uids", lambda port: {os.getuid()})
    assert desktop.check(URL, 8793) == "console"


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


# --- open ------------------------------------------------------------------------------------------------------

def test_open_when_already_running_starts_nothing(launcher: dict[str, Any]) -> None:
    launcher["checks"] = ["console"]
    assert desktop.open_console([], launcher["cfg"]) == 0
    assert launcher["started"] == 0 and launcher["opened"] == [URL] and launcher["notes"] == []


def test_open_starts_the_console_then_waits_for_health(launcher: dict[str, Any]) -> None:
    launcher["checks"] = ["down", "down", "console"]
    assert desktop.open_console([], launcher["cfg"]) == 0
    assert launcher["started"] == 1 and launcher["opened"] == [URL]


@pytest.mark.parametrize(("state", "note"), [("foreign", "something other than Nemo Console"),
                                             ("other-user", "another user's process")])
def test_open_refuses_a_port_it_cannot_trust(launcher: dict[str, Any], state: str, note: str) -> None:
    launcher["checks"] = [state]
    assert desktop.open_console([], launcher["cfg"]) == 1
    assert launcher["started"] == 0 and launcher["opened"] == [] and note in launcher["notes"][0]


def test_open_refuses_a_squatter_that_wins_the_port_during_start(launcher: dict[str, Any]) -> None:
    launcher["checks"] = ["down", "other-user"]            # our child lost the bind; the squatter answers
    launcher["proc"] = FakeProc(exit_code=1)
    assert desktop.open_console([], launcher["cfg"]) == 1
    assert launcher["opened"] == [] and "another user's process" in launcher["notes"][0]


def test_open_reports_a_console_that_exits_during_start(launcher: dict[str, Any]) -> None:
    launcher["checks"] = ["down"]
    launcher["proc"] = FakeProc(exit_code=1)
    assert desktop.open_console([], launcher["cfg"]) == 1
    assert launcher["opened"] == [] and "console.log" in launcher["notes"][0]


def test_open_stops_its_child_after_the_start_timeout(launcher: dict[str, Any],
                                                      monkeypatch: pytest.MonkeyPatch) -> None:
    launcher["checks"] = ["down"]
    monkeypatch.setattr(desktop, "START_TIMEOUT_S", 0)
    assert desktop.open_console([], launcher["cfg"]) == 1
    assert launcher["opened"] == [] and launcher["proc"].terminated


def test_open_refuses_a_non_loopback_host_before_probing(launcher: dict[str, Any], tmp_path: Path) -> None:
    assert desktop.open_console([], Settings(host="evil.example", port=8793, state_dir=tmp_path)) == 1
    assert launcher["started"] == 0 and "not a loopback address" in launcher["notes"][0]


def test_open_and_stop_reject_arguments(launcher: dict[str, Any]) -> None:
    assert desktop.open_console(["--port", "1"], launcher["cfg"]) == 2
    assert desktop.stop_console(["now"], launcher["cfg"]) == 2
    assert launcher["started"] == 0


# --- background start ------------------------------------------------------------------------------------------

@pytest.fixture
def popen(monkeypatch: pytest.MonkeyPatch) -> list[tuple[list[str], dict[str, Any]]]:
    calls: list[tuple[list[str], dict[str, Any]]] = []

    def fake_popen(args: list[str], **kw: Any) -> FakeProc:
        calls.append((args, kw))
        return FakeProc()

    monkeypatch.setattr(desktop.subprocess, "Popen", fake_popen)
    return calls


def test_start_background_is_isolated_detached_and_private(popen: list[tuple[list[str], dict[str, Any]]],
                                                           tmp_path: Path) -> None:
    state = tmp_path / "state"
    desktop.start_background(Settings(state_dir=state))
    args, kw = popen[0]
    assert args == [sys.executable, "-I", "-m", "nemo_console.main"] and kw["start_new_session"] is True
    assert stat.S_IMODE(state.stat().st_mode) == 0o700
    for name in ("console.log", "console.pid"):
        assert stat.S_IMODE((state / name).stat().st_mode) == 0o600
    assert (state / "console.pid").read_text() == "4242\n"


def test_start_background_refuses_a_symlinked_log(popen: list[tuple[list[str], dict[str, Any]]],
                                                  tmp_path: Path) -> None:
    state, target = tmp_path / "state", tmp_path / "elsewhere"
    state.mkdir(mode=0o700)
    target.write_text("keep\n")
    (state / "console.log").symlink_to(target)
    with pytest.raises(OSError):
        desktop.start_background(Settings(state_dir=state))
    assert popen == [] and target.read_text() == "keep\n"


def test_start_background_tightens_an_existing_log_and_rotates_a_big_one(
        popen: list[tuple[list[str], dict[str, Any]]], tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    state = tmp_path / "state"
    state.mkdir(mode=0o755)
    log = state / "console.log"
    log.write_text("x" * 20)
    log.chmod(0o644)
    monkeypatch.setattr(desktop, "LOG_ROTATE_BYTES", 10)
    desktop.start_background(Settings(state_dir=state))
    assert (state / "console.log.1").read_text() == "x" * 20
    assert stat.S_IMODE(log.stat().st_mode) == 0o600 and stat.S_IMODE(state.stat().st_mode) == 0o700


# --- stop ------------------------------------------------------------------------------------------------------

@pytest.fixture
def stopper(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> dict[str, Any]:
    rec: dict[str, Any] = {"alive": [], "signals": [], "notes": [], "cfg": Settings(state_dir=tmp_path)}
    (tmp_path / "console.pid").write_text("4242\n")

    def fake_cmdline(pid: int) -> tuple[int, list[str]] | None:
        assert pid == 4242
        if not rec["alive"]:
            return None
        proc: tuple[int, list[str]] = rec["alive"].pop(0)
        return proc

    monkeypatch.setattr(desktop, "_pid_cmdline", fake_cmdline)
    monkeypatch.setattr(desktop, "_signal", lambda pid, sig: rec["signals"].append((pid, sig)))
    monkeypatch.setattr(desktop, "notify", rec["notes"].append)
    monkeypatch.setattr(desktop, "POLL_S", 0)
    return rec


def test_stop_terminates_the_console_it_started(stopper: dict[str, Any]) -> None:
    me = (os.getuid(), [sys.executable, "-I", "-m", "nemo_console.main"])
    stopper["alive"] = [me, me]                         # identity check, then one poll before it exits
    assert desktop.stop_console([], stopper["cfg"]) == 0
    assert stopper["signals"] == [(4242, signal.SIGTERM)]
    assert not (stopper["cfg"].state_dir / "console.pid").exists()


@pytest.mark.parametrize("proc", [(0, [sys.executable, "-I", "-m", "nemo_console.main"]),     # another user
                                  (os.getuid(), ["/usr/bin/vim", "backend/src/nemo_console/main.py"])])  # recycled pid
def test_stop_never_signals_a_process_it_did_not_start(stopper: dict[str, Any],
                                                       proc: tuple[int, list[str]]) -> None:
    stopper["alive"] = [proc]
    assert desktop.stop_console([], stopper["cfg"]) == 1
    assert stopper["signals"] == [] and "nothing stopped" in stopper["notes"][0]


def test_stop_reports_a_console_that_is_already_gone(stopper: dict[str, Any]) -> None:
    assert desktop.stop_console([], stopper["cfg"]) == 1
    assert stopper["signals"] == [] and not (stopper["cfg"].state_dir / "console.pid").exists()


def test_server_shutdown_grace_is_bounded_below_the_stop_timeout(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    """2026-10-24: with a browser holding an SSE stream, uvicorn waited forever for the connection on SIGTERM and
    `nemo-console stop` gave up after STOP_TIMEOUT_S. The server must start with a graceful-shutdown bound below it."""
    import sys

    import nemo_console.main as main_mod
    from nemo_console.desktop import STOP_TIMEOUT_S

    seen: dict[str, object] = {}

    def fake_run(self: main_mod.ConsoleServer, sockets: object = None) -> None:
        seen["grace"] = self.config.timeout_graceful_shutdown
        self.started = True

    monkeypatch.setattr(main_mod.ConsoleServer, "run", fake_run)
    monkeypatch.setattr(sys, "argv", ["nemo-console"])
    monkeypatch.setenv("NEMO_CONSOLE_STATE_DIR", str(tmp_path / "state"))
    main_mod.run()
    grace = seen.get("grace")
    assert isinstance(grace, int) and 0 < grace < STOP_TIMEOUT_S
    # and the exit hook wakes the SSE streams before uvicorn starts waiting for connections
    assert main_mod.ConsoleServer.handle_exit is not main_mod.uvicorn.Server.handle_exit
