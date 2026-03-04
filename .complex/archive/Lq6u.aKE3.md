Integration tests verifying the acceptance criteria from the PRD.

These tests require a real KVM-capable Linux host and will be marked #[ignore] by default, runnable with `cargo test -- --ignored`.

Tests to cover:
- cold start: seguro run exits within 15s on KVM
- file sharing: write a file in the guest workspace, verify it appears on the host  
- filesystem isolation: agent user cannot read /etc/shadow or files outside /mnt/workspace
- network isolation: guest cannot reach 192.168.0.0/16 or 10.0.2.2 (curl should fail/403)
- non-HTTP/S TCP blocked: nc -z 1.1.1.1 9999 fails from inside guest
- api-only mode: request to unlisted domain returns 403; api.anthropic.com passes
- proxy log: after a curl from inside the guest, the host log file contains the request
- ephemeral: --snapshot mode, write a file to guest root fs, restart, file is gone
- ssh key: ps aux on host shows no secret values; /run/seguro/{id}/ is cleaned up after exit
- concurrent sessions: two simultaneous sessions do not interfere
- dev-bridge without --unsafe-dev-bridge: exits with non-zero and clear error message