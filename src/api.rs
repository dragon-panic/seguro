//! Programmatic API for managing sandboxed sessions.
//!
//! ```no_run
//! use seguro::api::{OutputMode, SandboxConfig, Sandbox};
//! use seguro::cli::NetMode;
//!
//! # async fn example() -> color_eyre::eyre::Result<()> {
//! let config = SandboxConfig {
//!     workspace: "/home/user/project".into(),
//!     env_vars: vec![("ANTHROPIC_API_KEY".into(), "sk-...".into())],
//!     stdout: OutputMode::Capture,
//!     stderr: OutputMode::Capture,
//!     ..Default::default()
//! };
//!
//! let sandbox = Sandbox::start(config).await?;
//! let result = sandbox.exec(&["echo".into(), "hello".into()]).await?;
//! assert_eq!(result.stdout.as_deref(), Some(b"hello\n".as_slice()));
//! sandbox.kill().await?;
//! # Ok(())
//! # }
//! ```

use std::path::PathBuf;
use std::io::IsTerminal;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr, eyre};
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::time::Instant;

/// Controls where guest command I/O is routed.
#[derive(Debug, Clone, Default)]
pub enum OutputMode {
    /// Inherit parent's stdout/stderr (default).
    #[default]
    Inherit,
    /// Discard all output.
    Null,
    /// Collect full output into `Vec<u8>`, returned in `SessionResult`.
    Capture,
    /// Forward output chunks in real-time through a channel.
    Stream(mpsc::Sender<OutputChunk>),
}

/// A chunk of output from a guest command (used with `OutputMode::Stream`).
#[derive(Debug, Clone)]
pub enum OutputChunk {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
}

/// How to handle unexpected QEMU process exits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RestartStrategy {
    /// Never restart (default). Current behavior.
    #[default]
    Never,
    /// Restart only on non-zero / signal exit.
    OnFailure,
    /// Always restart, even on clean exit.
    Always,
}

/// Controls automatic restart of the QEMU process on crash.
#[derive(Debug, Clone)]
pub struct RestartPolicy {
    pub strategy: RestartStrategy,
    /// Maximum number of restarts before giving up.
    pub max_restarts: u32,
    /// Backoff durations between restarts. Cycles through the list,
    /// staying on the last value once exhausted.
    /// Default: [1s, 5s, 15s].
    pub backoff: Vec<Duration>,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            strategy: RestartStrategy::Never,
            max_restarts: 3,
            backoff: vec![
                Duration::from_secs(1),
                Duration::from_secs(5),
                Duration::from_secs(15),
            ],
        }
    }
}

/// Persona configuration loaded from a TOML file.
///
/// Defines the identity and constraints for an agent running in the sandbox.
/// Ox passes this to configure each agent's behavior.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PersonaConfig {
    /// System prompt written to `workspace/.claude/CLAUDE.md`.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Role label (informational — included as env var `SEGURO_ROLE`).
    #[serde(default)]
    pub role: Option<String>,
    /// Environment variables injected into the guest.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

use crate::cli::NetMode;
use crate::config::Config;
use crate::proxy::ProxyServer;
use crate::session::{Session, image};
use crate::vm::{self, QemuParams};
use crate::vm::virtiofsd::Virtiofsd;

/// Persistent session metadata written to `runtime_dir/session.json`.
///
/// Contains everything needed to reconnect to or restart a session.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionMeta {
    pub session_id: String,
    pub ssh_port: u16,
    pub proxy_port: u16,
    pub overlay_path: PathBuf,
    pub ssh_key_path: PathBuf,
    pub workspace: PathBuf,
    pub base_image: PathBuf,
    pub memory_mb: u32,
    pub smp: u32,
    pub env_vars: Vec<(String, String)>,
    pub net: String,
    pub profile: Option<String>,
}

