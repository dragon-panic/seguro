## Decision
virtio-9p was a hack that never worked. Switching back to virtiofs+virtiofsd as the PRD specifies.

## Changes needed
1. Re-enable virtiofsd in run.rs — start it before QEMU, pass socket to QEMU
2. Replace virtio-9p QEMU flags with virtiofs flags (vhost-user-fs-pci + shared memory backend)
3. Update guest mount command from 9p to virtiofs (no sudo needed if fstab/cloud-init handles it)
4. Update cloud-init user-data in build-image.sh: mount -t virtiofs instead of 9p sudoers rule
5. Keep virtiofsd startup check in main.rs (it's correct after all)
6. Update demo/02 if needed

## Acceptance
- demo/02-file-sharing.sh passes end-to-end
- virtiofsd starts and stops cleanly with each session
- Host and guest can share files bidirectionally