# PRD: Provider-Aware API Usage Capture

## Problem

Seguro's MITM proxy already intercepts and logs HTTPS traffic metadata (host,
path, status, bytes) per session. But orchestrators like Ox have no visibility
into **AI API token usage** — they can see request counts and byte sizes but not
the actual token consumption, model selection, or request latency that drive
cost and performance decisions.

The proxy is the natural capture point: it already decrypts traffic when
`--tls-inspect` is on, and each proxy instance maps 1:1 to a session. No new
infrastructure is needed — just smarter logging for recognised AI API hosts.

## Design

### Provider detection by hostname

The proxy identifies AI providers by destination hostname. A static map ships
with Seguro; orchestrators can extend it via config.

Default provider map:

| Hostname pattern | Provider ID |
|---|---|
| `api.anthropic.com` | `anthropic` |
| `api.openai.com` | `openai` |
| `generativelanguage.googleapis.com` | `google` |
| `api.mistral.ai` | `mistral` |

Config extension point in `seguro.toml` / `.seguro.toml`:

```toml
[proxy.ai_providers]
# provider_id = ["hostname1", "hostname2"]
anthropic = ["api.anthropic.com"]
openai    = ["api.openai.com"]
custom    = ["my-llm-gateway.internal"]
```

When the proxy sees a request to a recognised hostname and TLS inspection is
active, it activates the usage extraction path.

### Token extraction in `forward_inspected`

Today `forward_inspected` (proxy/mod.rs:408) streams the response body straight
through without inspecting it. For AI API hosts, we buffer the response body,
extract the `usage` object, then forward the original bytes.

The extraction is provider-specific:

**Anthropic** (`api.anthropic.com`, POST to `/v1/messages`):
```json
{
  "model": "claude-sonnet-4-20250514",
  "usage": {
    "input_tokens": 1520,
    "output_tokens": 380,
    "cache_creation_input_tokens": 0,
    "cache_read_input_tokens": 1000
  }
}
```

**OpenAI** (`api.openai.com`, POST to `/v1/chat/completions`):
```json
{
  "model": "gpt-4o",
  "usage": {
    "prompt_tokens": 1520,
    "completion_tokens": 380,
    "total_tokens": 1900
  }
}
```

**Streaming responses**: Both Anthropic and OpenAI send token usage in the final
SSE event (`message_stop` for Anthropic, `[DONE]` preceded by a usage chunk for
OpenAI). The proxy must accumulate SSE events and extract usage from the final
message. For streaming, the proxy tees the SSE stream — forwarding each chunk
immediately to the client while accumulating a small buffer that tracks only the
final usage event.

### New log record: `api-usage.jsonl`

A separate per-session JSONL file alongside `proxy.jsonl`:

```
{session_dir}/api-usage.jsonl
```

Each record:

```json
{
  "ts": "2026-03-25T14:32:12Z",
  "provider": "anthropic",
  "model": "claude-sonnet-4-20250514",
  "endpoint": "/v1/messages",
  "latency_ms": 2340,
  "request_bytes": 12400,
  "response_bytes": 3200,
  "input_tokens": 1520,
  "output_tokens": 380,
  "cache_read_tokens": 1000,
  "cache_creation_tokens": 0,
  "status": 200,
  "streaming": true
}
```

Fields are nullable — if extraction fails (malformed response, unknown provider
format), the record is still written with what's available (at minimum: ts,
provider, endpoint, status, latency_ms, request/response bytes).

### Updated `ProxyStats`

Add atomic counters for API usage alongside existing traffic counters:

```rust
pub struct ProxyStats {
    // existing
    pub requests: AtomicU64,
    pub blocked: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub bytes_received: AtomicU64,
    // new
    pub ai_requests: AtomicU64,
    pub ai_input_tokens: AtomicU64,
    pub ai_output_tokens: AtomicU64,
    pub ai_cache_read_tokens: AtomicU64,
}
```

