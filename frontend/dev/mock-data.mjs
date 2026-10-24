// Canned fixtures for the mock server. Nothing here is a verified fact about the real systems: it exists so the UI
// can be built and screenshotted against contract-shaped data (ARCHITECTURE.md section 4).
export const SWITCHYARD_ROUTES = `# NeMo Switchyard routes (mock copy, trimmed from inference/switchyard/routes.toml)
schema_version = 1

[llm_clients.litellm]
format = "openai_chat"
base_url = "http://litellm:4000/v1"
forward_auth = true
max_retries = 2

[targets.lightning]
id = "nemotron-lightning"
llm_client = "litellm"

[targets.qwen-27b-1m]
id = "qwen3.8-27b-1m"
llm_client = "litellm"

[targets.flash-next]
id = "qwen3.8-flash-next"
llm_client = "litellm"

# auto-local's capable tier is the default engine (since 2026-09-26). Own client on purpose: target identity is
# (model id, llm_client), so a second target on the same id and client would be dropped.
[llm_clients.litellm_auto]
format = "openai_chat"
base_url = "http://litellm:4000/v1"
forward_auth = true
max_retries = 2

[targets.flash-next-auto]
id = "qwen3.8-flash-next"
llm_client = "litellm_auto"

# PRIMARY. Fully local.
[routes.auto-local]
id = "auto-local"
type = "stage_router"
capable_target = "flash-next-auto"
efficient_target = "lightning"
picker = "efficient_first"
confidence_threshold = 0.5
recent_turn_window = 3
context_window = 262144

[routes.auto-local.classifier]
target = "lightning"
base_threshold = 0.5
max_output_tokens = 4096

[routes.auto-flashnext]
id = "auto-flashnext"
type = "stage_router"
capable_target = "flash-next"
efficient_target = "lightning"
picker = "efficient_first"
confidence_threshold = 0.5

[routes.direct-lightning]
id = "direct-lightning"
type = "passthrough"
target = "lightning"

[routes.direct-27b]
id = "direct-27b"
type = "passthrough"
target = "qwen-27b-1m"
`;

export const RELAY_ROUTES = `# Switchyard deployment for the NeMo Relay NATIVE PLUGIN (mock copy)
# Validate with the plugin, not switchyard-server: the schemas differ.
schema_version = 1

[llm_clients.litellm_anthropic]
format = "anthropic_messages"
base_url = "http://127.0.0.1:4000"
api_key_env = "LITELLM_MASTER_KEY"
max_retries = 2

[targets.lightning]
id = "nemotron-lightning"
llm_client = "litellm_anthropic"

[targets.qwen-27b-1m]
id = "qwen3.8-27b-1m"
llm_client = "litellm_anthropic"

[targets.flash-next]
id = "qwen3.8-flash-next"
llm_client = "litellm_anthropic"

[routes.auto-local]
id = "auto-local"
type = "stage_router"
capable_target = "flash-next"
efficient_target = "lightning"
picker = "efficient_first"
confidence_threshold = 0.5
`;

export const RELAY_PLUGINS = `# ~/.config/nemo-relay/plugins.toml (mock, read-only in the console)
[[plugin]]
name = "switchyard"
path = "~/.local/share/nemo-relay/plugins/switchyard.so"
signature = "required"
public_key_env = "NEMO_RELAY_PLUGIN_PUBKEY"
`;

export const RELAY_CONFIG = `# ~/.config/nemo-relay/config.toml (mock, read-only in the console)
[gateway]
listen = "127.0.0.1:4040"

[attestation]
mode = "enforce"

[atof]
enabled = true
[atif]
enabled = true
`;

