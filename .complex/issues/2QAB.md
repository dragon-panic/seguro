## Context
Gastown (steveyegge/gastown) is a mature multi-agent orchestrator for Claude Code.
Research identified several concerns that gastown handles at the tmux session layer
which, in seguro's VM-based architecture, belong in the sandbox layer.

Seguro already has: health monitoring (SSH-based), session events, crash restart,
session recovery, persona injection, output streaming.

## Architectural decision: seguro provides primitives, not policy

Seguro is a sandbox, not a supervisor. The original specs (filed by an Ox agent)
pushed supervisor logic into the sandbox — background watchers, staleness detection,
event-driven polling. That duplicates Ox's job.

**Principle**: seguro exposes synchronous read/write primitives over virtiofs.
Ox owns the supervision loop, polling frequency, and interpretation of state.
No new background tasks or events for orchestrator features.

This means:
- `agent_state()` is a synchronous file read, not a watched stream
- `inject()` is a synchronous file write, not an event-emitting pipeline
- Staleness, supervision cadence, and escalation are Ox's domain

## What's missing (revised)
Six primitives, all synchronous filesystem operations:

1. **Graceful shutdown** — signal agent before killing VM
2. **Pre-kill inspection** — check workspace git state before termination
3. **Agent state reporting** — `Sandbox::agent_state()` reads `.seguro/status.json`
4. **Resource metering** — wall-clock, proxy bytes, request count per session
5. **Message injection** — `Sandbox::inject()` writes to `.seguro/inbox/`
6. **Agent restart** — restart agent process without restarting VM

## Priority
Agent state reporting (rJol) and message injection (AHrf) first — they enable
Ox's core supervision loop.
