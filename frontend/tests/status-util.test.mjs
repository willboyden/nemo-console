import test from 'node:test';
import assert from 'node:assert/strict';
import { normalizeRole, auditLabel, isLoading, attestation, healthChanges, parsePreflight } from '../js/status-util.js';
import { extractDelta, chatBody, servedBy } from '../js/stream-text.js';
import { clampMaxTokens } from '../js/format.js';

test('isLoading: degraded loading is not down; down is never loading', () => {
  assert.equal(isLoading({ health: 'degraded', summary: 'Worker loading model weights', components: [], problems: [] }), true);
  assert.equal(isLoading({ health: 'degraded', summary: 'x', components: [{ detail: 'loading weights 62%' }], problems: [] }), true);
  assert.equal(isLoading({ health: 'degraded', summary: 'x', components: [], problems: [{ code: 'dynamo.worker.loading' }] }), true);
  assert.equal(isLoading({ health: 'down', summary: 'loading', components: [], problems: [] }), false); assert.equal(isLoading({ health: 'up', summary: 'Serving', components: [], problems: [] }), false);
});
test('attestation comes from the attestation component', () => {
  assert.deepEqual(attestation({ components: [{ name: 'attestation', state: 'running', detail: 'mode=enforce' }] }), { state: 'verified', detail: 'mode=enforce' });
  assert.equal(attestation({ components: [{ name: 'Attestation check', state: 'failed' }] }).state, 'failed'); assert.equal(attestation({ components: [] }).state, 'unreported'); assert.equal(attestation(null).state, 'unreported');
});
test('healthChanges reports only flips', () => {
  const a = { systems: [{ system: 'dynamo', health: 'up' }, { system: 'relay', health: 'up' }] }, b = { systems: [{ system: 'dynamo', health: 'down' }, { system: 'relay', health: 'up' }] };
  assert.deepEqual(healthChanges(a, b), [{ id: 'dynamo', from: 'up', to: 'down' }]); assert.deepEqual(healthChanges(null, b), []);
});
test('parsePreflight names the sharing container, its state and the dark aliases', () => {
  const out = 'dynamo_state: up\nshares_card: ai-lab-vllm-qwen38-vllm-1 (stopped)\nrouter_aliases_dark: qwen3.8-27b-1m, switchyard capable tier (auto-local cannot escalate)\nother_cards: untouched';
  const p = parsePreflight(out); assert.equal(p.container, 'ai-lab-vllm-qwen38-vllm-1'); assert.equal(p.sharingRunning, false); assert.deepEqual(p.darkAliases, ['qwen3.8-27b-1m', 'switchyard capable tier']);
  assert.equal(parsePreflight('shares_card: ai-lab-vllm-qwen38-vllm-1 (running)').sharingRunning, true);
  assert.equal(parsePreflight('shares_card: ai-lab-vllm-qwen38-vllm-1 (not running)').sharingRunning, false);
  const none = parsePreflight('free text without structure'); assert.equal(none.parsed, false); assert.equal(none.container, null);
});
test('extractDelta: OpenAI chat and Anthropic Messages streams', () => {
  assert.deepEqual(extractDelta('openai_chat', { data: { choices: [{ delta: { content: 'Hi' } }] } }), { text: 'Hi', usage: undefined, done: undefined });
  assert.equal(extractDelta('openai_chat', { data: { choices: [{ delta: {}, finish_reason: 'stop' }], usage: { completion_tokens: 3 } } }).done, true);
  assert.equal(extractDelta('anthropic_messages', { data: { type: 'content_block_delta', delta: { type: 'text_delta', text: 'Yo' } } }).text, 'Yo');
  assert.deepEqual(extractDelta('anthropic_messages', { data: { type: 'message_delta', usage: { output_tokens: 9 } } }).usage, { completion_tokens: 9 });
  assert.equal(extractDelta('openai_chat', { data: 'not json' }).text, ''); assert.equal(extractDelta('openai_chat', null).text, '');
});
test('chatBody clamps max_tokens to 4096 and always streams; servedBy reads both headers', () => {
  assert.equal(chatBody('openai_chat', { model: 'm', messages: [], maxTokens: 999999 }).max_tokens, 4096); assert.equal(chatBody('openai_chat', { model: 'm', messages: [], maxTokens: 0 }).max_tokens, 256); assert.equal(chatBody('openai_chat', { model: 'm', messages: [] }).stream, true);
  assert.deepEqual(servedBy(new Headers({ 'X-Served-By-Route': 'auto-local', 'X-Served-By-Tier': 'capable' })), { route: 'auto-local', tier: 'capable' }); assert.deepEqual(servedBy(new Headers()), { route: null, tier: null });
  assert.equal(clampMaxTokens('5000'), 4096); assert.equal(clampMaxTokens('abc', 100), 100); assert.equal(clampMaxTokens('-5'), 1);
});

test('auditLabel derives the action from the path (rows have no action field)', () => {
  assert.equal(auditLabel({ method: 'POST', path: '/api/v1/systems/dynamo/actions/up' }), 'dynamo: action up');
  assert.equal(auditLabel({ method: 'POST', path: '/api/v1/systems/switchyard/config/routes/apply' }), 'switchyard: config apply routes');
  assert.equal(auditLabel({ method: 'PUT', path: '/api/v1/systems/dynamo/params' }), 'dynamo: params apply');
  assert.equal(auditLabel({ method: 'PUT', path: '/api/v1/settings' }), 'settings update');
  assert.equal(auditLabel({ method: 'POST', path: '/api/v1/weird' }), 'POST /weird'); assert.equal(auditLabel({}), '–');
});

test('normalizeRole covers admin, viewer, public, and the absent/unknown fallback (admin; server enforces)', () => {
  assert.equal(normalizeRole('admin'), 'admin'); assert.equal(normalizeRole('viewer'), 'viewer'); assert.equal(normalizeRole('public'), 'public');
  assert.equal(normalizeRole(undefined), 'admin'); assert.equal(normalizeRole('root'), 'admin');
});

test('attestation falls back to the value in the summary line when there is no component', () => {
  assert.deepEqual(attestation({ components: [], summary: 'gateway up · attestation=signature_required' }), { state: 'reported', detail: '', value: 'signature_required' });
});
