QEMU and virtiofsd process management.

**virtiofsd.rs**: spawn virtiofsd with per-session socket path (/run/seguro/{id}/virtiofs.sock), pointing at the share directory. Watch for unexpected exit and surface error to user.

**fw_cfg.rs**: build the -fw_cfg argument string to inject the SSH authorized_key into the guest at boot: `name=opt/seguro/authorized_key,file={path}`

**mod.rs**: QemuBuilder — construct the full qemu-system-x86_64 argument list from a Session + Config:
- -M q35, -cpu host -enable-kvm (or -accel tcg with warning if /dev/kvm absent)
- -m and -smp from config (bumped for --browser)
- -drive (session overlay qcow2)
- -netdev user with hostfwd for SSH and guestfwd for proxy
- -chardev + -device vhost-user-fs-pci + -object memory-backend-file + -numa
- -fw_cfg for SSH key
- -nographic -serial stdio (or -serial null for non-interactive)

Startup checks: verify qemu-system-x86_64 ≥7.2, virtiofsd present, /dev/kvm accessible — all with actionable error messages.

Poll SSH port for readiness after QEMU starts (exponential backoff, 15s timeout).