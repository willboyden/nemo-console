"""Observability for the console itself: JSON logs (scrubbed), Prometheus /metrics, optional OTLP."""
from __future__ import annotations

import importlib
import json
import logging
from typing import Any

from prometheus_client import CollectorRegistry, Counter, Gauge, Histogram, generate_latest

from nemo_console.services.common import redact


class JsonFormatter(logging.Formatter):
    def format(self, record: logging.LogRecord) -> str:
        doc: dict[str, Any] = {"ts": self.formatTime(record, "%Y-%m-%dT%H:%M:%S%z"), "level": record.levelname,
                               "logger": record.name, "msg": redact(record.getMessage())}
        if record.exc_info and record.exc_info[0]:
            doc["exc"] = record.exc_info[0].__name__      # no traceback text: it can carry values
        return json.dumps(doc)


def configure_logging(level: int = logging.INFO) -> None:
    root = logging.getLogger()
    if any(isinstance(h.formatter, JsonFormatter) for h in root.handlers):
        return
    h = logging.StreamHandler()
    h.setFormatter(JsonFormatter())
    root.handlers = [h]
    root.setLevel(level)
    logging.getLogger("httpx").setLevel(logging.WARNING)       # httpx logs full URLs at INFO


class Telemetry:
    def __init__(self) -> None:
        self.registry = CollectorRegistry()
        self.requests = Counter("nemo_console_http_requests_total", "HTTP requests", ["method", "route", "status"],
                                registry=self.registry)
        self.latency = Histogram("nemo_console_http_request_seconds", "HTTP latency", ["method", "route"], registry=self.registry)
        self.denied = Counter("nemo_console_denied_requests_total", "Requests denied by the middleware/roles (not audited per row)",
                              ["status"], registry=self.registry)
        self.sse = Gauge("nemo_console_sse_connections", "Open SSE streams", registry=self.registry)
        self.tracer: Any = None

    def render(self) -> bytes:
        return generate_latest(self.registry)

    def setup_otlp(self, endpoint: str | None) -> None:
        """Optional: needs opentelemetry-sdk + the OTLP gRPC exporter installed; otherwise logs one warning."""
        if not endpoint:
            return
        try:
            trace = importlib.import_module("opentelemetry.trace")
            sdk = importlib.import_module("opentelemetry.sdk.trace")
            export = importlib.import_module("opentelemetry.sdk.trace.export")
            otlp = importlib.import_module("opentelemetry.exporter.otlp.proto.grpc.trace_exporter")
        except ImportError:
            logging.getLogger(__name__).warning("OTLP endpoint configured but opentelemetry packages are not installed")
            return
        provider = sdk.TracerProvider()
        provider.add_span_processor(export.BatchSpanProcessor(otlp.OTLPSpanExporter(endpoint=endpoint, insecure=True)))
        trace.set_tracer_provider(provider)
        self.tracer = trace.get_tracer("nemo_console")
