//! Programmatic API for managing sandboxed sessions.
//!
//! ```no_run
//! use seguro::api::{SandboxConfig, Sandbox};
//! use seguro::cli::NetMode;
//!
//! # async fn example() -> color_eyre::eyre::Result<()> {
//! let config = SandboxConfig {
//!     workspace: "/home/user/project".into(),
//!     env_vars: vec![("ANTHROPIC_API_KEY".into(), "sk-...".into())],
//!     ..Default::default()
//! };
//!
//! let sandbox = Sandbox::start(config).await?;
//! let result = sandbox.exec(&["claude".into(), "--help".into()]).await?;
//! println!("exit={:?} timed_out={} duration={:?}", result.exit_code, result.timed_out, result.duration);
//! sandbox.kill().await?;
//! # Ok(())
//! # }
//! ```

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr, eyre};
use tokio::time::Instant;

/// Controls where guest command I/O is routed.
#[derive(Debug, Clone, Copy, Default)]
pub enum OutputMode {
    /// Inherit parent's stdout/stderr (default).
    #[default]
    Inherit,
    /// Discard all output.
    Null,
}

use crate::cli::NetMode;
use crate::config::Config;
use crate::proxy::ProxyServer;
use crate::session::{Session, image};
use crate::vm::{self, QemuParams};
use crate::vm::virtiofsd::Virtiofsd;

/// Result of executing a command in the sandbox.
#[derive(Debug)]
pub struct SessionResult {
    /// Process exit code. None if the process was killed or timed out.
    pub exit_code: Option<i32>,
    /// Whether the session was terminated due to timeout.
    pub timed_out: bool,
    /// Wall-clock duration of the exec() call.
    pub duration: Duration,
}

/// Configuration for starting a sandboxed session.
pub struct SandboxConfig {
    /// Host directory to share with the guest via virtiofs.
    pub workspace: PathBuf,
    /// Network isolation mode.
    pub net: NetMode,
    /// Enable TLS inspection (MITM CA injected into guest).
    pub tls_inspect: bool,
    /// Environment variables to inject into the guest session.
    pub env_vars: Vec<(String, String)>,
    /// Guest RAM in MB.
    pub memory_mb: u32,
    /// Guest vCPU count.
    pub smp: u32,
    /// Keep session overlay and workspace after exit.
    pub persistent: bool,
    /// Use the browser base image variant.
    pub browser: bool,
    /// Kill the session after this duration. None means no timeout.
    pub timeout: Option<Duration>,
    /// Where to route guest command stdout.
    pub stdout: OutputMode,
    /// Where to route guest command stderr.
    pub stderr: OutputMode,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            workspace: PathBuf::new(),
            net: NetMode::FullOutbound,
            tls_inspect: false,
            env_vars: Vec::new(),
            memory_mb: 2048,
            smp: 2,
            persistent: false,
            browser: false,
            timeout: None,
            stdout: OutputMode::Inherit,
            stderr: OutputMode::Inherit,
        }
    }
}

/// A running sandboxed VM session.
///
/// Owns the QEMU process, virtiofsd daemon, and proxy server.
/// Drop will attempt to kill child processes but will not block;
/// prefer calling [`Sandbox::kill`] for clean shutdown.
pub struct Sandbox {
    session: Session,
    qemu: vm::QemuProcess,
    virtiofsd: Virtiofsd,
    _proxy: ProxyServer,
    net: NetMode,
    timeout: Option<Duration>,
    stdout: OutputMode,
    stderr: OutputMode,
}

