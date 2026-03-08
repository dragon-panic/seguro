## Goal
Kill runaway agents. Cap resources per session.

## Approach
- --timeout flag: kill session after N minutes
- --max-memory: QEMU memory cap (already have --memory_mb in config)
- --max-cpu: SMP cap
- Watchdog thread that monitors session age and kills on timeout

## Acceptance
- Session auto-terminates after configured timeout
- Resource limits enforced at VM level
