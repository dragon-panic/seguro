## Problem
After a crash+restart, the manager agent needs to resume where it left off.
cx state and checkpoint files persist on the workspace (virtiofs), but the
agent process itself is gone.

## Design
- Session metadata file: runtime_dir/session.json (ports, image, profile, env)
- On restart: reload session.json, re-allocate ports if needed, reattach overlay
- Ox handles agent-level recovery (re-exec claude with cx state)
- Seguro's job: VM is back, SSH works, workspace is mounted, env is injected

## Acceptance
- After crash+restart: workspace files intact (virtiofs survives QEMU restart)
- session.json contains enough info to fully reconstruct VM params
- Sandbox::recover(session_id) → reconnect to restarted VM