/// Result of executing a command in the sandbox.
#[derive(Debug)]
pub struct SessionResult {
    /// Process exit code. None if the process was killed or timed out.
    pub exit_code: Option<i32>,
    /// Whether the session was terminated due to timeout.
    pub timed_out: bool,
    /// Wall-clock duration of the exec() call.
    pub duration: Duration,
    /// Captured stdout bytes (populated only with `OutputMode::Capture`).
    pub stdout: Option<Vec<u8>>,
    /// Captured stderr bytes (populated only with `OutputMode::Capture`).
    pub stderr: Option<Vec<u8>>,
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
    /// Profile env vars are merged first; these take priority.
    pub env_vars: Vec<(String, String)>,
    /// Guest RAM in MB. If set, overrides the profile default.
    pub memory_mb: Option<u32>,
    /// Guest vCPU count. If set, overrides the profile default.
    pub smp: Option<u32>,
    /// Keep session overlay and workspace after exit.
    pub persistent: bool,
    /// VM profile name. Resolves image, RAM, CPU, env from config.
    /// None means "default".
    pub profile: Option<String>,
    /// Kill the session after this duration. None means no timeout.
    pub timeout: Option<Duration>,
    /// Where to route guest command stdout.
    pub stdout: OutputMode,
    /// Where to route guest command stderr.
    pub stderr: OutputMode,
    /// Restart policy for QEMU crashes. Default: never restart.
    pub restart_policy: RestartPolicy,
    /// Path to a persona TOML file on the host. When set, the system prompt
    /// is written to `workspace/.claude/CLAUDE.md` and persona env vars are
    /// merged into the guest environment.
    pub persona_config: Option<PathBuf>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            workspace: PathBuf::new(),
            net: NetMode::FullOutbound,
            tls_inspect: false,
            env_vars: Vec::new(),
            memory_mb: None,
            smp: None,
            persistent: false,
            profile: None,
            timeout: None,
            stdout: OutputMode::Inherit,
            stderr: OutputMode::Inherit,
            restart_policy: RestartPolicy::default(),
            persona_config: None,
        }
    }
}

/// Stored parameters needed to re-launch QEMU on restart.
struct QemuLaunchParams {
    overlay_path: PathBuf,
    virtiofs_sock: PathBuf,
    ssh_port: u16,
    proxy_port: u16,
    cidata_disk: PathBuf,
    memory_mb: u32,
    smp: u32,
    env_vars: Vec<(String, String)>,
    ssh_timeout: Duration,
}

/// A running sandboxed VM session.
///
/// Owns the QEMU process, virtiofsd daemon, and proxy server.
/// Drop will attempt to kill child processes but will not block;
/// prefer calling [`Sandbox::kill`] for clean shutdown.
pub struct Sandbox {
    session: Session,
    qemu: Arc<Mutex<vm::QemuProcess>>,
    virtiofsd: Virtiofsd,
    _proxy: ProxyServer,
    net: NetMode,
    timeout: Option<Duration>,
    stdout: OutputMode,
    stderr: OutputMode,
    /// Signals the monitor task to stop.
    shutdown: Arc<Notify>,
    /// Handle to the background monitor task (if restart policy != Never).
    _monitor: Option<tokio::task::JoinHandle<()>>,
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

        // Resolve profile → image, memory, smp, env
        let profile_name = config.profile.as_deref().unwrap_or("default");
        let profile = file_config.profile(profile_name);
        let base_image = image::locate_base(profile.image_suffix.as_deref(), None)?;
        let memory_mb = config.memory_mb.unwrap_or_else(|| profile.memory_mb.unwrap_or(2048));
        let smp = config.smp.unwrap_or_else(|| profile.smp.unwrap_or(2));

        // Merge env: profile env first, then explicit env_vars override
        let mut env_vars: Vec<(String, String)> = profile.env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (k, v) in &config.env_vars {
            if let Some(existing) = env_vars.iter_mut().find(|(ek, _)| ek == k) {
                existing.1 = v.clone();
            } else {
                env_vars.push((k.clone(), v.clone()));
            }
        }

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

        // Load and apply persona config if provided
        let persona = if let Some(ref persona_path) = config.persona_config {
            let persona = load_persona(persona_path)?;
            // Merge persona env vars (profile env < persona env < explicit env_vars)
            // Insert persona env before the explicit overrides
            for (k, v) in &persona.env {
                if !env_vars.iter().any(|(ek, _)| ek == k) {
                    env_vars.push((k.clone(), v.clone()));
                }
            }
            if let Some(ref role) = persona.role {
                if !env_vars.iter().any(|(k, _)| k == "SEGURO_ROLE") {
                    env_vars.push(("SEGURO_ROLE".into(), role.clone()));
                }
            }
            Some(persona)
        } else {
            None
        };

