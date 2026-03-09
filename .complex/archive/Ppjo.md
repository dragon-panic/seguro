## Seguro base image needs Rust toolchain

Workers need cargo/rustc to compile and test Rust code. Installing via
rustup at boot time is not viable — each install writes ~1GB to the
tmpfs-backed overlay at /run/user/1000/seguro/, which is only 3.2GB.
Three VMs installing simultaneously fill it completely, causing "No space
left on device" errors across all sessions.

The bootstrap script tried adding rustup to CLAUDE_SEED but this was
reverted after it bricked the entire ensemble.

### Fix

Bake Rust into the seguro base VM image. The toolchain is installed once
at image build time, not at every boot. This also saves ~2 minutes of
boot time per worker.

### Alternatives considered and rejected

- rustup in CLAUDE_SEED: fills tmpfs, 1GB per VM
- Mount host ~/.cargo read-only: breaks sandbox isolation
- --persistent overlay: still tmpfs, same size limit
- Increase tmpfs size: doesn't scale with more workers
