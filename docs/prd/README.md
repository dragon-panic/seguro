# Seguro — Secure Sandbox for CLI Coding Agents

## Problem

CLI coding agents like Claude Code, Aider, and Cursor operate with broad filesystem and network access. On a development laptop this means an agent can accidentally (or through prompt injection) read credentials, SSH keys, browser cookies, or cloud provider tokens — or write to files outside the project. Giving an agent "more autonomy" today means increasing that blast radius.

## Goal

Run a CLI coding agent inside a minimal, hardened QEMU virtual machine that:

- Has **no access** to the host filesystem unless explicitly shared
- Has **controlled network access** (optionally air-gapped or allowlisted)
- Can produce output (code, commits, binaries) that reaches the host with minimal friction
- Supports both **GitHub-based coordination** (async, durable) and **local host↔guest sharing** (fast, for inner-loop testing)
- Launches quickly enough that it feels like a normal dev tool (target: <10 s cold, <2 s warm)

---

## Architecture Overview

```
┌──────────────────────────────────────────────────────────┐
│  Host OS (laptop)                                        │
│                                                          │
│  ┌──────────┐    virtio-fs / 9p    ┌──────────────────┐ │
│  │ workspace│◄──────────────────── │   QEMU guest     │ │
│  │  (ro/rw) │                      │   Alpine Linux   │ │
│  └──────────┘                      │                  │ │
│                                    │  agent process   │ │
│  ┌──────────┐    vsock / SSH       │  chromium        │ │
│  │ host CLI │◄──────────────────── │  (headless)      │ │
│  │  wrapper │                      └────────┬─────────┘ │
│  └──────────┘                               │ HTTP(S)   │
│                                             ▼           │
│  ┌──────────────────────────────────────────────────┐   │
│  │  transparent proxy (seguro built-in, Rust)       │   │
│  │  • logs all requests                             │   │
│  │  • enforces domain allow/deny list               │   │
│  │  • blocks RFC 1918 (SSRF protection)             │   │
│  └──────────────────────────┬─────────────────────-─┘   │
│                             │ filtered internet          │
│  ~/.ssh, ~/.config  ← never mounted                      │
└─────────────────────────────────────────────────────-────┘
```

---

## Guest OS Selection

### Current: Ubuntu 24.04 minimal (cloud image)

| Property | Value |
|---|---|
| Image size | ~350 MB base |
| Boot time | ~3–5 s with KVM |
| Attack surface | Minimal cloud image — no desktop, no snap |
| Package manager | apt |
| Cloud-init | Per-session cidata disk (FAT12) for SSH key + CA injection |

Ubuntu 24.04 minimal cloud image is the current guest OS. Claude Code and its Node.js runtime are pre-installed via cloud-init at image build time.

**Browser profile:** bump `-m` to 4G and `-smp` to 4 when enabling Playwright/browser-use. Chromium is memory-heavy and will OOM at 2G with multiple tabs.

---

## Headless Browser Support (Playwright / browser-use)

### Runtime choice

Playwright ships its own Chromium binaries linked against glibc. **These will not run on Alpine's musl libc.** Two options:

| Approach | Pros | Cons |
|---|---|---|
| Use Alpine's system `chromium` package + point Playwright at it | Tiny, musl-native, already in apk | Lags upstream Chromium by a few weeks |
| Switch guest OS to Debian/Ubuntu slim | Playwright bundles "just work" | Larger image (~400 MB), slower boot (~3 s) |

**Recommendation:** use Alpine system Chromium for v1. Set the env var so Playwright/browser-use skips its own download:

```sh
export PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH=/usr/bin/chromium-browser
export PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1
```

Install in the base image:

```sh
apk add chromium chromium-chromedriver
```

### Sandbox flags

Chromium normally uses Linux namespaces and seccomp as its own sandbox. Inside a VM, the VM boundary is the outer sandbox — it is safe to disable Chromium's internal sandbox (which would otherwise require elevated privileges):

```python
browser = playwright.chromium.launch(
    args=["--no-sandbox", "--disable-dev-shm-usage"]
)
```

