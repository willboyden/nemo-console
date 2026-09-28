"""`nemo-console open` / `nemo-console stop`: the desktop launcher's entry points (Linux).

`open` starts the console in the background if nothing is serving yet, waits for `/api/v1/health`, then opens the UI
with `xdg-open`. It opens a page only on a loopback address, and only when every socket listening on the port belongs
to the current user and the answer looks like Nemo Console. A process running as the same user can still impersonate
it (THREAT-MODEL S-6, R-2). It never reads or passes a key: the UI still asks for one. `stop` stops the console that
`open` started (tracked in `<state dir>/console.pid`). A desktop launch has no terminal, so failures also go to
`notify-send` when it exists.
"""
from __future__ import annotations

import html
import ipaddress
import os
import shutil
import signal
import stat
import subprocess
import sys
import time
from pathlib import Path
from typing import Literal

import httpx

from nemo_console.config import Settings, user_env_file

START_TIMEOUT_S = 30.0
STOP_TIMEOUT_S = 10.0
POLL_S = 0.5
LOG_ROTATE_BYTES = 10 * 1024 * 1024
HEALTH_KEYS = {"status", "version", "trust", "systems"}
PROC_NET_TABLES = ("/proc/net/tcp", "/proc/net/tcp6")
# -I (isolated): the launch directory must not land on the admin server's sys.path, where a stray uvicorn.py or
# secrets.py would run inside it.
SERVER_ARGS = ["-I", "-m", "nemo_console.main"]
Check = Literal["console", "other-user", "foreign", "down"]


class LauncherError(Exception):
    """A refusal the launcher reports (notification + exit status 1)."""


def console_url(cfg: Settings) -> str:
    """The console's URL, loopback only: neither the probe nor the browser may be sent off the box."""
    host = cfg.host.strip()
    if host in ("", "0.0.0.0"):
        host = "127.0.0.1"
    elif host in ("::", "[::]"):
        host = "::1"
    if host != "localhost":
        try:
            ip = ipaddress.ip_address(host.removeprefix("[").removesuffix("]"))
        except ValueError:
            ip = None
        if ip is None or not ip.is_loopback:
            raise LauncherError(f"host {cfg.host!r} is not a loopback address; the launcher only opens a local console")
        host = f"[{ip}]" if ip.version == 6 else str(ip)
    if not 1 <= cfg.port <= 65535:
        raise LauncherError(f"port {cfg.port} is out of range")
    return f"http://{host}:{cfg.port}/"


def listen_uids(port: int, tables: tuple[str, ...] = PROC_NET_TABLES) -> set[int] | None:
    """Owners of every LISTEN socket on `port`, any address, IPv4 and IPv6. None if a table is unreadable."""
    uids: set[int] = set()
    for table in tables:
        try:
            rows = Path(table).read_text(encoding="ascii").splitlines()[1:]
        except FileNotFoundError:
            continue                            # no IPv6
        except OSError:
            return None
        for row in rows:
            f = row.split()
            if len(f) > 7 and f[3] == "0A" and int(f[1].rsplit(":", 1)[1], 16) == port:
                uids.add(int(f[7]))
    return uids


def held_by_me(port: int) -> bool:
    uids = listen_uids(port)
    return bool(uids) and uids == {os.getuid()}


def probe(url: str) -> Literal["console", "foreign", "down"]:
    """`console` if the answer to /health has the console's shape, `foreign` for any other answer, `down` for none."""
    try:
        r = httpx.get(f"{url}api/v1/health", timeout=2.0, trust_env=False, follow_redirects=False)
        body = r.json()
    except httpx.HTTPError:
        return "down"
    except ValueError:
        return "foreign"
    return "console" if isinstance(body, dict) and HEALTH_KEYS <= body.keys() else "foreign"


def check(url: str, port: int) -> Check:
    state = probe(url)
    if state == "console" and not held_by_me(port):
        return "other-user"                     # answers like the console, but not every listener is ours
    return state


def _private_state_dir(state_dir: Path) -> None:
    state_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    st = os.lstat(state_dir)
    if not stat.S_ISDIR(st.st_mode) or st.st_uid != os.getuid():
        raise LauncherError(f"state dir {state_dir} is not a directory you own")
    os.chmod(state_dir, 0o700)


def _open_private(path: Path, flags: int) -> int:
    """Open a regular file we own, never through a symlink or a FIFO, and force mode 0600."""
    fd = os.open(path, flags | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, 0o600)
    st = os.fstat(fd)
    if not stat.S_ISREG(st.st_mode) or st.st_uid != os.getuid():
        os.close(fd)
        raise LauncherError(f"{path} is not a regular file you own")
    os.fchmod(fd, 0o600)
    return fd


def _rotate(log: Path) -> None:
    try:
        st = os.lstat(log)
    except FileNotFoundError:
        return
    if stat.S_ISREG(st.st_mode) and st.st_size > LOG_ROTATE_BYTES:
        os.replace(log, log.with_name(log.name + ".1"))


