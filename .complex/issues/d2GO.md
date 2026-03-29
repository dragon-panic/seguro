## Shared sccache for pool VMs — eliminate cold cargo builds

### Problem

Every pool VM compiles from scratch. A full `cargo build -p ox-server` takes
5+ minutes in a 6GB VM. Workers and reviewers both build, so a single
code-task workflow burns 10+ minutes just on compilation. The cargo `target/`
cache is ephemeral — lost when the VM dies.

### Fix: shared sccache via virtio-fs

1. **Host**: create a persistent sccache storage directory (e.g. `.ox/cache/sccache/`)
2. **Bootstrap**: add a second virtio-fs mount sharing the cache into VMs
3. **VM image**: install sccache, configure `RUSTC_WRAPPER=sccache` and
   `SCCACHE_DIR` pointing at the shared mount
4. **Concurrency**: sccache handles concurrent reads/writes safely — multiple
   VMs can build simultaneously against the same cache

First build populates the cache. Subsequent builds (any VM) get cache hits
for unchanged crates. Incremental: only recompiles what actually changed.

### Also consider

- Bake `~/.cargo/registry` into the base VM image to skip crate downloads
- Both changes are complementary

### Acceptance criteria

- sccache installed in base Seguro image
- Shared cache directory mounted into pool VMs at bootstrap
- Second `cargo build` in a fresh VM shows sccache cache hits
- Build time for unchanged code drops to <30s (from 5+ min)