`--disable-dev-shm-usage` is needed because `/dev/shm` is small inside the VM by default; Chromium falls back to `/tmp` automatically.

### Display

`--headless=new` (Chromium ≥112) does not need Xvfb or any display server. No X11 or Wayland needed in the guest at all.

If you ever need to visually inspect what the browser sees (debugging), forward a VNC port:

```sh
# In guest: Xvfb :99 & DISPLAY=:99 chromium ...
# QEMU: -hostfwd tcp::5900-:5900
# Host: vncviewer localhost:5900
```

This is a debug-only path, not part of normal operation.

### browser-use

`browser-use` wraps Playwright and inherits all of the above. No special configuration beyond the Playwright setup. The agent process and the browser process run inside the same VM, so browser cookies, session storage, and downloaded files are all contained within the guest.

---

## VM Configuration

### QEMU flags (q35 profile)

```sh
qemu-system-x86_64 \
  -M q35 \
  -cpu host -enable-kvm \
  -m 2G \
  -smp 2 \
  -drive file=agent-root.qcow2,format=qcow2,if=virtio \
  -netdev user,id=net0,hostfwd=tcp::${SSH_PORT}-:22,guestfwd=tcp:10.0.2.100:3128-cmd:"mitmdump --mode regular --listen-port 8080 -s /etc/seguro/proxy.py" \
  -device virtio-net-pci,netdev=net0 \
  -chardev socket,id=char0,path=${VIRTIOFS_SOCK} \
  -device vhost-user-fs-pci,chardev=char0,tag=workspace \
  -object memory-backend-file,id=mem,size=2G,mem-path=/dev/shm,share=on \
  -numa node,memdev=mem \
  -nographic -serial stdio
```

Key choices:

- `-M q35` — modern PCIe chipset; supports `vhost-user-fs-pci` and all virtio PCI devices. Boots in ~2–3 s with KVM, which is acceptable. (`-M microvm` strips all PCI and is incompatible with virtio-fs.)
- `-cpu host -enable-kvm` — near-native performance; falls back to TCG with a warning if `/dev/kvm` is unavailable
- `guestfwd` — forwards a virtual guest-reachable address (`10.0.2.100:3128`) to the mitmproxy process on the host; see Transparent Proxy below
- `${SSH_PORT}` — dynamically allocated by the CLI wrapper per session to avoid port collisions; see Host Wrapper CLI
- No USB, no audio

### Network isolation modes

```
Mode            | Guest internet | Host LAN  | Proxy
----------------|---------------|-----------|--------------------
air-gapped      | ✗             | ✗         | n/a
api-only        | allowlist      | ✗         | enforced (deny-default)
full-outbound   | ✓             | ✗         | logging + SSRF block (default)
dev-bridge      | ✓             | ✓ UNSAFE  | logging + SSRF block
```

**`full-outbound` with transparent proxy is the recommended default.**

> **`dev-bridge` WARNING:** This mode allows the guest to reach your home/office LAN, which partially undermines the threat model. Enabling it requires passing `--unsafe-dev-bridge` explicitly. Use only for local service integration testing where you understand the risk.

### Transparent proxy (Seguro built-in, via guestfwd)

`restrict=yes` on the SLIRP netdev blocks all outbound guest traffic but also prevents the guest from reaching a host-side proxy. Instead, Seguro uses SLIRP's `guestfwd` TCP forward variant to expose a virtual address (`10.0.2.100:3128`) inside the guest that forwards to Seguro's own proxy server running on a random localhost port. The proxy becomes the enforcement point; no `restrict=yes` is needed.

