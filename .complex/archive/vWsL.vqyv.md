## Approved Plan

### 1. Extend `OutputMode` (api.rs)
- Add `Capture` variant — collects full stdout/stderr into `Vec<u8>`
- Add `Stream(mpsc::Sender<OutputChunk>)` — forwards chunks in real-time
- `OutputMode` loses `Copy` (Stream contains a Sender)

### 2. Add `OutputChunk` (api.rs)
```rust
pub enum OutputChunk {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
}
```

### 3. Extend `SessionResult`
- `stdout: Option<Vec<u8>>` — populated in Capture mode
- `stderr: Option<Vec<u8>>` — populated in Capture mode

### 4. Change `exec()` internals
- Capture/Stream: use `cmd.spawn()` + read handles instead of `cmd.status()`
- Skip PTY (`-t`/`-tt`) when Capture/Stream — PTY merges streams
- For Capture: `tokio::io::AsyncReadExt::read_to_end` on both handles
- For Stream: spawn two tasks forwarding chunks via sender
- Timeout wraps the whole child.wait() + read logic

### 5. Sandbox struct
- `stdout`/`stderr` fields change from `OutputMode` to... hmm, `Stream` has a Sender
  which is per-exec, not per-sandbox. Better: make `exec()` accept an optional
  `ExecOptions` or keep stdout/stderr on SandboxConfig but clone the Sender.

  Actually: `OutputMode` on SandboxConfig sets the default. But `Stream` requires
  a fresh Sender per exec call. Solution: `exec()` uses config defaults, add
  `exec_with(command, stdout, stderr)` for per-call override.

### 6. No CLI changes
run.rs keeps using Inherit/Null. New modes are API-only for Ox.
