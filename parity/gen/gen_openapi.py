"""Write backend/crates/nemo-console/assets/openapi.json: the Python app's OpenAPI document, served as-is by the Rust
backend at /api/v1/openapi.json (the API is the same, so the document is the same).

    REF=$(node parity/ref.mjs) && cd backend && env -i HOME=$(mktemp -d) PATH=/nonexistent NEMO_CONSOLE_NO_USER_ENV=1 "$REF/.venv/bin/python" -I \
        ../parity/gen/gen_openapi.py
"""
import json
import tempfile
from pathlib import Path

from nemo_console.config import Settings
from nemo_console.main import create_app

OUT = Path(__file__).resolve().parents[2] / "backend" / "crates" / "nemo-console" / "assets" / "openapi.json"
tmp = Path(tempfile.mkdtemp(prefix="nc-openapi-"))
app = create_app(Settings(state_dir=tmp / "s", repo_root=tmp / "r", background_tasks=False))
OUT.parent.mkdir(parents=True, exist_ok=True)
OUT.write_text(json.dumps(app.openapi(), separators=(",", ":")))
print(f"wrote {OUT}")
