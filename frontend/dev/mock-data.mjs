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

# PRIMARY. Fully local.
[routes.auto-local]
id = "auto-local"
type = "stage_router"
capable_target = "qwen-27b-1m"
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

[routes.auto-local]
id = "auto-local"
type = "stage_router"
capable_target = "qwen-27b-1m"
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
