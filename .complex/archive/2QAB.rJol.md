## Problem
Seguro's health monitor only checks SSH connectivity (healthy/degraded/unresponsive/dead).
It has no visibility into what the agent is actually doing. Ox needs to know
if a worker is stuck vs working vs idle — SSH liveness alone can't distinguish these.

## Design decision: primitives only, no watcher
The original spec proposed a background watcher that polls `status.json` and emits
`SessionEvent::AgentStateChanged` events with staleness detection. **We're not doing that.**

Seguro is a sandbox, not a supervisor. Ox already runs its own supervision loop
(every 2-3 min). Pushing polling and staleness policy into seguro would:
- Duplicate Ox's responsibility
- Force seguro to have opinions about polling frequency and staleness thresholds
  that only the orchestrator has context for
- Add background tasks for something the consumer already does

Seguro provides the **read primitive**. Ox owns the polling loop and the policy.

## Implementation
1. `AgentState` struct with serde:
   ```rust
   pub struct AgentState {
       pub state: String,         // "working", "idle", "stuck", "exiting" — not an enum, agent-defined
       pub updated_at: DateTime<Utc>,
       pub task: Option<String>,
       pub progress: Option<f64>,
   }
   ```
2. Store `workspace: PathBuf` on the `Sandbox` struct (currently only in SessionMeta on disk)
3. `Sandbox::agent_state() -> Result<Option<AgentState>>`:
   - Reads `{workspace}/.seguro/status.json`
   - Returns `Ok(None)` if file doesn't exist or is malformed (partial write)
   - No SSH, no background task — synchronous filesystem read
4. Convention: `.seguro/status.json` on virtiofs. Agent writes atomically
   (temp file + rename). Guest-side documentation is agent-specific, not seguro's job.

## What this does NOT include
- No background watcher / poller
- No `SessionEvent::AgentStateChanged` event
- No staleness detection or `AgentStateStale` event
- No `SandboxConfig::watch_agent_state` field
- No new `HealthState` variants
