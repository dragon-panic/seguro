## Goal
A manager process needs to start/monitor/stop sessions without shelling out
to the CLI. Expose a library interface or long-lived daemon.

## Options
- Refactor core logic into a `seguro-lib` crate with a clean async API
- Add a daemon mode (`seguro daemon`) with a Unix socket API
- gRPC or JSON-RPC for cross-language manager support

## Key operations
- create_session(config) -> session_id
- session_status(id) -> running/finished/failed
- kill_session(id)
- list_sessions()

## Acceptance
- Manager can create and manage sessions without CLI subprocess
