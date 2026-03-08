## Problem
Ox spawns a manager VM + N worker VMs concurrently. Need to manage the pool:
track active sessions, enforce concurrency limits, clean up on exit.

## Design
- SessionPool struct: manages multiple Sandbox instances
- Concurrency cap: max_sessions (from Ox budget "concurrent slots")
- Operations: spawn(config) → session_id, kill(session_id), list(), kill_all()
- Resource tracking: total RAM allocated, total CPUs, active count
- Clean shutdown: kill_all on Ox exit / Ctrl+C

## API surface
```rust
pub struct SessionPool {
    sessions: HashMap<String, Sandbox>,
    max_sessions: usize,
}

impl SessionPool {
    pub async fn spawn(&mut self, config: SandboxConfig) -> Result<String>;
    pub async fn exec(&self, id: &str, cmd: &[String]) -> Result<SessionResult>;
    pub async fn kill(&mut self, id: &str) -> Result<()>;
    pub fn list(&self) -> Vec<SessionInfo>;
    pub async fn kill_all(&mut self) -> Result<()>;
}
```

## Acceptance
- 5 concurrent sessions start and run without port/socket collisions
- kill_all cleans up all VMs and temp dirs
- spawn beyond max_sessions returns clear error
