## Task
Verify `seguro images build` (or `scripts/build-image.sh`) successfully produces
`~/.local/share/seguro/images/base.qcow2`.

This is a prerequisite for all demos — without a base image, nothing runs.

## Notes
- Script uses Ubuntu 24.04 minimal cloud image
- Needs: qemu-system-x86_64, qemu-img, mkfs.vfat, mcopy, wget/curl
- Cloud-init installs packages, creates agent user, then powers off

## Acceptance
- base.qcow2 exists and is a valid qcow2 image
- Guest boots, has agent user, has git/python3/nodejs installed