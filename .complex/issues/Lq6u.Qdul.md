Implement seguro.toml loading and two-level merge.

- Define `Config` struct with serde (all fields Optional for merge)
- User config: `~/.config/seguro/config.toml` via `dirs`
- Project config: `.seguro.toml` in the shared directory (if present)
- Project values override user values; built-in defaults are the base
- Key config fields: default net mode, default memory/cpus, apk allow list, proxy allow/deny lists, tls_inspect flag
- Expose `Config::load(share_path: Option<&Path>) -> Result<Config>`
- Unit tests for merge priority