## Approved approach

Extract a `src/api.rs` module exposing `SessionConfig` and `SessionHandle` as
the programmatic interface. The CLI `run` command becomes a thin wrapper.

### Key types

- `SessionConfig` — all inputs needed to start a sandbox (workspace, command,
  net mode, env vars, memory, smp, tls_inspect, persistent). Concrete values,
  no Options. Implements Default for ergonomic construction.
- `SessionHandle` — owns the running VM lifecycle (QEMU child, virtiofsd,
  proxy, session state). Methods: `id()`, `exec()`, `kill()`, `wait()`.

### What stays in CLI only
- Terminal save/restore (raw mode fix)
- Ctrl+C signal handling
- println! status messages
- Reading env vars from host environment
- --browser flag (just sets memory/smp defaults)
- --unsafe-dev-bridge validation

### Migration
- `commands/run.rs` calls `SessionHandle::start(config)` then `handle.exec()`
- Internal helpers (iptables_preamble, shell_quote, inject_workspace_config)
  move into api.rs as private functions