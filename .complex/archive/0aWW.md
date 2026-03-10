## Problem

Workers inside VMs need to inject environment variables at launch time — specifically, PATH needs prepending so they pick up the latest `cx` binary from the Ox mount rather than a stale baked-in version.

## Current State

Seguro already passes through a hardcoded list of env vars from the host (ANTHROPIC_API_KEY, CLAUDE_CODE_MAX_MODEL). There is no `--env` CLI flag for arbitrary key=value injection.

See `src/commands/run.rs` lines 24-32: env vars are host-passthrough only.

## Requested Change

Add `--env KEY=VALUE` flag (repeatable) to `seguro run`:

```
seguro run --env "PATH=/ox/bin:/usr/local/bin:/usr/bin:/bin" -- claude
```

### What needs to change

- **CLI** (`src/cli.rs`): Add `env: Vec<String>` arg to `RunArgs`
- **run.rs**: Parse `KEY=VALUE` pairs, merge with existing passthrough env vars, inject into SSH preamble

This is a small change — the env injection machinery already exists.

## Filed by

ox (fixes ox task Ler8)
