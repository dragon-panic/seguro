## Goal
Manager creates a branch, shares the worktree with the agent, agent works,
manager reviews the diff.

## Approach
- Manager creates a git worktree for each agent session
- Agent works in the worktree (commits allowed)
- On completion, manager can diff, review, merge or discard
- Consider auto-creating a branch per session (seguro/session-<id>)

## Acceptance
- Agent's git changes are isolated to a branch
- Manager can review diff after session completes
