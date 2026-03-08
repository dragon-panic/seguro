## Problem
`sessions prune` removes dead sessions without checking if the shared workspace
has uncommitted git changes. `Sandbox::kill()` doesn't check either. Gastown
runs `check-recovery` + `git-state` before any termination and refuses to nuke
polecats with unpushed work.

## What's needed
1. `Sandbox::workspace_state()` — SSH into guest (or check virtiofs directly)
   and return workspace git status: clean, has_uncommitted, has_unpushed, has_stash
2. `sessions prune` warns (and skips by default) sessions whose workspace is dirty
   - `--force` flag to override
3. Optional `SandboxConfig::pre_kill_check: bool` — when true, `kill()` checks
   workspace state first and returns an error if dirty (caller decides what to do)

## Why this matters for Ox
Ox's intervention protocol needs pre-kill verification. The sandbox layer should
provide the primitive so Ox doesn't have to SSH in manually. Maps directly to
gastown's "never nuke polecats with unpushed work" principle.
