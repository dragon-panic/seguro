## Context

An orchestrator (ox, or any programmatic consumer of the seguro API) needs to
declare what VM image its agents require — packages, memory, CPU, guest setup.
Seguro shouldn't hardcode profiles like "browser" or "cua". Instead, profiles
are **data** that the orchestrator or user defines in config.

## Design

### Profile as data, not enum

A profile is a named config block that declares:

```toml
[profiles.default]
# image_suffix omitted → uses base.qcow2
memory_mb = 2048
smp = 2

[profiles.browser]
image_suffix = "browser"       # → base-browser.qcow2
memory_mb = 4096
smp = 4
packages = ["chromium-browser", "fonts-liberation"]
env = { PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH = "/usr/bin/chromium-browser" }

[profiles.cua]
image_suffix = "cua"
memory_mb = 8192
smp = 4
packages = ["chromium-browser", "xvfb", "x11vnc", "openbox"]
env = { DISPLAY = ":99" }

[profiles.openai-agent]
image_suffix = "openai"
memory_mb = 4096
smp = 2
packages = ["python3", "python3-pip"]
```

- `image_suffix` → maps to `base-{suffix}.qcow2`. Omit for bare `base.qcow2`.
- `packages` → apt packages to bake into the image at build time.
- `env` → env vars injected into guest at session start.
- `memory_mb`, `smp` → VM resource overrides.

### Image naming & layering

```
base.qcow2                          ← bare Ubuntu + core tools (always exists)
base-browser.qcow2   (backed by base.qcow2)   ← + chromium, fonts
base-cua.qcow2       (backed by base.qcow2)   ← + xvfb, vnc, wm
base-openai.qcow2    (backed by base.qcow2)   ← + custom python env
session.qcow2        (backed by profile image) ← ephemeral per-session
```

qcow2 backing chains keep shared layers deduplicated on disk.

### CLI surface

```
seguro run --profile browser -- claude    # use browser profile
seguro run --browser -- claude            # alias for --profile browser (compat)
seguro run -- claude                      # uses "default" profile

seguro images build                       # builds base.qcow2
seguro images build --profile browser     # builds base-browser.qcow2
seguro images build --browser             # alias (compat)
seguro images ls                          # shows all images + which profile they serve
```

### API surface (SandboxConfig)

```rust
pub struct SandboxConfig {
    pub profile: Option<String>,  // NEW — profile name, None = "default"
    // ... existing fields ...
}
```

`Sandbox::start()` resolves profile → image path, memory, smp, env vars.
Explicit SandboxConfig fields (memory_mb, smp, env) override profile defaults.

### Config resolution order

1. Built-in defaults (profile "default": 2G, 2 smp)
2. User config `~/.config/seguro/config.toml` `[profiles.*]`
3. Project config `.seguro.toml` `[profiles.*]`
4. CLI flags / SandboxConfig fields (highest priority)

### What changes

- `src/config.rs` — add `profiles: HashMap<String, ProfileConfig>` to Config
- `src/cli.rs` — add `--profile <NAME>`, keep `--browser` as alias
- `src/session/image.rs` — `locate_base(profile)` takes a string, not bool
- `src/api.rs` — SandboxConfig gains `profile: Option<String>`
- `src/commands/run.rs` — resolve profile before building QemuParams
- `scripts/build-image.sh` — accept `--profile` + read profile config for packages
- `src/commands/images.rs` — `images build` accepts `--profile`

### What does NOT change yet

- No new images actually built (only base + base-browser exist)
- No CUA/VNC guest setup code
- No multi-agent orchestration
- Build script still works with `--browser` flag as before

## Acceptance

- [ ] `ProfileConfig` struct exists with image_suffix, memory_mb, smp, packages, env
- [ ] Config parses `[profiles.*]` sections from TOML
- [ ] `--profile browser` works identically to current `--browser`
- [ ] `--browser` still works (alias)
- [ ] `SandboxConfig.profile` field exists, API consumers can set it
- [ ] `locate_base` accepts profile name string
- [ ] `seguro images build --profile X` builds base-X.qcow2
- [ ] No regressions — existing tests pass, existing `--browser` behavior unchanged
