"""Redaction, secret detection and small helpers shared by the whole core."""
from __future__ import annotations

import hashlib
import re
import threading
from typing import Any

REDACTED = "[redacted]"
_PRIVATE_KEY = re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|\Z)", re.DOTALL)
_BEARER = re.compile(r"\b(Bearer)\s+[A-Za-z0-9._~+/=\-]{8,}", re.IGNORECASE)
_PREFIXED = re.compile(r"(?<![A-Za-z0-9])(?:sk-|nck_|hf_|ghp_|gho_|xox[bap]-|AKIA)[A-Za-z0-9_\-]{8,}")
_JWT = re.compile(r"\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]*")
# key = value  /  key: value  /  "key": "value" where the key NAME ends in a secret-ish word
_KEYVAL = re.compile(
    r"""(?P<pre>(?<![A-Za-z0-9])[\w.\-]{0,40}?(?:api[_\-]?key|secret|token|passw(?:or)?d|authorization)["']?\s*[=:]\s*)"""
    # The unquoted-value alternative must NOT swallow the closing quote of an enclosing JSON string, or
    # `"HF_TOKEN=",` becomes `"HF_TOKEN=[redacted],` (invalid JSON). A quote is part of the value only when it is
    # NOT followed by a delimiter; so an empty value matches nothing (nothing to hide) and `abc",` stops at `abc`.
    # An unterminated `token="abc` and a quote inside a secret (`ab"cd`) are still consumed in full.
    r"""(?P<val>"[^"\n]*"|'[^'\n]*'|(?:[^\s,;#}\]"']|["'](?![,;\]}\s]|$))+)""",
    re.IGNORECASE,
)
_ENV_NAME = re.compile(r"[A-Z_][A-Z0-9_]{2,}")
_ENV_REF = re.compile(r"(?:os\.environ/|env:|\$\{?)[A-Za-z_][A-Za-z0-9_]*\}?")
_SENSITIVE_KEY = re.compile(
    r"(?:^|[_\-])(?:api[_\-]?key|secret|token|password|passwd|authorization|credential|cookie|bearer)$", re.IGNORECASE)


class _Known:
    """Exact secret VALUES (bootstrap key, playground credential) that must never appear in any output."""

    def __init__(self) -> None:
        self._values: set[str] = set()
        self._lock = threading.Lock()

    def add(self, value: str) -> None:
        if len(value) >= 6:
            with self._lock:
                self._values.add(value)

    def scrub(self, text: str) -> str:
        with self._lock:
            values = sorted(self._values, key=len, reverse=True)
        for v in values:
            if v in text:
                text = text.replace(v, REDACTED)
        return text


KNOWN = _Known()


def _unquote(v: str) -> str:
    return v[1:-1] if len(v) >= 2 and v[0] == v[-1] and v[0] in "\"'" else v


def _is_env_reference(value: str) -> bool:
    v = _unquote(value)
    return v == "" or _ENV_NAME.fullmatch(v) is not None or _ENV_REF.fullmatch(v) is not None


CHUNK = 4000          # every regex runs on at most this many characters: linear overall, never quadratic
_USERINFO = re.compile(r"(?<=://)[^/\s:@]{1,64}:[^/\s@]{1,128}@")


def _chunks(text: str) -> list[str]:
    """Split on whitespace near the chunk edge so a token is very unlikely to straddle two chunks."""
    out: list[str] = []
    i, n = 0, len(text)
    while i < n:
        j = min(i + CHUNK, n)
        if j < n:
            k = max(text.rfind(c, i + CHUNK - 200, j) for c in (" ", "\n", "\t"))
            if k > i:
                j = k + 1
        out.append(text[i:j])
        i = j
    return out


def redact(text: str, *, keep_env_names: bool = False) -> str:
    """Scrub secret-looking substrings and any registered exact secret value from text (linear time)."""
    text = KNOWN.scrub(text)
    if len(text) <= CHUNK:
        return _redact_chunk(text, keep_env_names)
    return "".join(_redact_chunk(c, keep_env_names) for c in _chunks(text))


def _redact_chunk(text: str, keep_env_names: bool) -> str:
    text = _USERINFO.sub(REDACTED + "@", text)
    text = _PRIVATE_KEY.sub(REDACTED, text)
    text = _BEARER.sub(lambda m: f"{m.group(1)} {REDACTED}", text)
    text = _PREFIXED.sub(REDACTED, text)
    text = _JWT.sub(REDACTED, text)

    def kv(m: re.Match[str]) -> str:
        if keep_env_names and _is_env_reference(m.group("val")):
            return m.group(0)
        return m.group("pre") + REDACTED

    return _KEYVAL.sub(kv, text)


def key_is_sensitive(key: str) -> bool:
    return _SENSITIVE_KEY.search(key) is not None


def redact_obj(obj: Any, depth: int = 0) -> Any:
    """Recursively redact a JSON-like value: sensitive KEY names lose their value, strings are scrubbed."""
    if depth > 8:
        return REDACTED
    if isinstance(obj, dict):
        return {str(k): (REDACTED if key_is_sensitive(str(k)) else redact_obj(v, depth + 1))
                for k, v in list(obj.items())[:200]}
    if isinstance(obj, list | tuple):
        return [redact_obj(v, depth + 1) for v in list(obj)[:200]]
    if isinstance(obj, str):
        return redact(obj[:2000])
    return obj


def find_secret(content: str) -> str | None:
    """Return a short reason if content looks like it embeds a literal secret (env-var NAMES are fine)."""
    for chunk in _chunks(content):
        if (why := _find_secret_chunk(chunk)) is not None:
            return why
    return None


def _find_secret_chunk(content: str) -> str | None:
    if _PRIVATE_KEY.search(content):
        return "a private key block"
    if _BEARER.search(content):
        return "a 'Bearer <token>' literal"
    if _PREFIXED.search(content):
        return "a token with a well-known secret prefix (sk-, hf_, ghp_, ...)"
    if _JWT.search(content):
        return "a JWT"
    for m in _KEYVAL.finditer(content):
        val = m.group("val")
        if not _is_env_reference(val) and len(_unquote(val)) >= 8:
            key = m.group("pre").split("=")[0].split(":")[0].strip(" \"'")
            return f"a literal value for '{key[:40]}' (reference an env-var NAME instead)"
    return None


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def cap(text: str, limit: int = 8000) -> str:
    return text if len(text) <= limit else text[:limit] + "...[truncated]"
