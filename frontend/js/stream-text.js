// Extract the incremental reply text and usage from one parsed playground SSE event, for both wire formats.
/** @returns {{text:string, usage?:object, done?:boolean}} */
export function extractDelta(wire, ev) {
  const d = ev?.data;
  if (d == null || typeof d !== 'object') return { text: '' };
  if (wire === 'anthropic_messages') {
    if (d.type === 'content_block_delta' && d.delta?.type === 'text_delta') return { text: String(d.delta.text ?? '') };
    if (d.type === 'message_delta') return { text: '', usage: d.usage ? { completion_tokens: d.usage.output_tokens } : undefined };
    if (d.type === 'message_start' && d.message?.usage) return { text: '', usage: { prompt_tokens: d.message.usage.input_tokens } };
    if (d.type === 'message_stop') return { text: '', done: true };
    return { text: '' };
  }
  const choice = d.choices?.[0];
  const text = choice?.delta?.content ?? '';
  return { text: typeof text === 'string' ? text : '', usage: d.usage || undefined, done: choice?.finish_reason ? true : undefined };
}
/** Build the request body. max_tokens is clamped to the contract's 4096. */
export function chatBody(wire, { model, messages, maxTokens }) {
  return { model, messages, max_tokens: Math.max(1, Math.min(4096, Number(maxTokens) || 256)), stream: true };
}
/** Header names the backend may add on a playground response. */
export function servedBy(headers) {
  const get = (n) => headers?.get?.(n) || null;
  return { route: get('X-Served-By-Route'), tier: get('X-Served-By-Tier') };
}