```
QEMU netdev:
  guestfwd=tcp:10.0.2.100:3128-tcp:127.0.0.1:${PROXY_PORT}

  (Seguro starts its proxy server first, allocates PROXY_PORT, then passes it to QEMU)

Guest iptables (injected at boot via rc.local):
  # Route all HTTP/HTTPS through the proxy virtual address
  iptables -t nat -A OUTPUT ! -d 10.0.2.0/24 -p tcp --dport 80  -j DNAT --to-destination 10.0.2.100:3128
  iptables -t nat -A OUTPUT ! -d 10.0.2.0/24 -p tcp --dport 443 -j DNAT --to-destination 10.0.2.100:3128

  # Block non-proxy outbound TCP as defence-in-depth
  iptables -A OUTPUT ! -d 10.0.2.0/24 -p tcp -j DROP
  iptables -A OUTPUT -p udp --dport 53 -j ACCEPT   # DNS to SLIRP resolver only
  iptables -A OUTPUT ! -d 10.0.2.0/24 -p udp -j DROP
```

This gives:
- **Full request log** — every HTTP/HTTPS request recorded to a per-session JSONL file on the host
- **Domain allow/deny list** — configured in `seguro.toml`, evaluated in `proxy/filter.rs`
- **SSRF protection** — RFC 1918 destinations always denied (see below)
- **TLS visibility** — default: log SNI hostname only (no CA needed in guest). Opt-in `--tls-inspect` generates a CA via `rcgen`, installs it in the guest, and enables full URL + response body logging via `hudsucker`.

For `api-only` mode the allow list is configured in `seguro.toml`:

```toml
[proxy.api_only.allow]
hosts = [
  "api.anthropic.com",
  "github.com",
  "api.github.com",
  "objects.githubusercontent.com",
  "registry.npmjs.org",
  "pypi.org",
  "files.pythonhosted.org",
  "crates.io",
  "static.crates.io",
]
```

### SSRF protection (always on)

Regardless of mode, `proxy.py` always blocks requests whose resolved IP falls within:

- RFC 1918: `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`
- Loopback: `127.0.0.0/8`, `::1`
- Link-local: `169.254.0.0/16` (cloud metadata endpoints)
- SLIRP gateway: `10.0.2.2`

### Proxy scope limitation

mitmproxy handles HTTP and HTTPS only. The iptables rules above also drop non-HTTP/S TCP and non-DNS UDP as defence-in-depth. However, raw TCP on non-standard ports (e.g., custom agent protocols, git over SSH on port 22) bypasses the proxy's content inspection. For v1 this is acceptable — the DROP rules prevent exfiltration over arbitrary ports, even if there is no visibility into what was attempted.

DNS is resolved by SLIRP's built-in resolver (forwarding to the host's system resolver). DNS-based exfiltration (data encoded in lookup hostnames) is not blocked in v1.

### Browser-specific considerations

When browser-use/Playwright is active, the browser makes requests to arbitrary third-party domains (CDNs, analytics, auth providers). This is expected. The proxy logs all of it but does not block in `full-outbound` mode.

The browser runs inside the guest with no access to host cookies, saved passwords, or host TLS certificates.

---

## File Sharing

### Primary: virtio-fs (recommended)

`virtiofsd` (Rust rewrite, shipped with QEMU ≥7.2) runs on the host as an unprivileged daemon and exposes a directory over a virtio socket. The QEMU flags are shown in VM Configuration above. The CLI wrapper starts `virtiofsd` automatically before launching QEMU and uses a per-session socket path to avoid conflicts.

```sh
# Host: start virtiofsd (CLI wrapper does this automatically)
virtiofsd \
  --socket-path=/run/seguro/${SESSION_ID}/virtiofs.sock \
  --shared-dir=${SHARE_PATH} \
  --announce-submounts \
  --sandbox=namespace \
  --log-level=warn &
```

Inside the guest:

```sh
mount -t virtiofs workspace /mnt/workspace
```

Performance is close to native. The host daemon runs as your user — it cannot escalate.

### Rejected: virtio-9p

> **Do not use virtio-9p for the workspace share.** It was tried (commit 816fea8)
> and reverted. Problems: mmap/file-locking quirks break real-world toolchains
> (npm, cargo, git), `security_model=none` loses uid/gid mapping, requires
> `sudo mount` inside the guest, and performance is 2–5× worse than virtiofs.
> The implementation never passed the demo/02 file-sharing test.
>
> Keep virtiofs + virtiofsd as the only supported workspace sharing mechanism.

