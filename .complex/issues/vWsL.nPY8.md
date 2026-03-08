## Problem
Ox personas define system prompts, constraints, and tool access for each agent.
The guest VM needs to receive this config so claude runs with the right identity.

## Design
- Persona config injected via workspace .seguro/persona.toml
- Contains: system_prompt, role, constraints, tool_access, budget_cap
- Guest preamble reads persona config and passes to claude via CLAUDE.md or env
- SandboxConfig gains persona_config: Option<PathBuf> (path to persona TOML)

## How claude receives the persona
- Write persona system prompt to workspace/.claude/CLAUDE.md
- claude picks it up automatically as project instructions
- Additional constraints passed as env vars

## Acceptance
- Persona TOML injected into guest, claude reads it
- Different personas produce different agent behavior
- Persona config never persists in guest overlay (ephemeral)