export const DYNAMO_CATALOG = [
  { key: 'served_model_name', flag: '--served-model-name', label: 'Served model name', help: 'Name clients use in requests to the frontend.', type: 'string', group: 'model', default: 'unsloth/Qwen3.8-27B-NVFP4', requires_restart: true },
  { key: 'gpu_memory_utilization', flag: '--gpu-memory-utilization', label: 'GPU memory utilisation', help: 'Fraction of the card the worker may claim. The card is shared with vllm-qwen38.', type: 'float', group: 'memory', default: 0.9, min: 0.3, max: 0.98, requires_restart: true },
  { key: 'kv_cache_dtype', flag: '--kv-cache-dtype', label: 'KV cache dtype', help: 'fp8 halves KV memory at a small quality cost.', type: 'enum', group: 'memory', default: 'auto', choices: ['auto', 'fp8', 'fp8_e4m3'], requires_restart: true },
  { key: 'max_model_len', flag: '--max-model-len', label: 'Max context length', help: 'Tokens of context the worker serves.', type: 'int', group: 'context', default: 262144, min: 1024, max: 1048576, requires_restart: true },
  { key: 'max_num_seqs', flag: '--max-num-seqs', label: 'Max concurrent sequences', help: 'Upper bound on sequences batched together.', type: 'int', group: 'scheduling', default: 64, min: 1, max: 1024, requires_restart: true },
  { key: 'enable_prefix_caching', flag: '--enable-prefix-caching', label: 'Prefix caching', help: 'Reuse KV for shared prompt prefixes.', type: 'bool', group: 'scheduling', default: true, requires_restart: true },
  { key: 'tensor_parallel_size', flag: '--tensor-parallel-size', label: 'Tensor parallel size', help: 'GPUs per worker. This lab pins Dynamo to one card.', type: 'int', group: 'parallelism', default: 1, min: 1, max: 2, requires_restart: true },
  { key: 'router_mode', flag: '--router-mode', label: 'Router mode', help: 'How the frontend picks a worker.', type: 'enum', group: 'network', default: 'round-robin', choices: ['round-robin', 'random', 'kv'], requires_restart: false },
  { key: 'enforce_eager', flag: '--enforce-eager', label: 'Enforce eager', help: 'Disable CUDA graphs (slower, lower memory).', type: 'bool', group: 'performance', default: false, advanced: true, requires_restart: true },
  { key: 'hf_token', flag: 'env:HF_TOKEN', label: 'Hugging Face token', help: 'Only needed for gated models. Referenced by env name on the server; never shown.', type: 'string', group: 'advanced', advanced: true, requires_restart: true },
  { key: 'extra_worker_args', flag: null, label: 'Extra worker arguments', help: 'Passed through verbatim after the flags above.', type: 'string_list', group: 'advanced', advanced: true, requires_restart: true },
];

// ── Data Designer (mock) ────────────────────────────────────────────────────────────────────
// Canned fixtures shaped like the contract addendum: config scripts in scripts_dir, run metadata
// (never record text in events) and records for the /runs/{id}/records|files endpoints.
export const DD_SCRIPTS = ['support_tickets.py', 'code_review_prs.py', 'release_notes.py'];
export const DD_COLS = ['id', 'subject', 'priority', 'assignee', 'created_at', 'body'];

export const DD_SCRIPT = `# Generate a synthetic support-ticket dataset through the lab router.
# Every generation call goes to LiteLLM on 127.0.0.1:4000 (alias lab-text).
from data_designer.config import DataDesignerConfigBuilder
from data_designer.config.models import ModelProvider, ModelConfig

PROVIDER_NAME = "lab-router"
PROVIDER_ENDPOINT = "http://127.0.0.1:4000/v1"
API_KEY_ENV = "LITELLM_DD_KEY"      # env NAME only; the wrapper injects the value

PROVIDER = ModelProvider(
    name=PROVIDER_NAME,
    endpoint=PROVIDER_ENDPOINT,
    provider_type="openai",
    api_key=API_KEY_ENV,
)

def load_config_builder() -> DataDesignerConfigBuilder:
    builder = DataDesignerConfigBuilder(model_providers=[PROVIDER])
    builder.add_model_config(ModelConfig(alias="lab-text", model="glm-5.3-flash-next-local", provider=PROVIDER_NAME))
    builder.add_column(name="subject", prompt="One-line summary of a plausible support ticket.", info_type="text")
    builder.add_column(name="body", prompt="Three sentences describing the ticket in detail.", info_type="text")
    return builder
`;