### Tertiary: GitHub (async coordination)

1. Guest has its own GitHub identity (bot token or deploy key, scoped to one repo/org)
2. Agent commits and pushes to a feature branch
3. Host pulls, reviews, merges
4. CI runs on GitHub Actions — never needs VM access

This is the safest coordination path and requires no direct host↔guest socket.

### Local loop (for testing / inner dev)

For rapid inner-loop testing where you want the agent to build and you want to run the result immediately:

```
agent (guest) → writes binary to /mnt/workspace/out/
host          → inotifywait on workspace/out/ → runs binary in host terminal
```

Or via SSH forwarded port:

```sh
# Guest runs a tiny HTTP server on :8080
# Host accesses http://localhost:18080 via hostfwd tcp::18080-:8080
```

---

## Guest Access (SSH)

The CLI wrapper SSHs into the guest to exec the agent. Authentication uses an ephemeral key pair generated per session — never the user's `~/.ssh/id_*` keys.

```
seguro run:
  1. ssh-keygen -t ed25519 -f /run/seguro/${SESSION_ID}/id_ed25519 -N ""
  2. Pass public key to guest via cloud-init cidata (FAT12 seed disk)
     - meta-data: instance-id (changes each session to force re-run)
     - user-data: creates agent user, installs SSH key, optional CA cert
  3. cloud-init runs on first boot, sets up agent user with the key
  4. CLI connects: ssh -i /run/seguro/${SESSION_ID}/id_ed25519 -p ${SSH_PORT} agent@localhost
```

The private key lives in `/run/seguro/` (tmpfs on most Linux systems) and is deleted when the session exits. It is never written to the workspace share or to disk in the guest.

### Agent sudo policy

The agent user has **no sudo access** by default. All toolchain packages (Node, Python, Rust, Go, git, Chromium, etc.) must be pre-installed in the base image. This is intentional — if the agent can run `apk add`, it can install arbitrary binaries and widen its attack surface.

The only exception is `/sbin/apk` scoped to a curated allowlist, configurable in `seguro.toml`:

```toml
[guest.apk_allow]
packages = ["nodejs", "npm", "python3", "py3-pip", "git", "curl"]
```

If a package is not in the allowlist and not in the base image, the agent cannot install it. The base image should be rebuilt with the package added instead.

---

## Security Model

### What the guest cannot do

| Threat | Mitigation |
|---|---|
| Read host SSH keys | Not mounted; host ~/.ssh never in any share |
| Read host credentials | Host $HOME not mounted; only explicit workspace dir |
| Reach host LAN | `restrict=yes` on SLIRP netdev |
| Persist malware across resets | Boot from snapshot; `--snapshot` flag discards writes |
| Escape via kernel exploit | KVM + separate kernel; host kernel not shared |
| Privilege escalate inside guest | Alpine + no setuid; agent runs as unprivileged user `agent` |
| Browser reads host cookies/passwords | Browser runs inside guest; no host profile mounted |
| Browser exfiltrates workspace files | Browser has no filesystem access beyond what the agent explicitly passes to it |
| Malicious page probes host LAN (SSRF) | Proxy blocks all RFC 1918 and link-local destinations |
| Agent browses to malicious page that exfiltrates secrets | Secrets are in guest memory only; TLS inspection log captures URLs |

### What the host controls

- Which directories are shared (and as read-only vs read-write)
- Which ports are forwarded
- Whether the disk is ephemeral (`--snapshot`) or persistent
- Network allowlist policy

### Credential injection

The agent needs API keys (e.g., `ANTHROPIC_API_KEY`). Do **not** mount credential files. Instead:

```sh
# Option A: environment variable via QEMU -fw_cfg
# Option B: vsock secret store (simple key-value daemon on host)
# Option C: agent-specific vault (Vault agent, 1Password CLI) inside guest
```

Option B sketch:

```
host: vsock server listens on CID 2, port 9999
      responds to GET <key> with the secret value, once

guest: reads key at startup via socat vsock-connect:2:9999
       stores in memory only, never on disk
```