impl Sandbox {
    /// Start a new sandbox. Returns once the guest SSH is ready.
    pub async fn start(config: SandboxConfig) -> Result<Self> {
        if !config.workspace.exists() {
            return Err(eyre!(
                "workspace directory does not exist: {}",
                config.workspace.display()
            ));
        }
        let workspace = config.workspace.canonicalize()
            .wrap_err("canonicalizing workspace path")?;

        let file_config = Config::load(Some(&workspace))?;
        let base_image = image::locate_base(config.browser, None)?;

        let mut session = Session::allocate(&base_image).await?;
        tracing::info!(session_id = %session.id, ssh_port = session.ssh_port, "session allocated");

        // Start proxy
        let proxy = ProxyServer::start(
            &file_config,
            &config.net,
            config.tls_inspect,
            &session.runtime_dir,
        ).await?;
        session.proxy_port = proxy.port;

        // SSH key
        let ssh_pubkey_path = session.ssh_key_path.with_extension("pub");
        let pubkey_str = std::fs::read_to_string(&ssh_pubkey_path)
            .wrap_err("reading SSH public key")?;
        let pubkey_str = pubkey_str.trim_end_matches('\n');

        // Cloud-init seed disk
        let cidata_path = session.runtime_dir.join("cidata.img");
        vm::cidata::create_cidata_seed(
            &session.id,
            pubkey_str,
            proxy.ca_cert_pem(),
            &cidata_path,
        )?;

        // Inject env vars into workspace
        inject_workspace_config(&workspace, &config.env_vars)?;

        // Start virtiofsd
        let virtiofs_sock = session.runtime_dir.join("virtiofs.sock");
        let virtiofsd = Virtiofsd::start(&workspace, &virtiofs_sock).await?;
        tracing::info!(pid = virtiofsd.id(), "virtiofsd started");

        // Launch QEMU
        let qemu_params = QemuParams {
            overlay_path: &session.overlay_path,
            virtiofs_sock: &virtiofs_sock,
            ssh_port: session.ssh_port,
            proxy_port: session.proxy_port,
            cidata_disk: &cidata_path,
            memory_mb: config.memory_mb,
            smp: config.smp,
            env_vars: &config.env_vars,
            silent: true,
        };

        let qemu = vm::start_qemu(&qemu_params).await?;
        if let Some(pid) = qemu.id() {
            session.record_qemu_pid(pid)?;
        }

        // Write session metadata
        std::fs::write(session.runtime_dir.join("ssh.port"), session.ssh_port.to_string())?;
        std::fs::write(session.runtime_dir.join("workspace.path"), workspace.display().to_string())?;
        std::fs::write(session.runtime_dir.join("base.path"), base_image.display().to_string())?;

        // Wait for SSH
        let ssh_timeout = Duration::from_secs(file_config.ssh_timeout() as u64);
        vm::wait_for_ssh(session.ssh_port, ssh_timeout).await?;

        Ok(Self {
            session,
            qemu,
            virtiofsd,
            _proxy: proxy,
            net: config.net,
            timeout: config.timeout,
            stdout: config.stdout,
            stderr: config.stderr,
        })
    }

    /// Session ID.
    pub fn id(&self) -> &str {
        &self.session.id
    }

    /// SSH port the guest is listening on (127.0.0.1).
    pub fn ssh_port(&self) -> u16 {
        self.session.ssh_port
    }

    /// Run a command in the guest via SSH.
    ///
    /// An empty `command` slice starts an interactive shell (typically
    /// only useful from the CLI, not programmatic use).
    pub async fn exec(&self, command: &[String]) -> Result<SessionResult> {
        let mut cmd = tokio::process::Command::new("ssh");
        cmd.args([
            "-i", self.session.ssh_key_path.to_str().unwrap(),
            "-p", &self.session.ssh_port.to_string(),
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "IdentitiesOnly=yes",
            "-o", "IdentityAgent=none",
            "-o", "LogLevel=QUIET",
            "agent@127.0.0.1",
        ]);

        // Route stdout/stderr per config
        match self.stdout {
            OutputMode::Inherit => { cmd.stdout(Stdio::inherit()); }
            OutputMode::Null => { cmd.stdout(Stdio::null()); }
        }
        match self.stderr {
            OutputMode::Inherit => { cmd.stderr(Stdio::inherit()); }
            OutputMode::Null => { cmd.stderr(Stdio::null()); }
        }

        // Mount virtiofs, source env vars, cd into workspace
        cmd.arg(concat!(
            "mountpoint -q ~/workspace 2>/dev/null || sudo -n mount -t virtiofs workspace ~/workspace;",
            " if [ -f ~/workspace/.seguro/environment ]; then set -a; . ~/workspace/.seguro/environment; set +a; fi;",
            " cd ~/workspace 2>/dev/null || true;",
        ));

        // Network isolation preamble
        cmd.arg(iptables_preamble(&self.net));

        if command.is_empty() {
            cmd.arg("exec bash -l");
        } else {
            let quoted: Vec<String> = command.iter().map(|a| shell_quote(a)).collect();
            cmd.arg(quoted.join(" "));
        }

        let start = Instant::now();

        if let Some(timeout) = self.timeout {
            match tokio::time::timeout(timeout, cmd.status()).await {
                Ok(result) => {
                    let status = result.wrap_err("executing command in guest")?;
                    Ok(SessionResult {
                        exit_code: status.code(),
                        timed_out: false,
                        duration: start.elapsed(),
                    })
                }
                Err(_) => Ok(SessionResult {
                    exit_code: None,
                    timed_out: true,
                    duration: start.elapsed(),
                }),
            }
        } else {
            let status = cmd.status().await.wrap_err("executing command in guest")?;
            Ok(SessionResult {
                exit_code: status.code(),
                timed_out: false,
                duration: start.elapsed(),
            })
        }
    }

