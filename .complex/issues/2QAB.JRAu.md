## Problem
Seguro tracks no resource usage per session. Gastown records costs on session stop.
Ox needs budget tracking (planning 10%, execution 50%, iteration 30%, reserve 10%)
but has no data to work with.

## What's needed
1. Per-session resource summary written to `runtime_dir/usage.json` on session end:
   ```json
   {
     "wall_clock_secs": 3600,
     "proxy_bytes_sent": 1048576,
     "proxy_bytes_received": 5242880,
     "proxy_requests": 142,
     "proxy_blocked": 3
   }
   ```
2. Proxy already logs per-request bytes — aggregate on session cleanup
3. Wall clock: `Started` timestamp to `kill()`/exit timestamp (trivial)
4. New `SessionEvent::UsageSummary { ... }` emitted just before cleanup
5. `SessionMeta` gains `started_at: DateTime` for duration calculation
6. Future: guest CPU seconds via `/proc/stat` sampling (not v1)

## Why this matters for Ox
Budget tranches need data. Even wall-clock + proxy bytes lets Ox estimate cost
and enforce limits. Without metering at the sandbox layer, Ox would have to
implement its own timing and log parsing.
