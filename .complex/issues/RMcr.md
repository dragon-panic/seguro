## Provider-aware API usage capture in MITM proxy

Enhance the proxy to recognise AI API providers by hostname (Anthropic, OpenAI,
Google, Mistral) and extract token usage metadata from decrypted responses when
`--tls-inspect` is active.

### What this adds

- **Provider detection**: static hostname → provider map, configurable via
  `proxy.ai_providers` in config
- **Token extraction**: parse `usage` object from AI API responses (both
  non-streaming and SSE streaming)
- **Per-session `api-usage.jsonl`**: structured log with provider, model,
  input/output/cache tokens, latency, request/response bytes
- **Aggregate counters**: `ai_requests`, `ai_input_tokens`, `ai_output_tokens`,
  `ai_cache_read_tokens` in `ProxyStats` and `SessionUsage`
- **CLI**: `seguro api-usage [SESSION_ID]` table view
- **Base logging improvements**: `request_bytes` and `latency_ms` added to all
  `RequestRecord` entries (not just AI)

### Design document

Full PRD with response format details, streaming handling, and risk analysis:

    docs/prd/api-usage-capture.md

### Key implementation points

- Hook point is `forward_inspected` in `proxy/mod.rs` — already has decrypted
  request/response
- New `src/proxy/ai_usage.rs` for provider map + per-provider response parsers
- For streaming: tee the SSE stream (forward immediately, buffer only final
  usage event)
- Best-effort extraction — failed parsing writes partial records, never breaks
  the proxy
- No-op when `--tls-inspect` is off
- No message content is ever stored — only `usage`, `model`, and size metadata

### Filed by

ox — needed for tBCE (API traffic capture + analytics)
