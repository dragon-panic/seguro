## Problem

Currently `seguro run --share <path>` accepts a single source directory, mounted at `~/workspace` in the guest. Ox needs to mount two directories separately per VM:

- `/repo` — the target source code (changes per worker type)
- `/ox` — shared Ox operational state (same for all workers)

## Requested Change

Support multiple `--share` flags with explicit guest mount paths:

```
seguro run \
  --share /host/projects/myapp:/repo \
  --share /host/ox-state:/ox \
  -- claude
```

### What needs to change

- **CLI** (`src/cli.rs`): `share: Option<PathBuf>` → `share: Vec<String>` with `host:guest` parsing
- **virtiofsd** (`src/vm/virtiofsd.rs`): spawn one virtiofsd per mount, each with its own socket
- **QEMU args** (`src/vm/mod.rs`): add a `vhost-user-fs-pci` device per mount with distinct tags
- **Guest mount** (`src/commands/run.rs`): SSH preamble creates each guest mount point and runs `mount -t virtiofs <tag> <guest-path>`
- **Config merging** (`src/config.rs`): `.seguro.toml` discovery — decide which share dir to look in (first? each?)

### Backwards compatibility

Bare `--share /path` (no colon) should keep working, defaulting guest path to `~/workspace`.

## Filed by

ox (blocks ox tasks uurp and phHD)