def start_background(cfg: Settings) -> subprocess.Popen[bytes]:
    _private_state_dir(cfg.state_dir)
    log = cfg.state_dir / "console.log"
    _rotate(log)
    fd = _open_private(log, os.O_WRONLY | os.O_APPEND)
    try:        # the child gets its own copies of the descriptor
        proc = subprocess.Popen([sys.executable, *SERVER_ARGS], stdin=subprocess.DEVNULL, stdout=fd, stderr=fd,
                                start_new_session=True)
    finally:
        os.close(fd)
    pid_fd = _open_private(cfg.state_dir / "console.pid", os.O_WRONLY | os.O_TRUNC)
    try:
        os.write(pid_fd, f"{proc.pid}\n".encode())
    finally:
        os.close(pid_fd)
    return proc


def _stop_child(proc: subprocess.Popen[bytes]) -> None:
    proc.terminate()
    try:
        proc.wait(5)
    except subprocess.TimeoutExpired:
        proc.kill()


def open_browser(url: str) -> bool:
    opener = shutil.which("xdg-open")
    if opener is None:
        return False
    subprocess.Popen([opener, url], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                     start_new_session=True)
    return True


def notify(message: str) -> None:
    print(f"nemo-console: {message}", file=sys.stderr)
    tool = shutil.which("notify-send")
    if tool:
        try:    # the body may be parsed as markup
            subprocess.run([tool, "--", "Nemo Console", html.escape(message, quote=False)], check=False, timeout=5,
                           stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except (subprocess.TimeoutExpired, OSError):
            pass


def _other_user(url: str) -> LauncherError:
    return LauncherError(f"another user's process is listening on the console's port ({url}); not opening it")


def _open(cfg: Settings) -> None:
    url = console_url(cfg)
    state = check(url, cfg.port)
    if state == "other-user":
        raise _other_user(url)
    if state == "foreign":
        raise LauncherError(f"something other than Nemo Console answers on {url}; not opening it")
    if state == "down":
        proc = start_background(cfg)
        deadline = time.monotonic() + START_TIMEOUT_S
        while (state := check(url, cfg.port)) != "console":     # check first: a concurrent launch may have won
            if state == "other-user":
                _stop_child(proc)
                raise _other_user(url)
            if proc.poll() is not None:
                raise LauncherError(f"the console did not start; see {cfg.state_dir / 'console.log'}")
            if time.monotonic() > deadline:
                _stop_child(proc)
                raise LauncherError(f"the console did not become healthy in {START_TIMEOUT_S:.0f} s; "
                                    f"see {cfg.state_dir / 'console.log'}")
            time.sleep(POLL_S)
        print(f"started Nemo Console in the background (pid {proc.pid}); log: {cfg.state_dir / 'console.log'}")
    if not open_browser(url):
        raise LauncherError(f"xdg-open not found; open {url} in a browser")


def _pid_cmdline(pid: int) -> tuple[int, list[str]] | None:
    """(owner uid, argv) of a live process, or None if it is gone."""
    try:
        uid = os.stat(f"/proc/{pid}").st_uid
        raw = Path(f"/proc/{pid}/cmdline").read_bytes()
    except OSError:
        return None
    return uid, [a.decode(errors="replace") for a in raw.split(b"\0") if a]


def _signal(pid: int, sig: int) -> None:
    os.kill(pid, sig)


def _stop(cfg: Settings) -> None:
    pid_file = cfg.state_dir / "console.pid"
    try:
        fd = os.open(pid_file, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    except FileNotFoundError:
        raise LauncherError("no console started by `nemo-console open` is recorded") from None
    try:
        text = os.read(fd, 64).decode(errors="replace").strip()
    finally:
        os.close(fd)
    if not text.isdigit():
        raise LauncherError(f"{pid_file} does not hold a pid")
    pid = int(text)
    proc = _pid_cmdline(pid)
    if proc is None:
        pid_file.unlink(missing_ok=True)
        raise LauncherError("the console started by `nemo-console open` is no longer running")
    uid, argv = proc
    if uid != os.getuid() or argv[1:] != SERVER_ARGS:     # a recycled pid: never signal a stranger
        pid_file.unlink(missing_ok=True)
        raise LauncherError(f"pid {pid} is not a console started by `nemo-console open`; nothing stopped")
    _signal(pid, signal.SIGTERM)
    deadline = time.monotonic() + STOP_TIMEOUT_S
    while _pid_cmdline(pid) is not None:
        if time.monotonic() > deadline:
            raise LauncherError(f"pid {pid} did not exit within {STOP_TIMEOUT_S:.0f} s")
        time.sleep(POLL_S)
    pid_file.unlink(missing_ok=True)
    print(f"stopped Nemo Console (pid {pid})")


def _run(command: str, argv: list[str], cfg: Settings | None) -> int:
    if argv:
        print(f"usage: nemo-console {command}", file=sys.stderr)
        return 2
    cfg = cfg or Settings(_env_file=user_env_file())
    try:
        (_open if command == "open" else _stop)(cfg)
    except (LauncherError, OSError) as e:
        notify(str(e))
        return 1
    return 0


def open_console(argv: list[str], cfg: Settings | None = None) -> int:
    return _run("open", argv, cfg)


def stop_console(argv: list[str], cfg: Settings | None = None) -> int:
    return _run("stop", argv, cfg)
