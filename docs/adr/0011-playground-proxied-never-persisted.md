# ADR-0011: Playground chat is proxied, streamed, never persisted; credential resolved server-side by env NAME

Status: Accepted (v0.1.0)

## Context

Operators want to send one prompt "through" Switchyard, Relay or Dynamo and see which route served it. That
requires the console to hold or use a credential and to handle prompt text.

## Decision

`POST /systems/{id}/playground/chat` (admin only) validates the body (roles, up to 200 messages, model id shape),
clamps `max_tokens` to 4096 (default 1024), resolves the credential from the **environment variable named** in
`playground_key_env` (`switchyard` defaults to `LITELLM_API_KEY`), adds it as a bearer server-side, and relays the
upstream response. Streaming responses are passed through chunk by chunk; the credential value is registered with
the redactor and substituted in both stream chunks and non-stream bodies. Errors from upstream are capped and
redacted (502 `upstream_error`); a missing variable is 503 `credential_missing` naming the variable (not the
value). The audit log stores `[n messages]`, never the text. `X-Served-By-Route` / `-Tier` response headers are
added when upstream sends one of a set of candidate header names.

## Consequences

- The key is never in the browser, database, audit log or logs; settings shows `[set, N chars]`. Credential names set
  through the Settings API must be `LITELLM_*`, `NEMO_CONSOLE_PLAYGROUND_*` or `*_API_KEY` (an admin cannot point the
  proxy at an arbitrary variable such as a cloud secret); names from the operator's config file or environment are
  unrestricted.
- Prompt and completion text transit the process in memory; nothing writes them. (They may be logged by the
  system you proxy to, and Relay is designed to record traces of them.)
- Stream redaction replaces the secret inside a single chunk only (general redaction is chunked at 4000 characters too); a secret split across two network chunks
  would not be caught. Upstreams are not expected to echo the bearer; this is a defence-in-depth gap.
- The served-by header names are guesses; real Switchyard responses did not carry them, so the badges are
  normally empty.
- Only `switchyard` has a default credential mapping. Relay's gateway answers 401 without a bearer, and Dynamo's
  frontend needs none. To use the Relay playground, add a `relay` entry (Settings, or
  `NEMO_CONSOLE_PLAYGROUND_KEY_ENV`). Note the contract text says the gateway takes no caller bearer; the adapter
  says it does. Not verified live either way.
- Verified live (maintainer): a chat request through a Switchyard route returned 200. The Relay and Dynamo playgrounds
  are not verified live. Requests carry `Accept-Encoding: identity` so the stream redaction sees plain bytes.

## Alternatives considered

- **Browser talks to the systems directly.** Rejected: the key would reach the browser, and CSP `connect-src
  'self'` forbids it anyway.
- **Store transcripts for replay.** Rejected: conversation content at rest.
- **Accept a key from the UI per request.** Rejected: keys in browser memory and request bodies.
