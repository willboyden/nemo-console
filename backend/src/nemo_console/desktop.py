"""`nemo-console open`: the desktop launcher's entry point.

Starts the console in the background if nothing is serving yet, waits for `/api/v1/health`, then opens the UI with
`xdg-open`. It never reads or passes a key: the UI still asks for a bearer. A desktop launch has no terminal, so
failures also go to `notify-send` when it exists.
"""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
import time
from typing import Literal

import httpx

from nemo_console.config import Settings, user_env_file

START_TIMEOUT_S = 30.0
POLL_S = 0.5
HEALTH_KEYS = {"status", "version", "trust", "systems"}
Probe = Literal["console", "foreign", "down"]


def console_url(cfg: Settings) -> str:
    host = "127.0.0.1" if cfg.host in ("", "0.0.0.0") else "[::1]" if cfg.host == "::" else cfg.host
    if ":" in host and not host.startswith("["):
        host = f"[{host}]"
    return f"http://{host}:{cfg.port}/"


def probe(url: str) -> Probe:
    """`console` if Nemo Console answers /health, `foreign` if something else holds the port, `down` if nothing does."""
    try:
        r = httpx.get(f"{url}api/v1/health", timeout=2.0, trust_env=False, follow_redirects=False)
        body = r.json()
    except httpx.HTTPError:
        return "down"
    except ValueError:
        return "foreign"
    return "console" if isinstance(body, dict) and HEALTH_KEYS <= body.keys() else "foreign"


def start_background(cfg: Settings) -> subprocess.Popen[bytes]:
    cfg.state_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd = os.open(cfg.state_dir / "console.log", os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    with os.fdopen(fd, "ab") as log:    # the child keeps its own copy of the descriptor
        return subprocess.Popen([sys.executable, "-m", "nemo_console.main"], stdin=subprocess.DEVNULL, stdout=log,
                                stderr=log, start_new_session=True)


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
        subprocess.run([tool, "Nemo Console", message], check=False, timeout=5, stdin=subprocess.DEVNULL,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def open_console(argv: list[str], cfg: Settings | None = None) -> int:
    if argv:
        print("usage: nemo-console open", file=sys.stderr)
        return 2
    cfg = cfg or Settings(_env_file=user_env_file())
    url = console_url(cfg)
    state = probe(url)
    if state == "foreign":
        notify(f"something other than Nemo Console answers on {url}; not opening it")
        return 1
    if state == "down":
        proc = start_background(cfg)
        deadline = time.monotonic() + START_TIMEOUT_S
        while probe(url) != "console":      # probe first: a concurrent launch may have won the port
            if proc.poll() is not None or time.monotonic() > deadline:
                notify(f"the console did not start; see {cfg.state_dir / 'console.log'}")
                return 1
            time.sleep(POLL_S)
        print(f"started Nemo Console in the background (pid {proc.pid}); log: {cfg.state_dir / 'console.log'}")
    if not open_browser(url):
        notify(f"xdg-open not found; open {url} in a browser")
        return 1
    return 0
