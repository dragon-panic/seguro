Implement all seguro subcommands.

**run.rs**: Main entry point. Orchestrates: load config → allocate ports → generate keys → start proxy task → start virtiofsd → create session image → launch QEMU → poll SSH → exec agent (or shell). Handle Ctrl+C for graceful shutdown. Enforce --unsafe-dev-bridge requirement for dev-bridge mode.

**shell.rs**: `seguro shell [SESSION_ID]` — SSH into a running session's guest. If SESSION_ID omitted and exactly one session is running, use it; otherwise error.

**sessions.rs**: 
- `seguro sessions ls` — print table of active sessions (id, share path, net mode, uptime, agent)
- `seguro sessions prune` — find session overlays in ~/.local/share/seguro/sessions/ with no live QEMU process and delete them along with their /run/seguro/{id}/ state

**images.rs**: 
- `seguro images ls` — list base images with size and build date
- `seguro images build [--browser]` — invoke the base image build script (see base-image task)

**snapshot.rs**: `seguro snapshot save NAME` / `seguro snapshot restore NAME` — thin wrappers around qemu-img snapshot.

**proxy_log.rs**: `seguro proxy log [SESSION_ID]` — tail /run/seguro/{id}/proxy.log in real time (like tail -f but formatted).