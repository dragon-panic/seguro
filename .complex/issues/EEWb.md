## Problem

Two competing failure modes:

1. **No pruning** → dead session overlays accumulate in `/run/user/$UID` (tmpfs),
   filling the partition. 18 dead sessions = 3.2GB full = all new VMs fail with
   ENOSPC.

2. **Frequent `seguro sessions prune --force`** → races with VMs still booting,
   kills live sessions before they stabilize.

Observed in Ox: disabled per-tick prune to stop killing live VMs, then hit
disk-full after a few workflow retries.

## Root cause

`sessions prune` doesn't distinguish "QEMU process is dead" from "QEMU process
is still starting." `--force` is too aggressive. Also, SIGINT (Ctrl-C) is not
handled, so cleanup never runs on the most common exit path.

## Approved fix

### A. Better in-process cleanup (`run.rs`)
1. Add SIGINT to signal handlers (alongside SIGTERM/SIGHUP)
2. Write `seguro.pid` (parent process PID) to session dir alongside `qemu.pid`

### B. Make prune safe to call frequently (`sessions.rs`, `session/image.rs`)
1. New classification axis: **orphaned** = `seguro.pid` is dead/missing (managing process gone)
2. Stop auto-reaping Zombie sessions — a booting VM looks like a zombie
3. Only reap zombies when the seguro parent PID is dead (orphaned)
4. Add `--min-age <secs>` flag to skip sessions younger than N seconds

### C. Prune behavior (default, no `--force`)
- **Dead** (QEMU gone): reap (with git-dirty check, same as today)
- **Orphaned** (seguro PID gone, QEMU may still run): kill QEMU + reap
- **Zombie but seguro alive**: skip (probably still booting)
- **Alive**: skip (same as today)
- `--force`: kill everything (same as today)

### Acceptance criteria
- `seguro sessions prune` never kills a session whose managing seguro process is alive
- Dead sessions are reaped promptly (safe to call every 10s)
- SIGINT triggers cleanup (sandbox.kill())
- Running 50+ short-lived sessions doesn't fill tmpfs
