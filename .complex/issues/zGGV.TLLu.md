## Goal
Manager spawns multiple agents working on different tasks simultaneously.

## Notes
- Port allocation already handles concurrent sessions
- Need to verify resource isolation (memory, CPU) under load
- Manager needs to track multiple session lifecycles
- Consider per-session resource caps (cgroups or QEMU limits)

## Acceptance
- 3+ agents running concurrently without interference
- Manager can track all sessions independently
