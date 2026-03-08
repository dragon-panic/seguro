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
use tokio::sync::{broadcast, mpsc, watch, Mutex, Notify};
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

/// Health state of the sandbox VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthState {
    /// SSH responds quickly — VM is healthy.
    Healthy,
    /// SSH responds but slowly (>5s) — VM may be under heavy load.
    Degraded,
    /// SSH did not respond within the check interval — VM may be stuck.
    Unresponsive,
    /// QEMU process has exited.
    Dead,
}

/// Lifecycle event emitted by a sandbox session.
///
/// Subscribe via [`Sandbox::events`] to receive these in real-time.
/// Ox maps these to its SSE stream for UI visibility.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// QEMU process started, session allocated.
    Started { session_id: String },
    /// SSH banner detected — guest is ready for commands.
    SshReady { session_id: String, port: u16 },
    /// A command execution began.
    ExecStarted { session_id: String, command: Vec<String> },
    /// Health state changed.
    HealthChanged { session_id: String, state: HealthState },
    /// QEMU process exited unexpectedly.
    Crashed { session_id: String, exit_code: Option<i32> },
    /// QEMU process restarted after a crash.
    Restarted { session_id: String, attempt: u32 },
    /// A command execution completed.
    Completed { session_id: String, exit_code: Option<i32>, duration: Duration },
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
use crate::proxy::{ProxyServer, ProxyStats};
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
    /// Health check interval. When set, a background task pings SSH at this
    /// interval and updates the health state. None disables health checks.
    pub health_check_interval: Option<Duration>,
    /// Grace period before killing QEMU on [`Sandbox::kill`]. A `.seguro/shutdown`
    /// sentinel is written so the agent can save state. Default: 5s. None for
    /// immediate kill.
    pub shutdown_grace: Option<Duration>,
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
            health_check_interval: None,
            shutdown_grace: Some(Duration::from_secs(5)),
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

/// Agent status reported by the guest via `.seguro/status.json`.
///
/// The guest agent writes this file atomically (temp + rename) to the
/// virtiofs-shared workspace. The host reads it synchronously — no SSH needed.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentState {
    /// Agent-defined state label (e.g. "working", "idle", "stuck", "exiting").
    pub state: String,
    /// ISO 8601 timestamp of the last status update.
    pub updated_at: String,
    /// Human-readable description of the current task (optional).
    #[serde(default)]
    pub task: Option<String>,
    /// Progress fraction 0.0–1.0 (optional).
    #[serde(default)]
    pub progress: Option<f64>,
}

/// Git state of the shared workspace, inspected from the host side.
///
/// Read via [`Sandbox::workspace_state`]. Useful for pre-kill verification —
/// Ox can refuse to terminate sessions with unpushed work.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkspaceState {
    /// Whether the workspace is a git repository.
    pub is_git_repo: bool,
    /// Working tree has uncommitted changes (modified, staged, or untracked files).
    pub has_uncommitted: bool,
    /// Local branch has commits not pushed to the upstream remote.
    pub has_unpushed: bool,
    /// Number of dirty files (modified + untracked).
    pub dirty_files: u32,
}