---

## Disk Image Management

```
base.qcow2          — golden Alpine image, read-only, versioned
  └── session.qcow2 — copy-on-write overlay, discarded after session
```

Build `base.qcow2` with Packer or a shell script. Keep it in git-lfs or an OCI registry. Rebuild it monthly or when Alpine releases a security update.

```sh
# Start ephemeral session (all writes discarded on exit)
qemu-system-x86_64 ... --snapshot -drive file=base.qcow2,...

# Start persistent session (writes survive)
qemu-img create -f qcow2 -b base.qcow2 session-$(date +%s).qcow2
qemu-system-x86_64 ... -drive file=session-$(date +%s).qcow2,...
```

---

## Implementation

Seguro is implemented as a single Rust binary. There are no Python or scripting language dependencies on the host beyond QEMU and virtiofsd.

### Key crates

```toml
clap              = { version = "4", features = ["derive"] }
tokio             = { version = "1", features = ["full"] }
serde             = { version = "1", features = ["derive"] }
toml              = "0.8"
color-eyre        = "0.6"      # error reporting
tracing           = "0.1"
tracing-subscriber = "0.3"
hudsucker         = "0.3"      # MITM proxy (wraps hyper + rustls)
rcgen             = "0.13"     # CA + per-domain cert generation
rustls            = "0.23"
hyper             = { version = "1", features = ["full"] }
ed25519-dalek     = "2"        # ephemeral SSH key generation
ssh-key           = "0.6"      # serialize keys in OpenSSH wire format
uuid              = { version = "1", features = ["v4"] }
dirs              = "5"        # XDG paths
nix               = "0.29"     # Unix signals, process management
```

### Module structure

```
src/
  main.rs              — clap entrypoint, subcommand dispatch
  lib.rs               — library target, exposes all modules
  api.rs               — programmatic API: Sandbox, SandboxConfig, OutputMode,
                         RestartPolicy, PersonaConfig, SessionMeta, HealthState,
                         SessionEvent, AgentState, SessionUsage, WorkspaceState,
                         crash/health monitors, orchestrator primitives
  cli.rs               — all clap structs
  config.rs            — seguro.toml schema + loading + project override merge +
                         ProfileConfig + built-in profiles
  session/
    mod.rs             — Session struct, full lifecycle (start → running → cleanup)
    ports.rs           — dynamic host port allocation (bind :0, release)
    keys.rs            — ephemeral ed25519 key gen + OpenSSH serialization
    image.rs           — qcow2 overlay creation, snapshot management, GC
  vm/
    mod.rs             — QEMU process builder, SSH readiness polling
    virtiofsd.rs       — virtiofsd process management
    fw_cfg.rs          — -fw_cfg argument construction for env var injection
    cidata.rs          — cloud-init NoCloud seed disk generation
  proxy/
    mod.rs             — proxy server startup, mode dispatch, tokio task
    filter.rs          — SSRF block list, allow/deny list evaluation
    log.rs             — per-session request log writer (JSONL)
    ca.rs              — CA cert generation, per-domain cert signing cache
  commands/
    run.rs
    shell.rs
    sessions.rs
    images.rs
    proxy_log.rs
```

### Configuration

Two-level config merge: user defaults then project override.

- User config: `~/.config/seguro/config.toml`
- Project config: `.seguro.toml` in the directory passed to `--share` (if present)

Project config values override user config for that session only.

### VM Profiles

Profiles are named configuration blocks that define the VM image, resources, and
environment for a class of agent. Built-in profiles (`default`, `browser`) provide
sensible starting points. Orchestrators and users can define custom profiles in config:

```toml
[profiles.browser]
image_suffix = "browser"       # → base-browser.qcow2
memory_mb = 4096
smp = 4

[profiles.my-agent]
image_suffix = "my-agent"      # → base-my-agent.qcow2
memory_mb = 8192
smp = 4
packages = ["python3", "python3-venv"]

[profiles.my-agent.env]
AGENT_MODE = "autonomous"
```

