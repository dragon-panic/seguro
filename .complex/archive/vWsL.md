## Context

Ox (~/projects/ox) is an orchestration layer that turns Seguro sandboxes into a managed
AI workforce. See .ox/documents/plans/002-ox-executive-plan.html for the full plan.

Ox needs seguro to:
1. Run long-lived manager sessions (hours/days) with crash recovery
2. Spawn/kill many concurrent worker VMs on demand
3. Provide a solid programmatic API for Ox's ypmF session management component
4. Inject credentials, env vars, and persona configs into each VM

This work unblocks Ox Phase 2 (Infrastructure), specifically the ypmF Seguro session
management component.
