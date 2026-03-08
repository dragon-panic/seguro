## Problem
Ox needs to capture agent output programmatically — not just exit codes but
actual stdout/stderr content for checkpoint processing and logging.

## Design
- OutputMode::Capture: collect stdout/stderr as Vec<u8>
- SessionResult gains stdout: Option<Vec<u8>>, stderr: Option<Vec<u8>>
- Streaming variant: callback/channel that receives chunks as they arrive
- Ox uses this for: parsing checkpoints, monitoring progress, logging

## API surface
```rust
pub enum OutputMode {
    Inherit,
    Null,
    Capture,                         // NEW
    Stream(mpsc::Sender<OutputChunk>), // NEW
}

pub struct SessionResult {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration: Duration,
    pub stdout: Option<Vec<u8>>,    // NEW (when Capture)
    pub stderr: Option<Vec<u8>>,    // NEW (when Capture)
}
```

## Acceptance
- OutputMode::Capture collects full stdout/stderr
- OutputMode::Stream sends chunks in real-time
- Existing Inherit/Null modes unchanged
