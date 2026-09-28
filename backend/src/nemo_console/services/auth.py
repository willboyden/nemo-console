"""Bearer keys: stored as sha256 only, key files written 0600 with fchmod, roles admin/viewer, loopback trust."""
from __future__ import annotations

import hmac
import os
import secrets
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

from nemo_console.domain.models import Role
from nemo_console.services.common import KNOWN, sha256_hex
from nemo_console.services.store import Store

LOOPBACK_CLIENTS = {"127.0.0.1", "::1", "localhost"}


@dataclass(frozen=True)
class Principal:
    name: str
    role: Role | Literal["public"]


PUBLIC = Principal("public", "public")


def write_private_file(path: Path, data: str) -> None:
    """Create exclusively with 0600 and fchmod (umask cannot widen it)."""
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        os.fchmod(fd, 0o600)
        os.write(fd, data.encode())
    finally:
        os.close(fd)


class AuthService:
    def __init__(self, store: Store, state_dir: Path, trust_loopback: bool) -> None:
        self.store = store
        self.state_dir = state_dir
        self.trust_loopback = trust_loopback

    def mint(self, role: Role, note: str = "") -> tuple[str, str]:
        """Returns (key_id, plaintext). The plaintext is shown once by the caller and never stored."""
        key = "nck_" + secrets.token_urlsafe(32)
        kid = uuid.uuid4().hex[:12]
        KNOWN.add(key)
        self.store.execute("INSERT INTO keys(id, role, sha256, created, note) VALUES (?,?,?,?,?)",
                           (kid, role, sha256_hex(key.encode()), time.time(), note))
        return kid, key

    def ensure_bootstrap_key(self) -> Path | None:
        if self.store.query("SELECT 1 FROM keys LIMIT 1"):
            return None
        _, key = self.mint("admin", "bootstrap")
        path = self.state_dir / "bootstrap-admin.key"
        path.unlink(missing_ok=True)
        write_private_file(path, key + "\n")
        return path

    def authenticate(self, token: str) -> Principal | None:
        digest = sha256_hex(token.encode())
        found: Principal | None = None
        for row in self.store.query("SELECT id, role, sha256 FROM keys"):   # no early exit: constant-ish time
            if hmac.compare_digest(row["sha256"], digest):
                found = Principal(f"key:{row['id']}", row["role"])
        return found

    def principal_for(self, authorization: str | None, client_host: str | None, forwarded: bool) -> Principal | None:
        if authorization:
            scheme, _, tok = authorization.partition(" ")
            if scheme.lower() != "bearer" or not tok.strip():
                return None
            return self.authenticate(tok.strip())
        if self.trust_loopback and client_host in LOOPBACK_CLIENTS and not forwarded:
            return Principal("loopback", "admin")
        return None
