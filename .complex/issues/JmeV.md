## Fix 2: SSH liveness check in prune

### Problem
`prune` only removes session dirs where QEMU PID is dead. Zombie sessions
(QEMU alive, guest halted/unreachable) accumulate and fill /dev/shm + tmpfs.
Also gates on `session.qcow2` existing, which misses dirs without overlays.

### Approach
1. Remove the `overlay.exists()` gate — any session dir is a candidate
2. Add `is_guest_reachable(ssh_port, timeout)` — TCP connect + SSH banner check
3. Prune logic:
   - QEMU PID dead → remove dir
   - QEMU PID alive + guest unreachable → kill QEMU (SIGTERM, wait, SIGKILL), remove dir
   - QEMU PID alive + guest reachable → skip (unless --force)
4. `--force` kills even reachable sessions (no git-dirty check)

### Acceptance
- Zombie sessions (QEMU alive, guest dead) are killed and cleaned
- Live sessions are preserved by default
- `--force` cleans everything
- Dirs without session.qcow2 are still detected
