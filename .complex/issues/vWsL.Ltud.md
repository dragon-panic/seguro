## Problem
Manager agents run for hours/days. If QEMU crashes, segfaults, or gets OOM-killed,
the session is lost. Ox plan lists this as Risk #1.

## Design
- Monitor QEMU process (tokio::process::Child::wait in background task)
- On unexpected exit: log reason, preserve session state, restart QEMU with same overlay
- Configurable restart policy: always, on-failure, never (default: on-failure)
- Max restart count before giving up (default: 3)
- Backoff between restarts (1s, 5s, 15s)
- After restart: wait for SSH, re-run mount/env preamble, notify caller

## API surface
```rust
pub struct RestartPolicy {
    pub strategy: RestartStrategy,  // Always, OnFailure, Never
    pub max_restarts: u32,
    pub backoff: Vec<Duration>,
}
```

SandboxConfig gains `restart_policy: Option<RestartPolicy>`.

## Acceptance
- QEMU killed with SIGKILL → seguro detects within 1s, restarts, SSH ready again
- After max restarts exceeded → returns error, does not loop forever
- restart_policy: Never (default) → current behavior unchanged