const SUBJECTS = ['Login loop after SSO change', 'Export produces empty CSV', 'Webhook retries flood the inbox', 'Rate limit on the bulk API', 'Dark mode contrast too low', 'Search misses accented terms', 'Billing page shows stale totals', 'Mobile app crashes on upload'];
const PRIO = ['low', 'medium', 'high', 'urgent'];
const AGENTS = ['ana', 'kwame', 'priya', 'tom'];

/** n deterministic canned records (mock only). `body` is deliberately long so the UI truncation shows. */
export function ddRows(n) {
  const rows = [];
  for (let i = 1; i <= n; i++) {
    const day = String((i % 28) + 1).padStart(2, '0');
    rows.push({
      id: i,
      subject: `${SUBJECTS[i % SUBJECTS.length]} (#${1000 + i})`,
      priority: PRIO[i % PRIO.length],
      assignee: AGENTS[i % AGENTS.length],
      created_at: `2026-10-${day}T09:${String(i % 60).padStart(2, '0')}:00Z`,
      body: `Customer reports ${SUBJECTS[i % SUBJECTS.length].toLowerCase()} after the 2026-10-${day} update. Reproduced twice on the staging tenant; support asked for the tenant id and the last audit entry before escalating to engineering. Ticket synthesized by Data Designer for evaluation use only.`,
    });
  }
  return rows;
}

export const DD_PREVIEW_FILES = (n) => [
  { path: 'preview_results_20261023_091512/dataset.parquet', bytes: 8000 + n * 300 },
  { path: 'preview_results_20261023_091512/report.html', bytes: 42000 },
  { path: 'preview_results_20261023_091512/sample_records/record_00000.html', bytes: 5100 },
  { path: 'preview_results_20261023_091512/sample_records/sample_records_browser.html', bytes: 9800 },
  { path: 'records.jsonl', bytes: 900 + n * 320 },
];

export const DD_CREATE_FILES = (live) => [
  { path: `${live.dataset_name}/metadata.json`, bytes: 2400 },
  { path: `${live.dataset_name}/parquet-files/batch_00000.parquet`, bytes: 6000 + live.num_records * 300 },
  { path: `${live.dataset_name}/builder_config.json`, bytes: 7800 },
  { path: 'records.jsonl', bytes: 900 + Math.min(live.num_records, 2000) * 320 },
];

const CREATE_LIVE = { num_records: 120, dataset_name: 'support_tickets', destination: 'support_tickets', format: 'parquet' };

// Newest first, like the backend's run-dir listing. rows are the records the mock "records.jsonl" holds.
export const DD_RUNS = [
  { run_id: '20261023T091512-104-create-support_tickets', script: 'support_tickets.py', mode: 'create', verdict: 'succeeded',
    num_records: 120, dataset_name: 'support_tickets', destination: 'support_tickets', format: 'parquet',
    duration_ms: 742000, ts: Date.now() / 1000 - 5400, columns: DD_COLS, rows: ddRows(120), files: DD_CREATE_FILES(CREATE_LIVE) },
  { run_id: '20261023T084500-011-preview-code_review_prs', script: 'code_review_prs.py', mode: 'preview', verdict: 'succeeded',
    num_records: 5, dataset_name: null, destination: null, format: 'parquet',
    duration_ms: 6100, ts: Date.now() / 1000 - 9600, columns: DD_COLS, rows: ddRows(5), files: DD_PREVIEW_FILES(5) },
  { run_id: '20261022T170001-900-create-release_notes', script: 'release_notes.py', mode: 'create', verdict: 'failed',
    num_records: 0, dataset_name: 'release_notes', destination: 'release_notes', format: 'csv',
    duration_ms: 91000, ts: Date.now() / 1000 - 86400, columns: DD_COLS, rows: [],
    error: 'lab-lightning answered 429 for 3 consecutive retries (records: 0 written)', files: [] },
];
