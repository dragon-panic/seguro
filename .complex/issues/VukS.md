## Fix 1: Robust cleanup in `seguro run`

### Problem
`seguro run` has no signal handling and no `Drop` on `Sandbox`. When the parent
process kills `seguro run` (SIGTERM/SIGHUP), cleanup never runs, leaving QEMU
orphaned. The `?` on `exec()` also skips cleanup on SSH errors.

### Approach
1. Register SIGTERM + SIGHUP handlers via `tokio::signal::unix`
2. Use `tokio::select!` to race exec against signals — whichever fires first,
   always call `sandbox.kill()`
3. Replace `sandbox.kill().await?` with logging — don't let kill errors skip
   post-kill cleanup (terminal restore, temp workspace removal)

### Acceptance
- `seguro run -- bash -c 'echo done'` exits cleanly, no lingering QEMU
- Sending SIGTERM to `seguro run` kills QEMU and removes session dir
- SSH errors during exec don't leak QEMU