Profile fields:

| Field | Type | Description |
|---|---|---|
| `image_suffix` | string | Maps to `base-{suffix}.qcow2`. Omit for bare `base.qcow2`. |
| `memory_mb` | integer | Guest RAM in MB. |
| `smp` | integer | Guest vCPU count. |
| `packages` | string[] | Apt packages to bake into the profile image at build time. |
| `env` | table | Environment variables injected into the guest at session start. |

Resolution order (later wins): built-in defaults → user config → project config → explicit CLI/API fields.

The programmatic API accepts `SandboxConfig { profile: Some("my-agent".into()), .. }`.

### Programmatic API

The `seguro::api` module exposes `Sandbox` for programmatic use by orchestrators (e.g. Ox).

#### Output capture

`OutputMode` controls where guest command I/O is routed:

| Mode | Behavior |
|------|----------|
| `Inherit` | Pipe to parent stdout/stderr (default) |
| `Null` | Discard output |
| `Capture` | Collect into `Vec<u8>`, returned in `SessionResult.stdout`/`.stderr` |
| `Stream(mpsc::Sender)` | Forward chunks in real-time via channel as `OutputChunk::Stdout`/`Stderr` |

Set via `SandboxConfig { stdout: OutputMode::Capture, stderr: OutputMode::Capture, .. }` or override per-call with `sandbox.exec_with(command, &stdout_mode, &stderr_mode)`.

#### Crash detection and auto-restart

`RestartPolicy` controls automatic QEMU restart on crash:

```rust
SandboxConfig {
    restart_policy: RestartPolicy {
        strategy: RestartStrategy::OnFailure, // Never | OnFailure | Always
        max_restarts: 3,
        backoff: vec![Duration::from_secs(1), Duration::from_secs(5), Duration::from_secs(15)],
    },
    ..
}
```

A background monitor watches the QEMU child process. On unexpected exit, it re-launches QEMU with the same overlay disk (preserving guest filesystem state), waits for SSH, and resumes. Default policy is `Never` — existing behavior unchanged.

#### Persona injection

`SandboxConfig { persona_config: Some(path), .. }` loads a TOML file that defines agent identity:

```toml
system_prompt = "You are a security auditor. Be thorough."
role = "auditor"

[env]
AGENT_MODE = "audit"
```

- `system_prompt` → written to `workspace/.claude/CLAUDE.md` (Claude Code picks it up automatically)
- `role` → injected as `SEGURO_ROLE` env var
- `env` → merged into guest environment (profile env < persona env < explicit env_vars)

#### Session persistence and recovery

Session metadata is persisted to `runtime_dir/session.json` during `Sandbox::start()`. Contains all parameters needed to reconstruct a session: ports, paths, env vars, net mode, profile.

`Sandbox::recover(session_id)` reconnects to an existing session by reading `session.json`, re-launching QEMU with the preserved overlay, and waiting for SSH.

#### Health monitoring

When `health_check_interval` is set, a background task periodically checks SSH connectivity:

| State | Meaning |
|-------|---------|
| `Healthy` | SSH responds quickly |
| `Degraded` | SSH responds but slowly (>5s) |
| `Unresponsive` | SSH did not respond within check interval |
| `Dead` | QEMU process exited |

API: `sandbox.health()` returns current state. `sandbox.subscribe_health()` returns a `watch::Receiver` for async notifications.

#### Event stream

All lifecycle transitions emit `SessionEvent` through a `broadcast` channel:

- `Started` — QEMU launched
- `SshReady` — SSH banner detected
- `ExecStarted` — command execution began
- `Completed` — command execution finished (with exit code and duration)
- `HealthChanged` — health state transitioned
- `Crashed` — QEMU exited unexpectedly
- `Restarted` — QEMU re-launched after crash (with attempt number)

API: `sandbox.events()` returns a `broadcast::Receiver<SessionEvent>`. Multiple subscribers receive all events.

#### Orchestrator primitives

