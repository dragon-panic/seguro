Replace the Alpine Linux build-from-ISO approach with Ubuntu 24.04 Minimal official cloud image.

## Subtasks
1. Rewrite build-image.sh — download ubuntu-24.04-minimal-cloudimg-amd64.img, boot once to add agent user + packages (git curl wget python3 python3-pip nodejs npm iptables), compact
2. Update cidata.rs — add cloud-init users stanza to create agent user at runtime (Ubuntu minimal has no agent user pre-baked unlike Alpine build)
3. Update config.rs — replace apk_allow with apt_allow (package manager changed)
4. Update vm/mod.rs ssh timeout default — Ubuntu boots slower than Alpine virt ISO
5. Smoke-test: cargo run -- run -- whoami should print agent