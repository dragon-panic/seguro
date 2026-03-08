## Problem
`Sandbox::kill()` sends SIGTERM to QEMU with a 500ms grace period. The agent
process inside the guest gets no warning — it can't save work, commit, or
clean up. Gastown sends `LIFECYCLE:Shutdown` protocol messages before killing
tmux sessions.

## What's needed
1. Before killing QEMU, SSH into guest and signal the agent process (SIGTERM or
   write a sentinel file to `.seguro/shutdown` on virtiofs)
2. Configurable grace period (default 5s) for agent to save state
3. If agent doesn't exit within grace period, proceed with QEMU kill
4. New `SessionEvent::ShuttingDown` emitted before the grace period starts
5. `SandboxConfig` gains `shutdown_grace: Option<Duration>` (default 5s)

## Why this matters for Ox
Manager needs to cleanly shut down workers. Workers may have uncommitted changes,
in-progress cx updates, or partial output. A graceful signal lets the worker
persist state before the VM disappears.
