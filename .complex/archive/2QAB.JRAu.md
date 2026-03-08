## Problem
Seguro tracks no resource usage per session. Ox needs data for budget tracking.

## Design decision: live counters + proxy log aggregation
Original spec proposed writing `usage.json` on session end and emitting an event.
Instead: `Sandbox::usage()` reads live counters anytime. No event needed.

## Implementation
1. Add `ProxyStats` with atomic counters to `ProxyState`:
   - `requests: AtomicU64` — total proxy requests
   - `blocked: AtomicU64` — denied requests
   - `bytes_sent: AtomicU64` — request body bytes (estimated)
   - `bytes_received: AtomicU64` — response body bytes (estimated)
2. Increment counters in `log_request()` (already called for every request)
3. Share `ProxyStats` via `Arc` between `ProxyServer` and `Sandbox`
4. `SessionUsage` struct:
   ```rust
   pub struct SessionUsage {
       pub wall_clock: Duration,
       pub proxy_requests: u64,
       pub proxy_blocked: u64,
       pub proxy_bytes_sent: u64,
       pub proxy_bytes_received: u64,
   }
   ```
5. `Sandbox::usage() -> SessionUsage` reads atomic counters + wall clock
6. Store `started_at: Instant` on `Sandbox` struct for wall clock

## What this does NOT include
- No `usage.json` file written on kill
- No `SessionEvent::UsageSummary` event
- No guest CPU metering (future)
