## Problem
Ox needs to know if an agent VM is healthy — not just "QEMU is running" but
"SSH is responsive and the guest OS is functional."

## Design
- Background task pings SSH periodically (configurable interval, default 30s)
- Health states: Healthy, Degraded (SSH slow), Unresponsive, Dead
- Callback/channel when state changes
- Integrates with crash detection: Unresponsive → wait → Dead → restart

## API surface
```rust
pub enum HealthState { Healthy, Degraded, Unresponsive, Dead }

impl Sandbox {
    pub fn health(&self) -> HealthState;
    pub fn on_health_change(&self, callback: impl Fn(HealthState));
}
```

## Acceptance
- Healthy VM reports Healthy
- VM with high load reports Degraded (SSH responds slow)
- Killed sshd → Unresponsive within one interval
- Killed QEMU → Dead within 1s
