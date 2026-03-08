## Problem
Seguro's RestartPolicy only restarts QEMU (the whole VM). Gastown distinguishes
"restart agent session" (fast, preserves filesystem) from "restart VM" (slow,
nuclear). Restarting QEMU takes 3-5s boot + SSH wait. Restarting just the agent
process inside the VM would be near-instant.

## What's needed
1. `Sandbox::restart_agent(command: &[str])` — SSH kill the agent process, then
   re-exec the command in the same VM session
2. Requires tracking the agent's PID inside the guest (write to `.seguro/agent.pid`
   during preamble, or find by process name)
3. The VM stays running — overlay, virtiofs, proxy all untouched
4. New `SessionEvent::AgentRestarted { session_id }` event
5. Use case: agent is stuck/hung but VM is healthy → restart agent, not VM

## Why this matters for Ox
Gastown's intervention ladder: nudge → restart session → nuke. The "restart
session" step is fast and preserves all filesystem state. Without this, Ox
has to choose between "do nothing" and "restart the entire VM" — too coarse.
Maps to the redirect/pause steps in Ox's escalation: nudge → redirect → pause
→ terminate → force kill.
