use color_eyre::eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ── Top-level ─────────────────────────────────────────────────────────────────

/// Resolved seguro configuration after two-level merge.
///
/// Loading order (later wins):
///   1. Built-in defaults (via Default impls)
///   2. User config: `~/.config/seguro/config.toml`
///   3. Project config: `.seguro.toml` in the `--share` directory (if present)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub guest: GuestConfig,
    #[serde(default)]
    pub vm: VmConfig,
    #[serde(default)]
    pub session: SessionConfig,
}

// ── Proxy ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxyConfig {
    /// Default network isolation mode for `seguro run` when --net is not given.
    /// Valid values: "air-gapped", "api-only", "full-outbound", "dev-bridge".
    pub default_net: Option<String>,

    /// Enable TLS inspection by default (off unless --tls-inspect is passed).
    pub tls_inspect: Option<bool>,

    #[serde(default)]
    pub api_only: ApiOnlyConfig,

    /// Hosts always blocked regardless of mode (supplemental deny list).
    #[serde(default)]
    pub deny: DenyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ApiOnlyConfig {
    #[serde(default)]
    pub allow: AllowConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowConfig {
    /// Allowed hostnames in api-only mode. Subdomains are also allowed.
    #[serde(default = "default_api_only_hosts")]
    pub hosts: Vec<String>,
}

fn default_api_only_hosts() -> Vec<String> {
    vec![
        "api.anthropic.com".into(),
        "github.com".into(),
        "api.github.com".into(),
        "objects.githubusercontent.com".into(),
        "registry.npmjs.org".into(),
        "pypi.org".into(),
        "files.pythonhosted.org".into(),
        "crates.io".into(),
        "static.crates.io".into(),
    ]
}

impl Default for AllowConfig {
    fn default() -> Self {
        Self { hosts: default_api_only_hosts() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DenyConfig {
    /// Hostnames always blocked (all modes). Matched as suffix.
    #[serde(default)]
    pub hosts: Vec<String>,
}

// ── Guest ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GuestConfig {
    /// Packages the agent is allowed to install via `sudo apt-get install`.
    #[serde(default)]
    pub apt_allow: AptAllowConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AptAllowConfig {
    #[serde(default = "default_apt_packages")]
    pub packages: Vec<String>,
}

fn default_apt_packages() -> Vec<String> {
    vec![
        "nodejs".into(),
        "npm".into(),
        "python3".into(),
        "python3-pip".into(),
        "git".into(),
        "curl".into(),
    ]
}

impl Default for AptAllowConfig {
    fn default() -> Self {
        Self { packages: default_apt_packages() }
    }
}

// ── VM ────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VmConfig {
    /// Override guest RAM in MB.
    /// CLI default: 2048 (4096 with --browser).
    pub memory_mb: Option<u32>,

    /// Override guest vCPU count.
    /// CLI default: 2 (4 with --browser).
    pub smp: Option<u32>,
}

// ── Session ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionConfig {
    /// SSH connection timeout in seconds (default: 15).
    pub ssh_timeout_secs: Option<u32>,

    /// Keep session overlays after exit by default (like --persistent).
    pub persistent: Option<bool>,
}

// ── Impl ──────────────────────────────────────────────────────────────────────

impl Config {
    /// Load user config merged with optional project-level override.
    ///
    /// `share_dir` is the directory passed to `--share`; if it contains a
    /// `.seguro.toml`, those values override the user config.
    pub fn load(share_dir: Option<&Path>) -> Result<Self> {
        let mut config = Self::load_user()?;
        if let Some(dir) = share_dir {
            let project_path = dir.join(".seguro.toml");
            if project_path.exists() {
                let project = Self::from_path(&project_path)?;
                config.merge(project);
            }
        }
        Ok(config)
    }

    fn load_user() -> Result<Self> {
        let path = user_config_path();
        if path.exists() {
            Self::from_path(&path)
        } else {
            Ok(Self::default())
        }
    }

    fn from_path(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .wrap_err_with(|| format!("reading {}", path.display()))?;
        toml::from_str(&text)
            .wrap_err_with(|| format!("parsing {}", path.display()))
    }

    /// Overlay `other` on top of self. All `Some` values in `other` win.
    fn merge(&mut self, other: Config) {
        // proxy
        if let Some(v) = other.proxy.default_net {
            self.proxy.default_net = Some(v);
        }
        if let Some(v) = other.proxy.tls_inspect {
            self.proxy.tls_inspect = Some(v);
        }
        if !other.proxy.api_only.allow.hosts.is_empty() {
            self.proxy.api_only.allow.hosts = other.proxy.api_only.allow.hosts;
        }
        if !other.proxy.deny.hosts.is_empty() {
            self.proxy.deny.hosts = other.proxy.deny.hosts;
        }

        // guest
        if !other.guest.apt_allow.packages.is_empty() {
            self.guest.apt_allow.packages = other.guest.apt_allow.packages;
        }

        // vm
        if let Some(v) = other.vm.memory_mb {
            self.vm.memory_mb = Some(v);
        }
        if let Some(v) = other.vm.smp {
            self.vm.smp = Some(v);
        }

        // session
        if let Some(v) = other.session.ssh_timeout_secs {
            self.session.ssh_timeout_secs = Some(v);
        }
        if let Some(v) = other.session.persistent {
            self.session.persistent = Some(v);
        }
    }

    /// Effective SSH timeout in seconds.
    pub fn ssh_timeout(&self) -> u32 {
        self.session.ssh_timeout_secs.unwrap_or(120)
    }
}

// ── Paths ─────────────────────────────────────────────────────────────────────

pub fn user_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("seguro")
        .join("config.toml")
}

pub fn images_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join("seguro")
        .join("images")
}

