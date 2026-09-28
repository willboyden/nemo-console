"""Validate user-supplied values against ParamSpec descriptors (used by actions and by the params endpoints)."""
from __future__ import annotations

from typing import Any

from nemo_console.domain.models import ParamSpec
from nemo_console.services.errors import ApiError

MAX_STR = 1024
MAX_LIST = 64


def _bad(key: str, why: str) -> ApiError:
    return ApiError(422, "invalid_param", f"param '{key}': {why}")


def validate_values(specs: list[ParamSpec], values: dict[str, Any]) -> dict[str, Any]:
    by_key = {s.key: s for s in specs}
    out: dict[str, Any] = {}
    for key, v in values.items():
        s = by_key.get(key)
        if s is None:
            raise _bad(str(key)[:60], "unknown")
        out[key] = _check(s, v)
    return out


def _check(s: ParamSpec, v: Any) -> Any:
    t = s.type
    if t in ("int", "float"):
        if isinstance(v, bool) or not isinstance(v, int | float) or (t == "int" and isinstance(v, float) and not v.is_integer()):
            raise _bad(s.key, f"expected {t}")
        v = int(v) if t == "int" else float(v)
        if s.min is not None and v < s.min:
            raise _bad(s.key, f"below minimum {s.min}")
        if s.max is not None and v > s.max:
            raise _bad(s.key, f"above maximum {s.max}")
        return v
    if t == "bool":
        if not isinstance(v, bool):
            raise _bad(s.key, "expected bool")
        return v
    if t == "enum":
        if not isinstance(v, str) or v not in (s.choices or []):
            raise _bad(s.key, "not one of the allowed choices")
        return v
    if t == "string":
        return _string(s.key, v)
    if t == "string_list":
        if not isinstance(v, list) or len(v) > MAX_LIST:
            raise _bad(s.key, "expected a short list of strings")
        return [_string(s.key, x) for x in v]
    raise _bad(s.key, "unsupported type")


def _string(key: str, v: Any) -> str:
    if not isinstance(v, str) or len(v) > MAX_STR or "\x00" in v or "\n" in v or "\r" in v:
        raise _bad(key, "expected a single-line string")
    return v
