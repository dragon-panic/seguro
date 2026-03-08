## Completed fixes (2026-03-08)

### SSH PTY allocation
- `-tt` for interactive shells (empty command)
- `-t` when host stdout is a terminal (commands like `claude` that need a PTY)
- Without this, interactive sessions hang after "Guest is ready"

### Claude Code credential injection
- Host `~/.claude/.credentials.json` auto-copied to workspace `.seguro/`
- Guest preamble moves it to `~/.claude/` and deletes from workspace
- Enables Max subscription auth without manual login

### Proxy env vars break Claude Code
- Node.js / Claude Code hangs when `http_proxy`/`HTTPS_PROXY` are set
- `full-outbound` mode no longer sets proxy env vars
- `api-only` still forces proxy (enforcement point for allow-list)
- Proxy remains available at 10.0.2.100:3128 for tools that opt in

### Claude Code pre-installed in base image
- `npm install -g @anthropic-ai/claude-code` added to build-image.sh cloud-init
- `seguro run -- claude --version` works out of the box

### Validated
- `claude --version` → 2.1.71
- `claude -p "say hello"` → works with Max subscription
- Shell access works with proper PTY
- Network connectivity to api.anthropic.com confirmed
