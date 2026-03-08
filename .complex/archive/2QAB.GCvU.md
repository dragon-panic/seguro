## Problem
Restarting QEMU takes 3-5s boot + SSH wait. Sometimes only the agent process
needs restarting, not the VM.

## Design decision: kill_agent(), not restart_agent()
Original spec proposed `restart_agent(command)` that kills + re-execs. But
re-exec is Ox's job — it calls exec() again. Seguro provides the kill primitive.

## Implementation
1. `Sandbox::kill_agent() -> Result<()>`:
   - SSHes into the guest (suppressed output)
   - Runs `pkill -u agent -x -f` to kill all agent-user processes
   - Returns once the kill command completes
   - VM stays running — overlay, virtiofs, proxy all untouched
2. Ox's flow: call `kill_agent()`, then call `exec()` with new command

## What this does NOT include
- No PID tracking in `.seguro/agent.pid`
- No `SessionEvent::AgentRestarted` event
- No re-exec logic — Ox handles that
