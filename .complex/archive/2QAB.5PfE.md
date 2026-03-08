## Problem
`Sandbox::kill()` sends SIGTERM to QEMU immediately. The agent gets no warning.

## Design decision: virtiofs sentinel, not SSH
Original spec proposed SSHing in to signal the agent. That's inconsistent with
the virtiofs-first approach used for agent_state/inject. Instead:

1. Write `.seguro/shutdown` sentinel file to virtiofs workspace
2. Wait configurable grace period (default 5s)
3. Kill QEMU

The agent can watch for the sentinel (inotify or poll) and save state.
No SSH round-trip, no new events (Ox called kill(), it knows).

## Implementation
1. `SandboxConfig::shutdown_grace: Option<Duration>` — default `Some(5s)`, `None` for immediate kill
2. `kill()` writes `{workspace}/.seguro/shutdown` before waiting
3. During grace period, poll QEMU every 250ms — if it exits cleanly, skip the wait
4. After grace period, kill QEMU as before

## What this does NOT include
- No SSH signaling
- No `SessionEvent::ShuttingDown` event
- No guest-side hook implementation