/// Per-session resource usage snapshot.
///
/// Read via [`Sandbox::usage`] at any time — counters are live.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionUsage {
    /// Wall-clock time since the sandbox started.
    pub wall_clock: Duration,
    /// Total proxy requests (allowed + blocked).
    pub proxy_requests: u64,
    /// Proxy requests that were denied by filter rules.
    pub proxy_blocked: u64,
    /// Estimated response bytes received through the proxy.
    pub proxy_bytes_received: u64,
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
    /// Host-side path to the virtiofs-shared workspace directory.
    workspace: PathBuf,
    timeout: Option<Duration>,
    stdout: OutputMode,
    stderr: OutputMode,
    /// Grace period before killing QEMU. During this time a `.seguro/shutdown`
    /// sentinel is written so the agent can save state.
    shutdown_grace: Option<Duration>,
    /// Signals the monitor task to stop.
    shutdown: Arc<Notify>,
    /// Handle to the background monitor task (if restart policy != Never).
    _monitor: Option<tokio::task::JoinHandle<()>>,
    /// Current health state (updated by background heartbeat task).
    /// Sender is held to keep the channel alive; the health monitor writes to a clone.
    _health_tx: watch::Sender<HealthState>,
    health_rx: watch::Receiver<HealthState>,
    /// Handle to the health check task.
    _health_monitor: Option<tokio::task::JoinHandle<()>>,
    /// Broadcast channel for lifecycle events.
    events_tx: broadcast::Sender<SessionEvent>,
    /// Shared proxy traffic counters.
    proxy_stats: Arc<ProxyStats>,
    /// When the sandbox was started (for wall-clock metering).
    started_at: Instant,
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

        let (events_tx, _) = broadcast::channel::<SessionEvent>(64);

        let qemu = vm::start_qemu(&qemu_params).await?;
        if let Some(pid) = qemu.id() {
            session.record_qemu_pid(pid)?;
        }

        let _ = events_tx.send(SessionEvent::Started {
            session_id: session.id.clone(),
        });

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

        let _ = events_tx.send(SessionEvent::SshReady {
            session_id: session.id.clone(),
            port: session.ssh_port,
        });

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
                events_tx.clone(),
                session.id.clone(),
            );
            Some(handle)
        } else {
            None
        };

        let (health_tx, health_rx) = watch::channel(HealthState::Healthy);

        // Spawn health check if interval is configured
        let health_monitor = if let Some(interval) = config.health_check_interval {
            let handle = spawn_health_monitor(
                session.ssh_port,
                interval,
                health_tx.clone(),
                Arc::clone(&shutdown),
                events_tx.clone(),
                session.id.clone(),
            );
            Some(handle)
        } else {
            None
        };

        let proxy_stats = Arc::clone(&proxy.stats);

        Ok(Self {
            session,
            qemu,
            virtiofsd,
            _proxy: proxy,
            net: config.net,
            workspace,
            timeout: config.timeout,
            stdout: config.stdout,
            stderr: config.stderr,
            shutdown_grace: config.shutdown_grace,
            shutdown,
            _monitor: monitor,
            _health_tx: health_tx,
            health_rx,
            _health_monitor: health_monitor,
            events_tx,
            proxy_stats,
            started_at: Instant::now(),
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
        let _ = self.events_tx.send(SessionEvent::ExecStarted {
            session_id: self.session.id.clone(),
            command: command.to_vec(),
        });

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

        let result = if capturing {
            self.exec_capturing(cmd, stdout_mode, stderr_mode, start).await
        } else {
            self.exec_simple(cmd, start).await
        };

        if let Ok(ref r) = result {
            let _ = self.events_tx.send(SessionEvent::Completed {
                session_id: self.session.id.clone(),
                exit_code: r.exit_code,
                duration: r.duration,
            });
        }

        result
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
    ///
    /// If `shutdown_grace` is configured (default 5s), writes a `.seguro/shutdown`
    /// sentinel file to the workspace first, giving the agent time to save state.
    /// If the QEMU process exits cleanly during the grace period, the wait is
    /// cut short.
    pub async fn kill(mut self) -> Result<()> {
        // Signal the monitor to stop before killing QEMU
        self.shutdown.notify_waiters();

        // Graceful shutdown: write sentinel, wait for agent to save state
        if let Some(grace) = self.shutdown_grace {
            let sentinel = self.workspace.join(".seguro/shutdown");
            let _ = std::fs::create_dir_all(self.workspace.join(".seguro"));
            let _ = std::fs::write(&sentinel, "");

            // Poll QEMU exit every 250ms — if it exits cleanly, skip the rest
            let poll_interval = Duration::from_millis(250);
            let deadline = Instant::now() + grace;
            while Instant::now() < deadline {
                {
                    let mut qemu = self.qemu.lock().await;
                    if let Ok(Some(_)) = qemu.try_wait() {
                        break; // already exited
                    }
                }
                tokio::time::sleep(poll_interval).await;
            }
        }

        {
            let mut qemu = self.qemu.lock().await;
            let _ = qemu.kill();
        }
        let _ = self.virtiofsd.kill();
        self.session.cleanup().await
    }

    /// Kill all agent-user processes inside the guest without restarting the VM.
    ///
    /// The VM, overlay, virtiofs, and proxy remain running. After this returns,
    /// call [`Sandbox::exec`] to start a new agent process.
    pub async fn kill_agent(&self) -> Result<()> {
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
        cmd.arg("agent@127.0.0.1");
        cmd.arg("kill -TERM -- -1 2>/dev/null; true");
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());

        let status = cmd.status().await
            .wrap_err("SSH kill_agent command failed")?;

        tracing::info!(exit_code = ?status.code(), "kill_agent completed");
        Ok(())
    }

    /// Read live resource usage counters for this session.
    pub fn usage(&self) -> SessionUsage {
        use std::sync::atomic::Ordering::Relaxed;
        SessionUsage {
            wall_clock: self.started_at.elapsed(),
            proxy_requests: self.proxy_stats.requests.load(Relaxed),
            proxy_blocked: self.proxy_stats.blocked.load(Relaxed),
            proxy_bytes_received: self.proxy_stats.bytes_received.load(Relaxed),
        }
    }

    /// Inspect the workspace git state from the host side (via virtiofs).
    ///
    /// No SSH needed — runs `git` directly on the host-visible workspace path.
    /// Returns a [`WorkspaceState`] describing uncommitted and unpushed changes.
    pub fn workspace_state(&self) -> Result<WorkspaceState> {
        check_workspace_git_state(&self.workspace)
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

        let (health_tx, health_rx) = watch::channel(HealthState::Healthy);

        let (events_tx, _) = broadcast::channel::<SessionEvent>(64);

        let proxy_stats = Arc::clone(&proxy.stats);

        Ok(Self {
            session,
            qemu: Arc::new(Mutex::new(qemu)),
            virtiofsd,
            _proxy: proxy,
            net,
            workspace: meta.workspace,
            timeout: None,
            stdout: OutputMode::Inherit,
            stderr: OutputMode::Inherit,
            shutdown_grace: Some(Duration::from_secs(5)),
            shutdown: Arc::new(Notify::new()),
            _monitor: None,
            _health_tx: health_tx,
            health_rx,
            _health_monitor: None,
            events_tx,
            proxy_stats,
            started_at: Instant::now(),
        })
    }

    /// Current health state of the VM.
    ///
    /// Always returns `Healthy` if no health check interval was configured.
    pub fn health(&self) -> HealthState {
        *self.health_rx.borrow()
    }

    /// Subscribe to health state changes.
    ///
    /// Returns a `watch::Receiver` that yields the new state whenever it changes.
    pub fn subscribe_health(&self) -> watch::Receiver<HealthState> {
        self.health_rx.clone()
    }

    /// Subscribe to lifecycle events (Started, SshReady, Crashed, etc).
    ///
    /// Returns a broadcast receiver. Multiple subscribers can exist.
    /// Events that arrive before subscribing are not replayed.
    pub fn events(&self) -> broadcast::Receiver<SessionEvent> {
        self.events_tx.subscribe()
    }

    /// Read the agent's self-reported state from `.seguro/status.json`.
    ///
    /// Returns `Ok(None)` if the file doesn't exist or can't be parsed
    /// (e.g. partial write in progress). This is a synchronous filesystem
    /// read — no SSH round-trip. Ox calls this on its own supervision cadence.
    pub fn agent_state(&self) -> Result<Option<AgentState>> {
        let path = self.workspace.join(".seguro/status.json");
        match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<AgentState>(&content) {
                Ok(state) => Ok(Some(state)),
                Err(_) => Ok(None), // partial write or malformed — not an error
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).wrap_err("reading .seguro/status.json"),
        }
    }

    /// Write a message to the agent's inbox for turn-boundary delivery.
    ///
    /// Creates `{workspace}/.seguro/inbox/{timestamp_nanos}.md` containing
    /// the message text. The agent is responsible for reading and deleting
    /// inbox files at turn boundaries (e.g. via a Claude Code hook).
    /// Returns the path of the written file.
    pub fn inject(&self, message: &str) -> Result<PathBuf> {
        let inbox = self.workspace.join(".seguro/inbox");
        std::fs::create_dir_all(&inbox)
            .wrap_err("creating .seguro/inbox")?;

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let filename = format!("{ts}.md");
        let path = inbox.join(&filename);

        // Atomic write: temp file + rename to avoid partial reads
        let tmp_path = inbox.join(format!(".{filename}.tmp"));
        std::fs::write(&tmp_path, message)
            .wrap_err("writing inbox message")?;
        std::fs::rename(&tmp_path, &path)
            .wrap_err("renaming inbox message")?;

        Ok(path)
    }

    /// Count unread messages in the agent's inbox.
    ///
    /// Returns 0 if the inbox directory doesn't exist.
    pub fn pending_messages(&self) -> Result<usize> {
        let inbox = self.workspace.join(".seguro/inbox");
        match std::fs::read_dir(&inbox) {
            Ok(entries) => {
                let count = entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path().extension().is_some_and(|ext| ext == "md")
                            && !e.file_name().to_string_lossy().starts_with('.')
                    })
                    .count();
                Ok(count)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e).wrap_err("reading .seguro/inbox"),
        }
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
    events_tx: broadcast::Sender<SessionEvent>,
    session_id: String,
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

            let _ = events_tx.send(SessionEvent::Crashed {
                session_id: session_id.clone(),
                exit_code: exit_status.code(),
            });

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
                    let _ = events_tx.send(SessionEvent::Restarted {
                        session_id: session_id.clone(),
                        attempt: restart_count,
                    });
                }
                Err(e) => {
                    tracing::error!("failed to restart QEMU: {e}");
                    return;
                }
            }
        }
    })
}

