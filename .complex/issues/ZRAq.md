## Problem

When mounting the source repo, workers should write only to their worktree branch, never to the shared source tree. A read-only flag would enforce this at the hypervisor level rather than relying on agent discipline.

## Requested Change

Extend the `--share` syntax to support a read-only flag:

```
seguro run \
  --share /host/source:/repo:ro \
  --share /host/ox-state:/ox \
  -- claude
```

### What needs to change

- **Parsing**: Extend `host:guest` to `host:guest:ro` (optional third segment)
- **virtiofsd**: Pass `--sandbox none` or use a read-only bind mount for the shared dir
- **QEMU**: virtiofs already supports read-only — may need `readonly=on` in device args

## Priority

Nice-to-have / optional. Depends on Issue 1 (multiple mount points) being implemented first.

## Filed by

ox