Six synchronous methods for orchestrator integration (e.g. Ox). These are primitives — seguro provides the read/write operations, the orchestrator owns polling frequency, staleness detection, and escalation policy.

**Agent state reporting:**
`sandbox.agent_state()` reads `.seguro/status.json` from the virtiofs workspace (no SSH). Returns `Option<AgentState>` with fields `state` (string), `updated_at`, optional `task` and `progress`. Returns `None` if the file is missing or malformed (tolerates partial writes).

**Message injection:**
`sandbox.inject(message)` writes a timestamped `.md` file to `.seguro/inbox/` on virtiofs. Atomic write (temp + rename). The agent reads inbox files at turn boundaries (e.g. via a Claude Code hook). `sandbox.pending_messages()` counts unread `.md` files in the inbox directory.

**Graceful shutdown:**
`sandbox.kill()` writes a `.seguro/shutdown` sentinel file to the workspace before killing QEMU, then waits a configurable grace period (default 5s, set via `SandboxConfig::shutdown_grace`). During the grace period, QEMU is polled — if it exits cleanly, the wait is cut short. The agent can watch for the sentinel to save state before the VM disappears.

**Agent restart (without VM restart):**
`sandbox.kill_agent()` SSHes into the guest and kills all agent-user processes. The VM stays running — overlay, virtiofs, and proxy are untouched. The orchestrator calls `exec()` again to start a new agent. Enables fast restart (~instant) vs full VM restart (3-5s).

**Resource metering:**
`sandbox.usage()` returns live `SessionUsage` with wall-clock duration, proxy request count, blocked request count, and bytes received. Proxy counters are atomic — readable at any time without coordination.

**Workspace state inspection:**
`sandbox.workspace_state()` runs `git` on the host-side workspace path (no SSH) and returns `WorkspaceState` with `is_git_repo`, `has_uncommitted`, `has_unpushed`, and `dirty_files` count. Used for pre-kill verification — the orchestrator can refuse to terminate sessions with unpushed work. `sessions prune` also checks workspace state and skips dirty sessions unless `--force` is passed.

### Startup checks

On every invocation `seguro` verifies:

1. `qemu-system-x86_64` is on `$PATH` and reports version ≥ 7.2
2. `virtiofsd` is on `$PATH`
3. `/dev/kvm` is accessible — if not, warn and offer TCG fallback

All three failures produce a clear, actionable error message rather than a cryptic QEMU exit.

---

## `seguro` CLI

### Session startup sequence

1. Resolve profile → image, memory, smp, env vars
2. Allocate a free host port for SSH and proxy by binding to port 0 and releasing
3. Generate an ephemeral ed25519 key pair under `/run/seguro/${SESSION_ID}/`
4. Start the built-in Rust proxy server (HTTP/HTTPS, with filtering)
5. Create cloud-init cidata seed disk (SSH key + optional CA cert)
6. Load and apply persona config if provided (system prompt → `.claude/CLAUDE.md`, env vars)
7. Inject env vars and credentials into `workspace/.seguro/`
8. Start `virtiofsd` on a per-session socket path, pointed at the shared directory
9. Create a session qcow2 overlay on top of the profile's base image
10. Launch QEMU with all of the above wired together
11. Write `session.json` to runtime dir (enables `Sandbox::recover()`)
12. Wait for SSH to become available (poll with exponential backoff)
13. Spawn crash monitor (if restart policy ≠ Never) and health monitor (if interval set)
14. `ssh` into the guest and exec the requested agent command
15. On agent exit: write `.seguro/shutdown` sentinel, wait grace period, kill QEMU, stop virtiofsd, stop proxy, clean up `/run/seguro/${SESSION_ID}/`
16. If `--persistent`: keep the session overlay and workspace

### `--share` default behaviour

If `--share` is not provided, `seguro run` creates a temporary directory under `/tmp/seguro-workspace-${SESSION_ID}/` and mounts it as the workspace. The path is printed at startup so the user knows where to put files. On exit, the temp directory is deleted unless `--persistent` is also set.

### Commands

