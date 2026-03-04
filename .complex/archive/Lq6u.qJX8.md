Session lifecycle, port allocation, key generation, and disk image management.

**ports.rs**: allocate a free TCP port by binding to 0, capturing the assigned port, releasing the socket. Needs at least 2 ports per session (SSH, proxy).

**keys.rs**: generate an ephemeral ed25519 keypair using ed25519-dalek; serialize public key to OpenSSH authorized_keys format and private key to OpenSSH PEM format using ssh-key crate. No shelling out to ssh-keygen.

**image.rs**:
- Locate base.qcow2 (from config or `~/.local/share/seguro/images/`)
- `qemu-img create -f qcow2 -b base.qcow2 -F qcow2 session-{uuid}.qcow2` for persistent sessions
- List and prune orphaned session overlays (sessions with no running QEMU PID)
- Named snapshot save/restore via `qemu-img snapshot`

**mod.rs**: Session struct holding all runtime state (id, paths, ports, child PIDs). Cleanup method kills all children and removes /run/seguro/{id}/.