## Problem
The PRD specifies iptables rules injected at boot via rc.local to:
1. Route HTTP/HTTPS through the proxy (10.0.2.100:3128 via guestfwd)
2. Block non-proxy outbound TCP
3. Allow DNS only to SLIRP resolver

These rules are NOT present in the cloud-init user-data in build-image.sh.
Without them:
- air-gapped mode relies solely on the proxy denying requests, but the guest
  could bypass the proxy by connecting directly
- Non-HTTP/S traffic is not blocked

## Fix
Add iptables rules to the base image via cloud-init runcmd or a persistent
rc.local script that runs on every boot.

## Acceptance
- Guest iptables rules match PRD spec
- Direct curl bypassing proxy is blocked
- Non-HTTP/S TCP (e.g. nc) is dropped