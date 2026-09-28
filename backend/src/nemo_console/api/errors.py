"""`{error:{code,message}}` envelope for every failure, including framework-generated ones."""
from __future__ import annotations

import logging

from fastapi import FastAPI, Request
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse
from starlette.exceptions import HTTPException as StarletteHTTPException

from nemo_console.services.common import redact
from nemo_console.services.errors import ApiError

log = logging.getLogger(__name__)
CODES = {400: "bad_request", 401: "unauthorized", 403: "forbidden", 404: "not_found", 405: "method_not_allowed",
         409: "conflict", 413: "payload_too_large", 421: "misdirected_request", 422: "invalid_request",
         429: "too_many_requests", 502: "bad_gateway", 503: "unavailable"}


def problem(status: int, code: str, message: str, headers: dict[str, str] | None = None) -> JSONResponse:
    return JSONResponse({"error": {"code": code, "message": redact(message)[:1200]}}, status_code=status, headers=headers)


def install_error_handlers(app: FastAPI) -> None:
    @app.exception_handler(ApiError)
    async def _api(_: Request, e: ApiError) -> JSONResponse:
        return problem(e.status, e.code, e.message)

    @app.exception_handler(StarletteHTTPException)
    async def _http(_: Request, e: StarletteHTTPException) -> JSONResponse:
        return problem(e.status_code, CODES.get(e.status_code, "error"), str(e.detail))

    @app.exception_handler(RequestValidationError)
    async def _val(_: Request, e: RequestValidationError) -> JSONResponse:
        # only field locations + messages: never echo the submitted input (it can hold secrets)
        detail = "; ".join(f"{'.'.join(str(x) for x in err['loc'])}: {err['msg']}" for err in e.errors()[:5])
        return problem(422, "invalid_request", detail)
