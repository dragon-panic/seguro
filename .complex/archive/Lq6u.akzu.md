## Current state
Env vars are written to workspace/.seguro/environment via inject_workspace_config().
They are also passed via fw_cfg (QEMU -fw_cfg opt/seguro/env/KEY=VALUE).

But there's no mechanism in the guest to actually READ these env vars and export them
into the agent's shell environment. The run_agent() SSH command doesn't source them.

## Fix
Either:
1. Source workspace/.seguro/environment in the SSH command before running the agent
2. Or read fw_cfg files from /sys/firmware/qemu_fw_cfg/ and export them

Option 1 is simpler since the workspace is already mounted.

## Acceptance
- ANTHROPIC_API_KEY set on host is available inside the guest agent session