        // Inject env vars into workspace
        inject_workspace_config(&workspace, &env_vars, persona.as_ref())?;

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
            memory_mb,
            smp,
            env_vars: &env_vars,
            silent: true,
        };

        let qemu = vm::start_qemu(&qemu_params).await?;
        if let Some(pid) = qemu.id() {
            session.record_qemu_pid(pid)?;
        }

        // Write session metadata
        let meta = SessionMeta {
            session_id: session.id.clone(),
            ssh_port: session.ssh_port,
            proxy_port: session.proxy_port,
            overlay_path: session.overlay_path.clone(),
            ssh_key_path: session.ssh_key_path.clone(),
            workspace: workspace.clone(),
            base_image: base_image.clone(),
            memory_mb,
            smp,
            env_vars: env_vars.clone(),
            net: net_mode_to_str(&config.net),
            profile: config.profile.clone(),
        };
        let meta_json = serde_json::to_string_pretty(&meta)
            .wrap_err("serializing session metadata")?;
        std::fs::write(session.runtime_dir.join("session.json"), meta_json)
            .wrap_err("writing session.json")?;

        // Wait for SSH
        let ssh_timeout = Duration::from_secs(file_config.ssh_timeout() as u64);
        vm::wait_for_ssh(session.ssh_port, ssh_timeout).await?;

        let qemu = Arc::new(Mutex::new(qemu));
        let shutdown = Arc::new(Notify::new());

        // Spawn crash monitor if restart policy is not Never
        let monitor = if config.restart_policy.strategy != RestartStrategy::Never {
            let launch_params = QemuLaunchParams {
                overlay_path: session.overlay_path.clone(),
                virtiofs_sock,
                ssh_port: session.ssh_port,
                proxy_port: session.proxy_port,
                cidata_disk: cidata_path,
                memory_mb,
                smp,
                env_vars,
                ssh_timeout,
            };
            let handle = spawn_crash_monitor(
                Arc::clone(&qemu),
                config.restart_policy.clone(),
                launch_params,
                Arc::clone(&shutdown),
            );
            Some(handle)
        } else {
            None
        };

        Ok(Self {
            session,
            qemu,
            virtiofsd,
            _proxy: proxy,
            net: config.net,
            timeout: config.timeout,
            stdout: config.stdout,
            stderr: config.stderr,
            shutdown,
            _monitor: monitor,
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
        self.exec_with(command, &self.stdout, &self.stderr).await
    }

    /// Run a command with explicit output modes (overrides sandbox defaults).
    ///
    /// Use this when you need per-call control, e.g. `Stream` with a fresh
    /// channel for each exec.
    pub async fn exec_with(
        &self,
        command: &[String],
        stdout_mode: &OutputMode,
        stderr_mode: &OutputMode,
    ) -> Result<SessionResult> {
        let interactive = command.is_empty();
        let capturing = matches!(stdout_mode, OutputMode::Capture | OutputMode::Stream(_))
            || matches!(stderr_mode, OutputMode::Capture | OutputMode::Stream(_));

        let mut cmd = tokio::process::Command::new("ssh");
        cmd.args([
            "-i", self.session.ssh_key_path.to_str().unwrap(),
            "-p", &self.session.ssh_port.to_string(),
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "IdentitiesOnly=yes",
            "-o", "IdentityAgent=none",
            "-o", "LogLevel=QUIET",
        ]);
        // PTY merges stdout+stderr, so skip it when capturing/streaming.
        if !capturing {
            if interactive {
                cmd.arg("-tt");
            } else if std::io::stdout().is_terminal() {
                cmd.arg("-t");
            }
        }
        cmd.arg("agent@127.0.0.1");

        // Route stdout per mode
        match stdout_mode {
            OutputMode::Inherit => { cmd.stdout(Stdio::inherit()); }
            OutputMode::Null => { cmd.stdout(Stdio::null()); }
            OutputMode::Capture | OutputMode::Stream(_) => { cmd.stdout(Stdio::piped()); }
        }
        // Route stderr per mode
        match stderr_mode {
            OutputMode::Inherit => { cmd.stderr(Stdio::inherit()); }
            OutputMode::Null => { cmd.stderr(Stdio::null()); }
            OutputMode::Capture | OutputMode::Stream(_) => { cmd.stderr(Stdio::piped()); }
        }

        // Mount virtiofs, source env vars, inject credentials, cd into workspace
        cmd.arg(concat!(
            "mountpoint -q ~/workspace 2>/dev/null || sudo -n mount -t virtiofs workspace ~/workspace;",
            " if [ -f ~/workspace/.seguro/environment ]; then set -a; . ~/workspace/.seguro/environment; set +a; fi;",
            " if [ -f ~/workspace/.seguro/.credentials.json ]; then mkdir -p ~/.claude && cp ~/workspace/.seguro/.credentials.json ~/.claude/.credentials.json && rm ~/workspace/.seguro/.credentials.json; fi;",
            " cd ~/workspace 2>/dev/null || true;",
        ));

        // Network isolation preamble
        cmd.arg(iptables_preamble(&self.net));

        if interactive {
            cmd.arg("exec bash -l");
        } else {
            let quoted: Vec<String> = command.iter().map(|a| shell_quote(a)).collect();
            cmd.arg(quoted.join(" "));
        }

        let start = Instant::now();

        if capturing {
            self.exec_capturing(cmd, stdout_mode, stderr_mode, start).await
        } else {
            self.exec_simple(cmd, start).await
        }
    }

    /// Execute with Inherit/Null modes — uses `cmd.status()`.
    async fn exec_simple(
        &self,
        mut cmd: tokio::process::Command,
        start: Instant,
    ) -> Result<SessionResult> {
        if let Some(timeout) = self.timeout {
            match tokio::time::timeout(timeout, cmd.status()).await {
                Ok(result) => {
                    let status = result.wrap_err("executing command in guest")?;
                    Ok(SessionResult {
                        exit_code: status.code(),
                        timed_out: false,
                        duration: start.elapsed(),
                        stdout: None,
                        stderr: None,
                    })
                }
                Err(_) => Ok(SessionResult {
                    exit_code: None,
                    timed_out: true,
                    duration: start.elapsed(),
                    stdout: None,
                    stderr: None,
                }),
            }
        } else {
            let status = cmd.status().await.wrap_err("executing command in guest")?;
            Ok(SessionResult {
                exit_code: status.code(),
                timed_out: false,
                duration: start.elapsed(),
                stdout: None,
                stderr: None,
            })
        }
    }

    /// Execute with Capture/Stream modes — spawns child and reads handles.
    async fn exec_capturing(
        &self,
        mut cmd: tokio::process::Command,
        stdout_mode: &OutputMode,
        stderr_mode: &OutputMode,
        start: Instant,
    ) -> Result<SessionResult> {
        let mut child = cmd.spawn().wrap_err("spawning SSH command")?;

        // Take the handles before spawning read tasks
        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        let stdout_fut = spawn_output_reader(stdout_handle, stdout_mode);
        let stderr_fut = spawn_stderr_reader(stderr_handle, stderr_mode);

        let run_fut = async {
            let (status, stdout_bytes, stderr_bytes) =
                tokio::try_join!(
                    async { child.wait().await.wrap_err("waiting for guest command") },
                    stdout_fut,
                    stderr_fut,
                )?;
            Ok::<_, color_eyre::eyre::Report>(SessionResult {
                exit_code: status.code(),
                timed_out: false,
                duration: start.elapsed(),
                stdout: stdout_bytes,
                stderr: stderr_bytes,
            })
        };

        if let Some(timeout) = self.timeout {
            match tokio::time::timeout(timeout, run_fut).await {
                Ok(result) => result,
                Err(_) => {
                    let _ = child.kill().await;
                    Ok(SessionResult {
                        exit_code: None,
                        timed_out: true,
                        duration: start.elapsed(),
                        stdout: None,
                        stderr: None,
                    })
                }
            }
        } else {
            run_fut.await
        }
    }

    /// Kill the VM and all child processes. Cleans up session state.
    pub async fn kill(mut self) -> Result<()> {
        // Signal the monitor to stop before killing QEMU
        self.shutdown.notify_waiters();
        {
            let mut qemu = self.qemu.lock().await;
            let _ = qemu.kill();
        }
        let _ = self.virtiofsd.kill();
        self.session.cleanup().await
    }

    /// Wait for the QEMU process to exit (e.g. if the guest shuts itself down).
    pub async fn wait(&self) -> Result<std::process::ExitStatus> {
        let mut qemu = self.qemu.lock().await;
        qemu.wait().await
    }

    /// Recover an existing session by ID.
    ///
    /// Reads `session.json` from the runtime directory, verifies the QEMU
    /// process is still running, restarts the proxy and virtiofsd, and
    /// returns a connected `Sandbox`.
    pub async fn recover(session_id: &str) -> Result<Self> {
        let runtime_dir = crate::config::runtime_dir().join(session_id);
        if !runtime_dir.exists() {
            return Err(eyre!("no session found: {}", session_id));
        }

        let meta_path = runtime_dir.join("session.json");
        let meta_str = std::fs::read_to_string(&meta_path)
            .wrap_err_with(|| format!("reading {}", meta_path.display()))?;
        let meta: SessionMeta = serde_json::from_str(&meta_str)
            .wrap_err("parsing session.json")?;

        let net = str_to_net_mode(&meta.net)?;
        let file_config = Config::load(Some(&meta.workspace))?;
        let ssh_timeout = Duration::from_secs(file_config.ssh_timeout() as u64);

        // Re-inject workspace config (env vars, credentials)
        inject_workspace_config(&meta.workspace, &meta.env_vars, None)?;

        // Start proxy
        let proxy = ProxyServer::start(
            &file_config,
            &net,
            false, // TLS inspect not persisted — could be added later
            &runtime_dir,
        ).await?;

        // Start virtiofsd
        let virtiofs_sock = runtime_dir.join("virtiofs.sock");
        let virtiofsd = Virtiofsd::start(&meta.workspace, &virtiofs_sock).await?;

        // Check if QEMU is still running, otherwise re-launch
        let pid_file = runtime_dir.join("qemu.pid");
        let qemu = if crate::session::image::is_qemu_running(&pid_file) {
            // QEMU is alive — we can't take ownership of an existing process
            // via tokio::process::Child, so re-launch with the same overlay
            tracing::info!("existing QEMU process found, re-launching for ownership");
            // Kill the old one first
            if let Ok(content) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = content.trim().parse::<i32>() {
                    let _ = nix::sys::signal::kill(
                        nix::unistd::Pid::from_raw(pid),
                        nix::sys::signal::Signal::SIGTERM,
                    );
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
            let params = QemuParams {
                overlay_path: &meta.overlay_path,
                virtiofs_sock: &virtiofs_sock,
                ssh_port: meta.ssh_port,
                proxy_port: proxy.port,
                cidata_disk: &runtime_dir.join("cidata.img"),
                memory_mb: meta.memory_mb,
                smp: meta.smp,
                env_vars: &meta.env_vars,
                silent: true,
            };
            vm::start_qemu(&params).await?
        } else {
            // QEMU is dead — re-launch
            let params = QemuParams {
                overlay_path: &meta.overlay_path,
                virtiofs_sock: &virtiofs_sock,
                ssh_port: meta.ssh_port,
                proxy_port: proxy.port,
                cidata_disk: &runtime_dir.join("cidata.img"),
                memory_mb: meta.memory_mb,
                smp: meta.smp,
                env_vars: &meta.env_vars,
                silent: true,
            };
            vm::start_qemu(&params).await?
        };

        if let Some(pid) = qemu.id() {
            std::fs::write(runtime_dir.join("qemu.pid"), pid.to_string())
                .wrap_err("updating qemu.pid")?;
        }

        // Wait for SSH
        vm::wait_for_ssh(meta.ssh_port, ssh_timeout).await?;

        let session = Session {
            id: meta.session_id,
            ssh_port: meta.ssh_port,
            proxy_port: proxy.port,
            ssh_key_path: meta.ssh_key_path,
            overlay_path: meta.overlay_path,
            runtime_dir,
            qemu_pid: qemu.id(),
        };

        Ok(Self {
            session,
            qemu: Arc::new(Mutex::new(qemu)),
            virtiofsd,
            _proxy: proxy,
            net,
            timeout: None,
            stdout: OutputMode::Inherit,
            stderr: OutputMode::Inherit,
            shutdown: Arc::new(Notify::new()),
            _monitor: None,
        })
    }

    /// Read the persisted session metadata.
    pub fn meta(&self) -> Result<SessionMeta> {
        let path = self.session.runtime_dir.join("session.json");
        let content = std::fs::read_to_string(&path)
            .wrap_err("reading session.json")?;
        serde_json::from_str(&content).wrap_err("parsing session.json")
    }
}

/// Background task that monitors the QEMU process and restarts it on crash.
fn spawn_crash_monitor(
    qemu: Arc<Mutex<vm::QemuProcess>>,
    policy: RestartPolicy,
    params: QemuLaunchParams,
    shutdown: Arc<Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut restart_count: u32 = 0;

        loop {
            // Wait for QEMU to exit or shutdown signal
            let exit_status = {
                let mut qemu = qemu.lock().await;
                tokio::select! {
                    status = qemu.wait() => {
                        match status {
                            Ok(s) => s,
                            Err(e) => {
                                tracing::error!("error waiting for QEMU: {e}");
                                return;
                            }
                        }
                    }
                    _ = shutdown.notified() => {
                        tracing::debug!("crash monitor: shutdown signal received");
                        return;
                    }
                }
            };

            // Check if we should restart
            let should_restart = match policy.strategy {
                RestartStrategy::Never => false,
                RestartStrategy::Always => true,
                RestartStrategy::OnFailure => !exit_status.success(),
            };

            if !should_restart {
                tracing::info!(
                    code = ?exit_status.code(),
                    "QEMU exited, restart not needed (strategy={:?})",
                    policy.strategy
                );
                return;
            }

            if restart_count >= policy.max_restarts {
                tracing::error!(
                    count = restart_count,
                    max = policy.max_restarts,
                    "QEMU crashed but max restarts exceeded — giving up"
                );
                return;
            }

            // Backoff
            let delay_idx = (restart_count as usize).min(policy.backoff.len().saturating_sub(1));
            let delay = policy.backoff.get(delay_idx).copied().unwrap_or(Duration::from_secs(1));
            tracing::warn!(
                code = ?exit_status.code(),
                restart = restart_count + 1,
                max = policy.max_restarts,
                delay_ms = delay.as_millis() as u64,
                "QEMU crashed — restarting after backoff"
            );

            // Wait for backoff or shutdown
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = shutdown.notified() => {
                    tracing::debug!("crash monitor: shutdown during backoff");
                    return;
                }
            }

            // Re-launch QEMU with the same parameters
            let qemu_params = QemuParams {
                overlay_path: &params.overlay_path,
                virtiofs_sock: &params.virtiofs_sock,
                ssh_port: params.ssh_port,
                proxy_port: params.proxy_port,
                cidata_disk: &params.cidata_disk,
                memory_mb: params.memory_mb,
                smp: params.smp,
                env_vars: &params.env_vars,
                silent: true,
            };

            match vm::start_qemu(&qemu_params).await {
                Ok(new_qemu) => {
                    tracing::info!(pid = new_qemu.id(), "QEMU restarted successfully");

                    // Wait for SSH to become available
                    if let Err(e) = vm::wait_for_ssh(params.ssh_port, params.ssh_timeout).await {
                        tracing::error!("SSH not available after restart: {e}");
                        // Store the new process anyway so kill() can clean it up
                        *qemu.lock().await = new_qemu;
                        return;
                    }

                    *qemu.lock().await = new_qemu;
                    restart_count += 1;
                    tracing::info!(
                        restart = restart_count,
                        "QEMU restarted and SSH ready"
                    );
                }
                Err(e) => {
                    tracing::error!("failed to restart QEMU: {e}");
                    return;
                }
            }
        }
    })
}

