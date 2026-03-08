## Approach

Add `ProfileConfig` struct + `profiles: HashMap<String, ProfileConfig>` to Config.

### ProfileConfig fields
- `image_suffix: Option<String>` — maps to `base-{suffix}.qcow2`, None = `base.qcow2`
- `memory_mb: Option<u32>`
- `smp: Option<u32>`
- `packages: Vec<String>` — apt packages for image build
- `env: HashMap<String, String>` — guest env vars

### Built-in defaults
- "default": no suffix, 2048MB, 2 smp
- "browser": suffix "browser", 4096MB, 4 smp, chromium packages + Playwright env

### Resolution
- `Config::profile(name) -> ProfileConfig` merges built-in → user → project
- Merge: project [profiles.X] overrides user [profiles.X] field-by-field

### Tests
- Parse profiles from TOML
- Built-in defaults exist for "default" and "browser"
- Custom profile from TOML
- Merge: project overrides user profile fields
- Unknown profile returns default with no suffix
