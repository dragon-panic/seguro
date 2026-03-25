# Seguro

Sandbox CLI coding agents inside a QEMU virtual machine. Seguro launches an
Ubuntu 24.04 guest with controlled filesystem and network access, so agents
like Claude Code can operate without touching host credentials, SSH keys, or
your LAN.

## Host requirements

- **Linux** (x86_64) with KVM support (recommended) — works without KVM via
  TCG software emulation but ~5-10x slower
- **Rust toolchain** (stable) — to build seguro itself
- **QEMU >= 7.2** (`qemu-system-x86_64`, `qemu-img`)
- **virtiofsd** — checked at startup
- **dosfstools** (`mkfs.vfat`) — for building the cloud-init seed disk
- **mtools** (`mcopy`) — for building the cloud-init seed disk
- **wget** or **curl** — for downloading the Ubuntu cloud image
- **ssh** — for connecting to the guest

### Install dependencies

**Arch Linux:**

```sh
sudo pacman -S qemu-full virtiofsd dosfstools mtools openssh
```

**Debian / Ubuntu:**

```sh
sudo apt install qemu-system-x86 qemu-utils virtiofsd dosfstools mtools openssh-client
```

### KVM access

Make sure your user can access `/dev/kvm`:

```sh
sudo usermod -aG kvm $USER
# Log out and back in for group change to take effect
```

## Build

```sh
cargo build --release
```

The binary is at `target/release/seguro`.

## Setup

### 1. Build the base VM image

This downloads an Ubuntu 24.04 minimal cloud image, boots it in QEMU to
install packages (git, curl, python3, nodejs, etc.), then compacts the result.

```sh
./scripts/build-image.sh
```

Output: `~/.local/share/seguro/images/base.qcow2` (~500 MB)

For browser support (includes Chromium, ~900 MB):

```sh
./scripts/build-image.sh --browser
```

### 2. Run an agent

```sh
# Interactive shell in the VM
seguro run

# Share a project directory with the VM
seguro run --share ./myproject

# Run a specific agent command
seguro run --share ./myproject -- claude

# With browser support (4 GB RAM, 4 vCPUs)
seguro run --browser --share ./myproject -- claude
```

## CLI commands

```
seguro run [OPTIONS] [-- AGENT...]     Run an agent in a sandboxed VM
seguro shell [SESSION_ID]              Open a shell in a running session
seguro sessions ls                     List active and saved sessions
seguro sessions prune [--force]        Remove orphaned session state
seguro snapshot save NAME              Save session state as a named snapshot
seguro snapshot restore NAME           Restore a session from a snapshot
seguro images ls                       List available base images
seguro images build [--browser]        Build base image(s)
seguro proxy-log [SESSION_ID]          Tail the proxy request log
seguro api-usage [SESSION_ID]          View AI API token usage
```

### Network modes (`--net`)

| Mode | Internet | Host LAN | Notes |
|------|----------|----------|-------|
| `air-gapped` | No | No | Complete isolation |
| `api-only` | Allowlist only | No | Configure allowed hosts in `seguro.toml` |
| `full-outbound` (default) | Yes | No | Logs all requests, blocks RFC 1918 (SSRF) |
| `dev-bridge` | Yes | Yes | **UNSAFE** — requires `--unsafe-dev-bridge` |

### TLS inspection

Pass `--tls-inspect` to enable full URL logging via MITM proxy. A CA cert is
generated and injected into the guest automatically.

### AI API usage tracking

When `--tls-inspect` is active, the proxy automatically detects requests to
known AI API providers (Anthropic, OpenAI, Google, Mistral) and extracts token
usage from responses. Per-session usage is logged to `api-usage.jsonl` and
aggregated in `SessionUsage` counters.

```sh
# View token usage for the current session
seguro api-usage

# View for a specific session
seguro api-usage <SESSION_ID>
```

The default provider map can be extended in config:

```toml
[proxy.ai_providers]
custom = ["my-llm-gateway.internal"]
```

No message content is captured — only metadata (model, token counts, latency).

## Configuration

- User config: `~/.config/seguro/config.toml`
- Project config: `.seguro.toml` in the shared directory

Project config overrides user config for that session.

Example `seguro.toml` for `api-only` mode:

```toml
[proxy.api_only.allow]
hosts = [
  "api.anthropic.com",
  "github.com",
  "api.github.com",
  "registry.npmjs.org",
  "pypi.org",
]
```

## Security model

- Host `~/.ssh`, `~/.config`, and all files outside the shared directory are
  **never** accessible to the guest
- SSH authentication uses an **ephemeral ed25519 key** generated per session
  (stored in `/run/seguro/`, deleted on exit)
- All HTTP/HTTPS traffic is logged to a per-session JSONL file on the host
- RFC 1918, loopback, and link-local addresses are always blocked (SSRF
  protection)
- Non-HTTP/S outbound TCP is dropped by guest iptables rules
- The agent runs as an unprivileged `agent` user inside the guest with no sudo
