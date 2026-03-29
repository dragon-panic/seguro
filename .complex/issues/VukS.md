## `seguro sessions prune` doesn't remove dirs with dead QEMU PIDs

### Problem

After many Ox bootstrap cycles, `/run/user/1000/seguro/` accumulates hundreds
of stale session directories. Each has a `qemu.pid` file pointing at a
long-dead process, but `seguro sessions prune --force` reports "Nothing to prune."

Observed: 462 dirs on disk, only 2 with running QEMU processes. Prune removes 0.

### Expected behavior

`prune` should remove any session directory where:
- `qemu.pid` doesn't exist, OR
- `qemu.pid` points to a process that isn't running (`kill -0` fails)

### Reproduction

```
# After running Ox bootstrap stop/start a few times:
ls /run/user/1000/seguro/ | wc -l   # → 462
seguro sessions ls                    # → 2 live sessions
seguro sessions prune --force         # → "Nothing to prune."
```