    /// Kill the VM and all child processes. Cleans up session state.
    pub async fn kill(mut self) -> Result<()> {
        let _ = self.qemu.kill();
        let _ = self.virtiofsd.kill();
        self.session.cleanup().await
    }

    /// Wait for the QEMU process to exit (e.g. if the guest shuts itself down).
    pub async fn wait(&mut self) -> Result<std::process::ExitStatus> {
        self.qemu.wait().await
    }
}

/// Write env vars to `workspace/.seguro/` so the guest can read them via virtiofs.
fn inject_workspace_config(
    workspace: &std::path::Path,
    env_vars: &[(String, String)],
) -> Result<()> {
    let dir = workspace.join(".seguro");
    std::fs::create_dir_all(&dir).wrap_err("creating .seguro dir in workspace")?;

    if !env_vars.is_empty() {
        let content: String = env_vars
            .iter()
            .map(|(k, v)| format!("{}={}\n", k, v))
            .collect();
        std::fs::write(dir.join("environment"), content)
            .wrap_err("writing env vars to workspace")?;
    }

    Ok(())
}

/// Build the shell preamble that sets up iptables rules and proxy env vars.
fn iptables_preamble(net: &NetMode) -> String {
    match net {
        NetMode::AirGapped => {
            concat!(
                "sudo -n iptables -A OUTPUT -o lo -j ACCEPT 2>/dev/null;",
                " sudo -n iptables -A OUTPUT -p tcp --sport 22 -j ACCEPT 2>/dev/null;",
                " sudo -n iptables -A OUTPUT -j DROP 2>/dev/null;",
            ).to_string()
        }
        NetMode::ApiOnly | NetMode::FullOutbound => {
            concat!(
                "export http_proxy=http://10.0.2.100:3128;",
                " export https_proxy=http://10.0.2.100:3128;",
                " export HTTP_PROXY=http://10.0.2.100:3128;",
                " export HTTPS_PROXY=http://10.0.2.100:3128;",
                " sudo -n iptables -A OUTPUT -o lo -j ACCEPT 2>/dev/null;",
                " sudo -n iptables -A OUTPUT -p tcp --sport 22 -j ACCEPT 2>/dev/null;",
                " sudo -n iptables -A OUTPUT -d 10.0.2.100 -p tcp --dport 3128 -j ACCEPT 2>/dev/null;",
                " sudo -n iptables -A OUTPUT -p udp --dport 53 -j ACCEPT 2>/dev/null;",
                " sudo -n iptables -A OUTPUT -p tcp --dport 80 -j DROP 2>/dev/null;",
                " sudo -n iptables -A OUTPUT -p tcp --dport 443 -j DROP 2>/dev/null;",
            ).to_string()
        }
        NetMode::DevBridge => String::new(),
    }
}

/// Quote a string for safe inclusion in a remote shell command.
fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./=:@".contains(&b)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}
