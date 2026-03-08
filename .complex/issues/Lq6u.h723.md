## Current state
base.qcow2 is ~893 MB. This is large for a minimal cloud image with git/python3/nodejs.

## Ideas
- Use `apt-get clean` and remove `/var/lib/apt/lists/*` after package install
- Remove docs, man pages, locale data in cloud-init runcmd
- Use `--minimize` or strip unnecessary kernel modules
- Consider Alpine or a smaller base instead of Ubuntu minimal
- Tune qemu-img convert compression settings
- Remove snap/snapd if present

## Acceptance
- base.qcow2 under 500 MB ideally, under 600 MB acceptable
