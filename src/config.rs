use color_eyre::eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level seguro configuration.
/// Loaded from ~/.config/seguro/config.toml, optionally overridden by
/// .seguro.toml in the --share directory.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub guest: GuestConfig,
    #[serde(default)]
    pub vm: VmConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxyConfig {
    #[serde(default)]
    pub api_only: ApiOnlyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ApiOnlyConfig {
    #[serde(default)]
    pub allow: AllowConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowConfig {
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
pub struct GuestConfig {
    #[serde(default)]
    pub apk_allow: ApkAllowConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApkAllowConfig {
    #[serde(default = "default_apk_packages")]
    pub packages: Vec<String>,
}

fn default_apk_packages() -> Vec<String> {
    vec![
        "nodejs".into(),
        "npm".into(),
        "python3".into(),
        "py3-pip".into(),
        "git".into(),
        "curl".into(),
    ]
}

impl Default for ApkAllowConfig {
    fn default() -> Self {
        Self { packages: default_apk_packages() }
    }
}

/// VM resource overrides (useful in .seguro.toml for specific projects)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VmConfig {
    /// Override guest RAM in MB (defaults: 2048 base, 4096 with --browser)
    pub memory_mb: Option<u32>,
    /// Override guest vCPU count (defaults: 2 base, 4 with --browser)
    pub smp: Option<u32>,
}

impl Config {
    /// Load user config merged with optional project-level override.
    pub fn load(project_dir: Option<&Path>) -> Result<Self> {
        let mut config = Self::load_user()?;
        if let Some(dir) = project_dir {
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

    /// Overlay `other` on top of self (project config wins).
    fn merge(&mut self, other: Config) {
        if !other.proxy.api_only.allow.hosts.is_empty() {
            self.proxy.api_only.allow.hosts = other.proxy.api_only.allow.hosts;
        }
        if !other.guest.apk_allow.packages.is_empty() {
            self.guest.apk_allow.packages = other.guest.apk_allow.packages;
        }
        if let Some(v) = other.vm.memory_mb {
            self.vm.memory_mb = Some(v);
        }
        if let Some(v) = other.vm.smp {
            self.vm.smp = Some(v);
        }
    }
}

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
    // Prefer /run/seguro (tmpfs on most Linux); fall back to /tmp/seguro
    let run = PathBuf::from("/run/seguro");
    if run.parent().map(|p| p.exists()).unwrap_or(false) {
        run
    } else {
        std::env::temp_dir().join("seguro")
    }
}
