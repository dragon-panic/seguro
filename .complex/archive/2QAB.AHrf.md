## Problem
Seguro has no way to send a message to a running agent inside a VM. Ox needs
non-destructive message delivery for active supervision nudges.

## Design decision: filesystem primitive only
virtiofs is bidirectional — host can write directly to the shared workspace.
No SSH round-trip needed. Turn-boundary delivery is the agent's responsibility
(e.g., Claude Code UserPromptSubmit hook). Seguro provides the write primitive;
Ox decides message content and timing.

## Implementation
1. `Sandbox::inject(message: &str) -> Result<PathBuf>`:
   - Creates `{workspace}/.seguro/inbox/` if it doesn't exist
   - Writes message to `{workspace}/.seguro/inbox/{timestamp_nanos}.md`
   - Returns the path of the written file
   - Atomic write (temp file + rename) to avoid partial reads
2. `Sandbox::pending_messages() -> Result<usize>`:
   - Counts `.md` files in `{workspace}/.seguro/inbox/`
   - Returns 0 if directory doesn't exist

## What this does NOT include
- No guest-side hook implementation (agent-specific)
- No message format beyond plain text/markdown files
- No read/ack tracking — agent deletes or moves files after reading
