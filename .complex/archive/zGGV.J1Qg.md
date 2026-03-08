## Goal
Claude Code (or a configurable agent binary) should be available in the guest
without manual installation on every session boot.

## Options
- Bake Claude Code into the base image during build-image.sh
- Download on first boot via cloud-init runcmd
- Mount a host-side binary via virtiofs (read-only tools share)

## Acceptance
- `seguro run -- claude --version` works out of the box
