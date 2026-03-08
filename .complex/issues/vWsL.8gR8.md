## Problem
Ox needs real-time visibility into session lifecycle: started, SSH ready,
exec began, health changed, crashed, restarted, completed. Currently
Sandbox::start/exec/kill are blocking calls with no intermediate events.

## Design
- Event enum: Started, SshReady, ExecStarted, HealthChanged, Crashed, Restarted, Completed
- Channel-based: Sandbox emits events to a tokio::broadcast channel
- Ox subscribes and routes events to its SSE stream for the UI
- Each event carries session_id + timestamp + payload

## API surface
```rust
pub enum SessionEvent {
    Started { session_id: String },
    SshReady { session_id: String, port: u16 },
    ExecStarted { session_id: String, command: Vec<String> },
    HealthChanged { session_id: String, state: HealthState },
    Crashed { session_id: String, reason: String },
    Restarted { session_id: String, attempt: u32 },
    Completed { session_id: String, result: SessionResult },
}

impl Sandbox {
    pub fn events(&self) -> broadcast::Receiver<SessionEvent>;
}
```

## Acceptance
- All lifecycle transitions emit events
- Multiple subscribers receive all events
- Events include timestamps and session IDs
- Ox can map these to its own SSE stream
