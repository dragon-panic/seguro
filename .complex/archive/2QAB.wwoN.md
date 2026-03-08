## Problem
No way to check if a workspace has uncommitted/unpushed work before killing a session.

## Design decision: host-side git, no SSH, no policy
The workspace is shared via virtiofs — the host has direct filesystem access.
Run `git` on the host-side path. No SSH needed.

`pre_kill_check` in config is policy — Ox decides whether to inspect before
killing. Seguro provides the primitive. `sessions prune` gets a safety check
since it's a destructive CLI command aimed at humans.

## Implementation
1. `WorkspaceState` struct:
   ```rust
   pub struct WorkspaceState {
       pub is_git_repo: bool,
       pub has_uncommitted: bool,  // dirty working tree or staged changes
       pub has_unpushed: bool,     // local commits ahead of remote
       pub dirty_files: u32,       // count of modified/untracked files
   }
   ```
2. `Sandbox::workspace_state() -> Result<WorkspaceState>`:
   - Runs `git -C {workspace} status --porcelain` for uncommitted
   - Runs `git -C {workspace} log @{upstream}..HEAD --oneline` for unpushed
   - Returns struct, not an error — caller decides what to do
3. `sessions prune`: check workspace git state, warn and skip dirty sessions
   unless `--force` flag is passed

## What this does NOT include
- No `SandboxConfig::pre_kill_check` flag
- No automatic check in `kill()`
- No stash detection (uncommon, adds complexity)