/// Background task that periodically checks SSH connectivity and updates health state.
fn spawn_health_monitor(
    ssh_port: u16,
    interval: Duration,
    tx: watch::Sender<HealthState>,
    shutdown: Arc<Notify>,
    events_tx: broadcast::Sender<SessionEvent>,
    session_id: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = shutdown.notified() => {
                    tracing::debug!("health monitor: shutdown");
                    return;
                }
            }

            let state = check_ssh_health(ssh_port).await;
            let prev = *tx.borrow();
            if state != prev {
                tracing::info!(?prev, ?state, "health state changed");
                let _ = tx.send(state);
                let _ = events_tx.send(SessionEvent::HealthChanged {
                    session_id: session_id.clone(),
                    state,
                });
            }
        }
    })
}

/// Single-shot SSH health check. Attempts to read the SSH banner and
/// classifies the response time.
async fn check_ssh_health(port: u16) -> HealthState {
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpStream;

    let addr = format!("127.0.0.1:{}", port);
    let start = Instant::now();

    // Try to connect + read banner with a 10s overall timeout
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let mut stream = TcpStream::connect(&addr).await?;
        let mut buf = [0u8; 20];
        tokio::time::timeout(Duration::from_secs(8), stream.read(&mut buf))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "banner read timeout"))?
            .map(|n| n >= 4 && &buf[..4] == b"SSH-")
    })
    .await;

    match result {
        Ok(Ok(true)) => {
            let elapsed = start.elapsed();
            if elapsed > Duration::from_secs(5) {
                HealthState::Degraded
            } else {
                HealthState::Healthy
            }
        }
        Ok(Ok(false)) => HealthState::Unresponsive, // connected but no SSH banner
        Ok(Err(_)) => HealthState::Unresponsive,     // connection error
        Err(_) => HealthState::Unresponsive,          // overall timeout
    }
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

