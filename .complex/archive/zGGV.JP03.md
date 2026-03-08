## Goal
Manager needs to know: did the agent finish? What changed? What's the result?

## Approach
- Write structured result (exit code, files changed, summary) to workspace/.seguro/result.json
- Stream agent stdout/stderr to a log file the manager can tail
- Emit events (started, progress, finished) via the session API

## Acceptance
- Manager can retrieve structured task result after session completes