/// Read from an output handle according to the mode.
///
/// - `Capture`: reads all bytes into a `Vec<u8>` and returns `Some(bytes)`.
/// - `Stream(sender)`: reads in chunks and sends them through the channel; returns `None`.
/// - `Inherit`/`Null`: handle should be `None`; returns `None`.
async fn spawn_output_reader(
    handle: Option<tokio::process::ChildStdout>,
    mode: &OutputMode,
) -> Result<Option<Vec<u8>>> {
    // This function is generic over ChildStdout; we use a separate one for stderr below.
    use tokio::io::AsyncReadExt;

    let Some(mut reader) = handle else {
        return Ok(None);
    };

    match mode {
        OutputMode::Capture => {
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf).await.wrap_err("reading captured output")?;
            Ok(Some(buf))
        }
        OutputMode::Stream(sender) => {
            let mut buf = [0u8; 8192];
            loop {
                let n = reader.read(&mut buf).await.wrap_err("reading stream output")?;
                if n == 0 {
                    break;
                }
                // Best-effort send; if receiver dropped, just stop.
                if sender.send(OutputChunk::Stdout(buf[..n].to_vec())).await.is_err() {
                    break;
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// Same as `spawn_output_reader` but for stderr handles (sends `OutputChunk::Stderr`).
async fn spawn_stderr_reader(
    handle: Option<tokio::process::ChildStderr>,
    mode: &OutputMode,
) -> Result<Option<Vec<u8>>> {
    use tokio::io::AsyncReadExt;

    let Some(mut reader) = handle else {
        return Ok(None);
    };

    match mode {
        OutputMode::Capture => {
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf).await.wrap_err("reading captured stderr")?;
            Ok(Some(buf))
        }
        OutputMode::Stream(sender) => {
            let mut buf = [0u8; 8192];
            loop {
                let n = reader.read(&mut buf).await.wrap_err("reading stream stderr")?;
                if n == 0 {
                    break;
                }
                if sender.send(OutputChunk::Stderr(buf[..n].to_vec())).await.is_err() {
                    break;
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// Load persona config from a TOML file.
fn load_persona(path: &std::path::Path) -> Result<PersonaConfig> {
    let content = std::fs::read_to_string(path)
        .wrap_err_with(|| format!("reading persona config: {}", path.display()))?;
    toml::from_str(&content)
        .wrap_err_with(|| format!("parsing persona TOML: {}", path.display()))
}

/// Write env vars, credentials, and persona to `workspace/.seguro/` so the guest can read them via virtiofs.
fn inject_workspace_config(
    workspace: &std::path::Path,
    env_vars: &[(String, String)],
    persona: Option<&PersonaConfig>,
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

    // Inject Claude Code credentials if available on the host.
    // The guest preamble moves this to ~/.claude/ and deletes it from the workspace.
    if let Some(home) = dirs::home_dir() {
        let creds = home.join(".claude/.credentials.json");
        if creds.exists() {
            std::fs::copy(&creds, dir.join(".credentials.json"))
                .wrap_err("copying Claude credentials to workspace")?;
        }
    }

    // Inject persona system prompt as CLAUDE.md so Claude Code picks it up.
    if let Some(persona) = persona {
        if let Some(ref prompt) = persona.system_prompt {
            let claude_dir = workspace.join(".claude");
            std::fs::create_dir_all(&claude_dir)
                .wrap_err("creating .claude dir in workspace")?;
            std::fs::write(claude_dir.join("CLAUDE.md"), prompt)
                .wrap_err("writing persona system prompt to CLAUDE.md")?;
        }
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
        NetMode::ApiOnly => {
            // Force all HTTP/S through proxy — it's the enforcement point for allow-list
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
        NetMode::FullOutbound => {
            // No proxy enforcement — allow direct outbound connections.
            // The proxy is still running and available at 10.0.2.100:3128 for tools
            // that opt in, but we don't force it via env vars because many tools
            // (Claude Code, Node.js SDK) hang when proxy env vars are set.
            String::new()
        }
        NetMode::DevBridge => String::new(),
    }
}

fn net_mode_to_str(mode: &NetMode) -> String {
    match mode {
        NetMode::AirGapped => "air-gapped".into(),
        NetMode::ApiOnly => "api-only".into(),
        NetMode::FullOutbound => "full-outbound".into(),
        NetMode::DevBridge => "dev-bridge".into(),
    }
}

fn str_to_net_mode(s: &str) -> Result<NetMode> {
    match s {
        "air-gapped" => Ok(NetMode::AirGapped),
        "api-only" => Ok(NetMode::ApiOnly),
        "full-outbound" => Ok(NetMode::FullOutbound),
        "dev-bridge" => Ok(NetMode::DevBridge),
        _ => Err(eyre!("unknown net mode in session.json: {}", s)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_persona_toml() {
        let toml = r#"
system_prompt = "You are a code reviewer. Be thorough."
role = "reviewer"

[env]
AGENT_ROLE = "reviewer"
MAX_FILES = "10"
"#;
        let persona: PersonaConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            persona.system_prompt.as_deref(),
            Some("You are a code reviewer. Be thorough.")
        );
        assert_eq!(persona.role.as_deref(), Some("reviewer"));
        assert_eq!(persona.env.get("AGENT_ROLE").unwrap(), "reviewer");
        assert_eq!(persona.env.get("MAX_FILES").unwrap(), "10");
    }

    #[test]
    fn parse_minimal_persona_toml() {
        let toml = r#"
system_prompt = "Just a prompt."
"#;
        let persona: PersonaConfig = toml::from_str(toml).unwrap();
        assert_eq!(persona.system_prompt.as_deref(), Some("Just a prompt."));
        assert!(persona.role.is_none());
        assert!(persona.env.is_empty());
    }

    #[test]
    fn inject_persona_writes_claude_md() {
        let tmp = tempfile::tempdir().unwrap();
        let persona = PersonaConfig {
            system_prompt: Some("You are a security auditor.".into()),
            role: Some("auditor".into()),
            env: std::collections::HashMap::new(),
        };

        inject_workspace_config(tmp.path(), &[], Some(&persona)).unwrap();

        let claude_md = tmp.path().join(".claude/CLAUDE.md");
        assert!(claude_md.exists());
        let content = std::fs::read_to_string(&claude_md).unwrap();
        assert_eq!(content, "You are a security auditor.");
    }

    #[test]
    fn inject_no_persona_skips_claude_md() {
        let tmp = tempfile::tempdir().unwrap();
        inject_workspace_config(tmp.path(), &[], None).unwrap();

        let claude_md = tmp.path().join(".claude/CLAUDE.md");
        assert!(!claude_md.exists());
    }

    #[test]
    fn restart_policy_default_is_never() {
        let policy = RestartPolicy::default();
        assert_eq!(policy.strategy, RestartStrategy::Never);
        assert_eq!(policy.max_restarts, 3);
        assert_eq!(policy.backoff.len(), 3);
    }

    #[test]
    fn session_meta_roundtrip() {
        let meta = SessionMeta {
            session_id: "test-123".into(),
            ssh_port: 2222,
            proxy_port: 3128,
            overlay_path: "/tmp/overlay.qcow2".into(),
            ssh_key_path: "/tmp/id_ed25519".into(),
            workspace: "/home/user/project".into(),
            base_image: "/home/user/.local/share/seguro/images/base.qcow2".into(),
            memory_mb: 2048,
            smp: 2,
            env_vars: vec![("KEY".into(), "value".into())],
            net: "full-outbound".into(),
            profile: Some("default".into()),
        };

        let json = serde_json::to_string_pretty(&meta).unwrap();
        let parsed: SessionMeta = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.session_id, "test-123");
        assert_eq!(parsed.ssh_port, 2222);
        assert_eq!(parsed.memory_mb, 2048);
        assert_eq!(parsed.env_vars, vec![("KEY".into(), "value".into())]);
        assert_eq!(parsed.net, "full-outbound");
    }

    #[test]
    fn net_mode_str_roundtrip() {
        for (mode, s) in [
            (NetMode::AirGapped, "air-gapped"),
            (NetMode::ApiOnly, "api-only"),
            (NetMode::FullOutbound, "full-outbound"),
            (NetMode::DevBridge, "dev-bridge"),
        ] {
            assert_eq!(net_mode_to_str(&mode), s);
            assert!(matches!(str_to_net_mode(s), Ok(_)));
        }
        assert!(str_to_net_mode("invalid").is_err());
    }
}