pub fn runtime_dir() -> PathBuf {
    // XDG_RUNTIME_DIR is /run/user/{uid}/ on systemd — user-owned tmpfs, ideal for sockets/pids
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(xdg).join("seguro");
    }
    // Fallback: /tmp/seguro-{uid}
    std::env::temp_dir().join(format!("seguro-{}", nix::unistd::getuid()))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_toml(dir: &std::path::Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        write!(f, "{}", content).unwrap();
        path
    }

    #[test]
    fn default_config_has_expected_api_only_hosts() {
        let cfg = Config::default();
        assert!(cfg.proxy.api_only.allow.hosts.contains(&"api.anthropic.com".to_string()));
        assert!(cfg.proxy.api_only.allow.hosts.contains(&"crates.io".to_string()));
    }

    #[test]
    fn default_config_has_expected_apt_packages() {
        let cfg = Config::default();
        assert!(cfg.guest.apt_allow.packages.contains(&"git".to_string()));
        assert!(cfg.guest.apt_allow.packages.contains(&"nodejs".to_string()));
    }

    #[test]
    fn project_config_overrides_memory() {
        let mut base = Config::default();
        let mut project = Config::default();
        project.vm.memory_mb = Some(8192);
        base.merge(project);
        assert_eq!(base.vm.memory_mb, Some(8192));
    }

    #[test]
    fn project_config_overrides_apt_allow() {
        let mut base = Config::default();
        let mut project = Config::default();
        project.guest.apt_allow.packages = vec!["git".into(), "htop".into()];
        base.merge(project);
        assert_eq!(base.guest.apt_allow.packages, vec!["git", "htop"]);
    }

    #[test]
    fn project_config_overrides_api_only_hosts() {
        let mut base = Config::default();
        let mut project = Config::default();
        project.proxy.api_only.allow.hosts = vec!["example.com".into()];
        base.merge(project);
        assert_eq!(base.proxy.api_only.allow.hosts, vec!["example.com"]);
    }

    #[test]
    fn empty_project_config_does_not_clear_base_values() {
        let mut base = Config::default();
        let project = Config::default(); // empty (all defaults)
        // defaults have non-empty lists, but merge should not override with defaults
        let original_hosts = base.proxy.api_only.allow.hosts.clone();
        base.merge(project);
        // an empty project override list doesn't clear the base
        assert_eq!(base.proxy.api_only.allow.hosts, original_hosts);
    }

    #[test]
    fn load_from_toml_file() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            "config.toml",
            r#"
[vm]
memory_mb = 4096
smp = 4

[proxy.api_only.allow]
hosts = ["api.example.com"]
"#,
        );

        let cfg = Config::from_path(&dir.path().join("config.toml")).unwrap();
        assert_eq!(cfg.vm.memory_mb, Some(4096));
        assert_eq!(cfg.vm.smp, Some(4));
        assert_eq!(cfg.proxy.api_only.allow.hosts, vec!["api.example.com"]);
    }

    #[test]
    fn merge_priority_project_wins_over_user() {
        let mut user = Config::default();
        user.vm.memory_mb = Some(2048);

        let mut project = Config::default();
        project.vm.memory_mb = Some(4096);

        user.merge(project);
        assert_eq!(user.vm.memory_mb, Some(4096));
    }

    #[test]
    fn ssh_timeout_default() {
        let cfg = Config::default();
        assert_eq!(cfg.ssh_timeout(), 120);
    }

    #[test]
    fn ssh_timeout_override() {
        let mut cfg = Config::default();
        cfg.session.ssh_timeout_secs = Some(30);
        assert_eq!(cfg.ssh_timeout(), 30);
    }
}