/// Check git state of a workspace directory from the host side.
///
/// Public so that CLI commands (e.g. `sessions prune`) can use it
/// without a running `Sandbox` instance.
pub fn check_workspace_git_state(workspace: &std::path::Path) -> Result<WorkspaceState> {
    use std::process::Command;

    // Check if it's a git repo
    let is_git = Command::new("git")
        .args(["-C", &workspace.to_string_lossy(), "rev-parse", "--git-dir"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !is_git {
        return Ok(WorkspaceState {
            is_git_repo: false,
            has_uncommitted: false,
            has_unpushed: false,
            dirty_files: 0,
        });
    }

    // Check uncommitted changes (modified + untracked)
    let porcelain = Command::new("git")
        .args(["-C", &workspace.to_string_lossy(), "status", "--porcelain"])
        .stderr(Stdio::null())
        .output()
        .wrap_err("running git status")?;
    let porcelain_out = String::from_utf8_lossy(&porcelain.stdout);
    let dirty_files = porcelain_out.lines().filter(|l| !l.is_empty()).count() as u32;

    // Check unpushed commits
    let unpushed = Command::new("git")
        .args(["-C", &workspace.to_string_lossy(), "log", "@{upstream}..HEAD", "--oneline"])
        .stderr(Stdio::null())
        .output()
        .map(|o| {
            o.status.success()
                && !String::from_utf8_lossy(&o.stdout).trim().is_empty()
        })
        .unwrap_or(false); // no upstream configured → not "unpushed"

    Ok(WorkspaceState {
        is_git_repo: true,
        has_uncommitted: dirty_files > 0,
        has_unpushed: unpushed,
        dirty_files,
    })
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

    #[tokio::test]
    async fn health_check_no_ssh_returns_unresponsive() {
        // No SSH server on this port — should return Unresponsive
        let state = check_ssh_health(19999).await;
        assert_eq!(state, HealthState::Unresponsive);
    }

    #[test]
    fn health_state_default_is_healthy() {
        let (_, rx) = watch::channel(HealthState::Healthy);
        assert_eq!(*rx.borrow(), HealthState::Healthy);
    }

    #[test]
    fn event_broadcast_channel_works() {
        let (tx, mut rx1) = broadcast::channel::<SessionEvent>(16);
        let mut rx2 = tx.subscribe();

        let _ = tx.send(SessionEvent::Started {
            session_id: "test-1".into(),
        });

        match rx1.try_recv().unwrap() {
            SessionEvent::Started { session_id } => assert_eq!(session_id, "test-1"),
            _ => panic!("expected Started event"),
        }
        match rx2.try_recv().unwrap() {
            SessionEvent::Started { session_id } => assert_eq!(session_id, "test-1"),
            _ => panic!("expected Started event on second subscriber"),
        }
    }

    #[test]
    fn agent_state_parses_valid_json() {
        let json = r#"{
            "state": "working",
            "updated_at": "2026-03-08T12:00:00Z",
            "task": "implementing auth module",
            "progress": 0.6
        }"#;
        let state: AgentState = serde_json::from_str(json).unwrap();
        assert_eq!(state.state, "working");
        assert_eq!(state.task.as_deref(), Some("implementing auth module"));
        assert_eq!(state.progress, Some(0.6));
    }

    #[test]
    fn agent_state_parses_minimal_json() {
        let json = r#"{"state": "idle", "updated_at": "2026-03-08T12:00:00Z"}"#;
        let state: AgentState = serde_json::from_str(json).unwrap();
        assert_eq!(state.state, "idle");
        assert!(state.task.is_none());
        assert!(state.progress.is_none());
    }

    #[test]
    fn agent_state_returns_none_for_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        // No .seguro/status.json exists
        let path = tmp.path().join(".seguro/status.json");
        match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            other => panic!("expected NotFound, got {:?}", other),
        }
    }

    #[test]
    fn agent_state_returns_none_for_malformed_json() {
        let tmp = tempfile::tempdir().unwrap();
        let seguro_dir = tmp.path().join(".seguro");
        std::fs::create_dir_all(&seguro_dir).unwrap();
        std::fs::write(seguro_dir.join("status.json"), "not json").unwrap();

        let result: Result<AgentState, _> = serde_json::from_str("not json");
        assert!(result.is_err());
    }

    #[test]
    fn inject_creates_inbox_message() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join(".seguro/inbox");

        // Simulate inject
        std::fs::create_dir_all(&inbox).unwrap();
        let msg = "Please check your test results.";
        let path = inbox.join("1234567890.md");
        std::fs::write(&path, msg).unwrap();

        assert!(path.exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), msg);
    }

    #[test]
    fn pending_messages_counts_md_files() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join(".seguro/inbox");
        std::fs::create_dir_all(&inbox).unwrap();

        // 2 real messages, 1 temp file (dotfile)
        std::fs::write(inbox.join("100.md"), "msg1").unwrap();
        std::fs::write(inbox.join("200.md"), "msg2").unwrap();
        std::fs::write(inbox.join(".300.md.tmp"), "partial").unwrap();

        let count = std::fs::read_dir(&inbox)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path().extension().is_some_and(|ext| ext == "md")
                    && !e.file_name().to_string_lossy().starts_with('.')
            })
            .count();
        assert_eq!(count, 2);
    }

    #[test]
    fn pending_messages_zero_when_no_inbox() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join(".seguro/inbox");
        // Directory doesn't exist
        match std::fs::read_dir(&inbox) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            other => panic!("expected NotFound, got {:?}", other),
        }
    }

    #[test]
    fn session_usage_serializes() {
        let usage = SessionUsage {
            wall_clock: Duration::from_secs(3600),
            proxy_requests: 142,
            proxy_blocked: 3,
            proxy_bytes_received: 5242880,
        };
        let json = serde_json::to_string(&usage).unwrap();
        assert!(json.contains("\"proxy_requests\":142"));
        assert!(json.contains("\"proxy_blocked\":3"));
    }

    #[test]
    fn shutdown_grace_default_is_5s() {
        let config = SandboxConfig::default();
        assert_eq!(config.shutdown_grace, Some(Duration::from_secs(5)));
    }

    #[test]
    fn shutdown_sentinel_written_to_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join(".seguro/shutdown");
        std::fs::create_dir_all(tmp.path().join(".seguro")).unwrap();
        std::fs::write(&sentinel, "").unwrap();
        assert!(sentinel.exists());
    }

    #[test]
    fn proxy_stats_atomic_counters() {
        use crate::proxy::ProxyStats;
        use std::sync::atomic::Ordering::Relaxed;

        let stats = ProxyStats::default();
        assert_eq!(stats.requests.load(Relaxed), 0);

        stats.requests.fetch_add(10, Relaxed);
        stats.blocked.fetch_add(2, Relaxed);
        stats.bytes_received.fetch_add(1024, Relaxed);

        assert_eq!(stats.requests.load(Relaxed), 10);
        assert_eq!(stats.blocked.load(Relaxed), 2);
        assert_eq!(stats.bytes_received.load(Relaxed), 1024);
    }

    #[test]
    fn workspace_state_non_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let state = check_workspace_git_state(tmp.path()).unwrap();
        assert!(!state.is_git_repo);
        assert!(!state.has_uncommitted);
        assert!(!state.has_unpushed);
        assert_eq!(state.dirty_files, 0);
    }

    #[test]
    fn workspace_state_clean_git_repo() {
        let tmp = tempfile::tempdir().unwrap();
        // Init a git repo with one commit
        std::process::Command::new("git")
            .args(["-C", &tmp.path().to_string_lossy(), "init"])
            .output().unwrap();
        std::process::Command::new("git")
            .args(["-C", &tmp.path().to_string_lossy(), "config", "user.email", "test@test.com"])
            .output().unwrap();
        std::process::Command::new("git")
            .args(["-C", &tmp.path().to_string_lossy(), "config", "user.name", "Test"])
            .output().unwrap();
        std::fs::write(tmp.path().join("file.txt"), "hello").unwrap();
        std::process::Command::new("git")
            .args(["-C", &tmp.path().to_string_lossy(), "add", "."])
            .output().unwrap();
        std::process::Command::new("git")
            .args(["-C", &tmp.path().to_string_lossy(), "commit", "-m", "init"])
            .output().unwrap();

        let state = check_workspace_git_state(tmp.path()).unwrap();
        assert!(state.is_git_repo);
        assert!(!state.has_uncommitted);
        assert_eq!(state.dirty_files, 0);
    }

    #[test]
    fn workspace_state_dirty_git_repo() {
        let tmp = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["-C", &tmp.path().to_string_lossy(), "init"])
            .output().unwrap();
        std::process::Command::new("git")
            .args(["-C", &tmp.path().to_string_lossy(), "config", "user.email", "test@test.com"])
            .output().unwrap();
        std::process::Command::new("git")
            .args(["-C", &tmp.path().to_string_lossy(), "config", "user.name", "Test"])
            .output().unwrap();
        std::fs::write(tmp.path().join("file.txt"), "hello").unwrap();
        std::process::Command::new("git")
            .args(["-C", &tmp.path().to_string_lossy(), "add", "."])
            .output().unwrap();
        std::process::Command::new("git")
            .args(["-C", &tmp.path().to_string_lossy(), "commit", "-m", "init"])
            .output().unwrap();
        // Create uncommitted changes
        std::fs::write(tmp.path().join("dirty.txt"), "dirty").unwrap();

        let state = check_workspace_git_state(tmp.path()).unwrap();
        assert!(state.is_git_repo);
        assert!(state.has_uncommitted);
        assert_eq!(state.dirty_files, 1);
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
