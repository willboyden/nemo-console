"""Config files (validate / apply / history / rollback) and structured params."""
from __future__ import annotations

from typing import Annotated, Any

from fastapi import APIRouter
from pydantic import BaseModel, ConfigDict, Field

from nemo_console.api.deps import Adapter, Admin, C, FileId, Viewer, exclusive
from nemo_console.domain.models import (
    ApplyResult,
    ConfigFile,
    ConfigFileSpec,
    ConfigRevision,
    ParamSpec,
    ValidationResult,
)
from nemo_console.services.common import cap, redact
from nemo_console.services.errors import ApiError
from nemo_console.services.params import validate_values

router = APIRouter()
Sha = Annotated[str, Field(pattern=r"^[0-9a-f]{64}$")]


class _Body(BaseModel):
    model_config = ConfigDict(extra="forbid")


class ValidateBody(_Body):
    content: str


class ApplyBody(_Body):
    content: str
    base_sha256: Sha
    restart: bool = True
    confirm: bool = False


class RollbackBody(_Body):
    revision_id: Annotated[str, Field(pattern=r"^[0-9a-f]{8,32}$")]
    confirm: bool = False


class ParamsBody(_Body):
    values: dict[str, Any] = {}


class ParamsPutBody(ParamsBody):
    confirm: bool = False


@router.get("/systems/{system_id}/config", response_model=list[ConfigFileSpec])
async def config_list(a: Adapter, _: Viewer) -> list[ConfigFileSpec]:
    return a.config_files()


@router.get("/systems/{system_id}/config/{file_id}", response_model=ConfigFile)
async def config_get(a: Adapter, c: C, file_id: FileId, p: Viewer) -> ConfigFile:
    cf = await c.config_store.read(a, file_id)
    if p.role != "admin":      # viewers never see literal secrets that an unvetted on-disk file may hold (env NAMES stay)
        cf = cf.model_copy(update={"content": redact(cf.content, keep_env_names=True)})
    return cf


@router.post("/systems/{system_id}/config/{file_id}/validate", response_model=ValidationResult)
async def config_validate(a: Adapter, c: C, file_id: FileId, body: ValidateBody, _: Admin) -> ValidationResult:
    return await c.config_store.validate(a, file_id, body.content)      # bounded by the store's validate semaphore


@router.post("/systems/{system_id}/config/{file_id}/apply", response_model=ApplyResult)
async def config_apply(a: Adapter, c: C, file_id: FileId, body: ApplyBody, p: Admin) -> ApplyResult:
    async with exclusive(c, a.id):
        return await c.config_store.apply(a, file_id, content=body.content, base_sha256=body.base_sha256,
                                          restart=body.restart, confirm=body.confirm, actor=p.name)


@router.get("/systems/{system_id}/config/{file_id}/history", response_model=list[ConfigRevision])
async def config_history(a: Adapter, c: C, file_id: FileId, _: Viewer) -> list[ConfigRevision]:
    return c.config_store.history(a, file_id)


@router.post("/systems/{system_id}/config/{file_id}/rollback", response_model=ApplyResult)
async def config_rollback(a: Adapter, c: C, file_id: FileId, body: RollbackBody, p: Admin) -> ApplyResult:
    async with exclusive(c, a.id):
        return await c.config_store.rollback(a, file_id, revision_id=body.revision_id, confirm=body.confirm, actor=p.name)


# ── structured params ───────────────────────────────────────────────────────────────────────
@router.get("/systems/{system_id}/params")
async def params_get(a: Adapter, c: C, _: Viewer) -> dict[str, Any]:
    catalog: list[ParamSpec] = a.param_catalog()
    values = c.settings.saved_params(a.id) if catalog else {}
    return {"catalog": [s.model_dump() for s in catalog], "values": values}


@router.post("/systems/{system_id}/params/preview")
async def params_preview(a: Adapter, body: ParamsBody, _: Admin) -> dict[str, Any]:
    catalog = a.param_catalog()
    if not catalog:
        raise ApiError(404, "unsupported", "this system has no tunable parameters")
    values = validate_values(catalog, body.values)
    try:
        rendered = await a.preview_params(values)
    except NotImplementedError:
        raise ApiError(404, "unsupported", "this system cannot preview parameters") from None
    except ValueError as e:     # the adapter's own refusal (Dynamo: no repo compose to render from, a bad combination)
        raise ApiError(422, "params_invalid", redact(str(e))[:500]) from None
    warn_fn = getattr(a, "param_warnings", None)      # optional adapter extra (Dynamo)
    warnings: list[str] = []
    if callable(warn_fn):
        try:
            warnings = [redact(str(w))[:500] for w in warn_fn(values)][:20]
        except Exception:  # noqa: BLE001 - warnings are advisory, never fail the preview
            warnings = []
    return {"rendered": redact(cap(rendered, 200_000)), "warnings": warnings}


@router.put("/systems/{system_id}/params", response_model=ApplyResult)
async def params_put(a: Adapter, c: C, body: ParamsPutBody, _: Admin) -> ApplyResult:
    catalog = a.param_catalog()
    if not catalog:
        raise ApiError(404, "unsupported", "this system has no tunable parameters")
    if not body.confirm:
        raise ApiError(400, "confirm_required", "confirm:true is required to apply parameters")
    values = validate_values(catalog, body.values)
    async with exclusive(c, a.id):
        try:
            res = await a.apply_params(values)
        except NotImplementedError:
            raise ApiError(404, "unsupported", "this system cannot apply parameters") from None
        except Exception as e:  # noqa: BLE001
            return ApplyResult(ok=False, detail=redact(f"apply failed: {type(e).__name__}: {e}")[:500])
        if res.ok:
            c.settings.save_params(a.id, values)
        return res.model_copy(update={"detail": cap(redact(res.detail), 2000)})