These feed into `SessionUsage` so orchestrators get aggregate token counts
without parsing JSONL.

### Updated `SessionUsage`

```rust
pub struct SessionUsage {
    // existing
    pub wall_clock: Duration,
    pub proxy_requests: u64,
    pub proxy_blocked: u64,
    pub proxy_bytes_received: u64,
    // new
    pub ai_requests: u64,
    pub ai_input_tokens: u64,
    pub ai_output_tokens: u64,
    pub ai_cache_read_tokens: u64,
}
```

### CLI: `seguro api-usage [SESSION_ID]`

New subcommand (mirrors existing `proxy-log`) that formats `api-usage.jsonl`
as a table:

```
TIME         PROVIDER    MODEL                   IN_TOK  OUT_TOK  CACHE   LATENCY  STATUS
14:32:12     anthropic   claude-sonnet-4-20250514    1520      380   1000    2340ms     200
14:32:18     openai      gpt-4o                   820      220      0    1100ms     200
─────────────────────────────────────────────────────────────────────────────────
TOTAL                                            2340      600   1000
```

### Constraint: TLS inspection required

This feature only works when `--tls-inspect` is active. Without it, the proxy
does blind TCP forwarding and cannot see response bodies. When TLS inspection is
off, `api-usage.jsonl` is not created and `SessionUsage` AI fields are zero.

This is acceptable: any orchestrator that cares about token costs will already
be using `--tls-inspect` for URL-level logging.

## Implementation scope

### New files

- `src/proxy/ai_usage.rs` — Provider map, response body parser, `ApiUsageRecord`
  struct, `ApiUsageLog` writer (parallel to `log.rs`)

### Modified files

- `src/proxy/mod.rs` — In `forward_inspected`: detect AI provider host, buffer
  response body, call extraction, write usage record. Add `request_bytes`
  tracking (read `Content-Length` or count body bytes).
- `src/proxy/log.rs` — Add `request_bytes` and `latency_ms` to `RequestRecord`
  (benefits all requests, not just AI).
- `src/api.rs` — Extend `SessionUsage` with AI token fields, read from
  `ProxyStats`.
- `src/cli.rs` — Add `api-usage` subcommand.
- `src/config.rs` — Add `proxy.ai_providers` config section.

### Not in scope

- Mapping sessions to orchestrator-level concepts (workers, tasks, objectives) —
  that's the orchestrator's job. Seguro provides per-session data.
- Request body content capture — privacy boundary. Only metadata + token counts.
- Response message content — never stored. Only the `usage` and `model` fields
  are extracted.

## Risks

1. **Buffering large responses** — AI API responses can be large (long
   completions). Mitigation: only buffer responses from recognised AI hosts, and
   for streaming responses, only accumulate the final usage event, not the full
   body.

2. **Streaming SSE parsing complexity** — SSE format varies between providers.
   Mitigation: start with non-streaming extraction (simpler), add streaming
   support as a follow-up. Non-streaming covers tool-use-heavy workloads where
   streaming is often off.

3. **Provider format changes** — API response schemas evolve. Mitigation:
   extraction is best-effort; failed parsing writes a record with null token
   fields rather than breaking the proxy.

## Acceptance criteria

- [ ] Proxy detects AI API hosts from default provider map
- [ ] Provider map is configurable via `proxy.ai_providers` in config
- [ ] Token usage extracted from non-streaming Anthropic responses
- [ ] Token usage extracted from non-streaming OpenAI responses
- [ ] `api-usage.jsonl` written per session with all specified fields
- [ ] `request_bytes` and `latency_ms` added to base `RequestRecord`
- [ ] `SessionUsage` includes aggregate AI token counters
- [ ] `seguro api-usage` CLI command formats usage log
- [ ] Streaming SSE extraction works for Anthropic and OpenAI
- [ ] No response content is stored — only metadata and token counts
- [ ] Feature is no-op when `--tls-inspect` is off
- [ ] Extraction failures are logged and produce partial records, not errors