```
Usage:
  seguro run [--persistent] [--share PATH] [--profile NAME] [--browser] [--net MODE] [-- AGENT...]
  seguro shell [SESSION_ID]         # open a shell in a running session
  seguro sessions ls                # list active and saved sessions
  seguro sessions prune [--force]    # delete orphaned session overlays and /run state
  seguro snapshot save NAME         # save running session state as a named snapshot
  seguro snapshot restore NAME      # start a session from a named snapshot
  seguro images ls                  # list base images
  seguro images build [--profile NAME] [--browser]   # build base image for a profile
  seguro proxy log [SESSION_ID]     # tail the proxy request log for a session
```

`--profile NAME`: select a VM profile (defines image, RAM, CPU, env vars). See Configuration below.
`--browser`: alias for `--profile browser` (bumps RAM to 4G, SMP to 4, uses browser image).
`--net MODE`: `air-gapped` | `api-only` | `full-outbound` (default) | `dev-bridge` (requires `--unsafe-dev-bridge`).
`AGENT`: defaults to an interactive shell if omitted.

---

## Open Questions

1. **KVM availability** — laptops with Secure Boot + locked BIOS may not expose `/dev/kvm`. Need a graceful fallback to TCG (slower, ~5–10× overhead) with a warning to the user.
2. **`virtiofsd` availability** — shipped with QEMU ≥7.2 but may be a separate package on some distros (`virtiofsd` on Arch, `qemu-virtiofsd` on Fedora). Seguro should check and error clearly if missing.
3. **DNS exfiltration** — data encoded in DNS lookup hostnames is not blocked in v1. Low-risk for typical agent use but worth revisiting if the threat model tightens.
4. **`seguro.toml` location and schema** — needs a spec. Candidate: `~/.config/seguro/seguro.toml` for user defaults, `.seguro.toml` in a project directory to override per-project.

---

## Non-Goals (v1)

- GUI / desktop environment inside the VM
- macOS or Windows host support
- GPU / ML workloads
- Image signing / supply-chain verification
- Full container-in-VM nesting
- Custom kernel builds
- Automated CVE patching of the guest (manual rebuild cadence is fine for v1)

---

## Acceptance Criteria

- [ ] `seguro run --share ./myproject -- claude` launches Claude Code inside QEMU Alpine in <10 s on a machine with KVM
- [ ] Agent can read and write files in the shared directory; changes appear on the host in real time
- [ ] Agent cannot read files outside the shared directory (verified by `ls /root` failing or showing empty)
- [ ] Agent can reach github.com and api.anthropic.com; cannot reach host LAN IPs
- [ ] Killing the QEMU process leaves the host filesystem unchanged
- [ ] `--snapshot` mode discards all guest-side writes on exit
- [ ] API keys are available inside the guest without appearing in any mounted file or QEMU command line (no `ps aux` leak)
- [ ] Two concurrent `seguro run` sessions use different host SSH ports and do not interfere with each other
- [ ] `seguro run` without `--share` creates a temp workspace, prints its path, and deletes it on exit
- [ ] The ephemeral SSH key is in `/run/seguro/` and is deleted after the session; it never appears in the workspace share
- [ ] The agent user inside the guest cannot run `sudo apk add curl` (not in allowlist); can run `sudo apk add git` if git is in the allowlist
- [ ] `seguro sessions ls` shows running sessions; `seguro sessions prune` removes orphaned overlays
- [ ] `seguro run --browser -- claude` launches Claude Code with Chromium available; a Playwright script can fetch a public URL
- [ ] The proxy blocks requests to `192.168.0.0/16` and `10.0.2.2` (SSRF test)
- [ ] Non-HTTP/S outbound TCP from the guest is dropped (verified by attempting `nc -z 1.1.1.1 9999` from inside the guest)
- [ ] All HTTP/HTTPS requests made during a session are written to a log file on the host
- [ ] In `api-only` mode, a request to an unlisted domain returns 403; requests to `api.anthropic.com` succeed
- [ ] `--net dev-bridge` without `--unsafe-dev-bridge` exits with an error and a clear message
