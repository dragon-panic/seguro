use std::path::PathBuf;
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr, eyre};
use tokio::signal;

use crate::cli::{NetMode, RunArgs};
use crate::config::Config;
use crate::proxy::ProxyServer;
use crate::session::{Session, image};
use crate::vm::{self, QemuParams, virtiofsd::Virtiofsd};

pub async fn execute(args: RunArgs) -> Result<()> {
    // ── Safety gate for dev-bridge ────────────────────────────────────────────
    if matches!(args.net, NetMode::DevBridge) && !args.unsafe_dev_bridge {
        return Err(eyre!(
            "--net dev-bridge allows the guest to reach your host LAN (UNSAFE).\n\
             Pass --unsafe-dev-bridge to acknowledge the risk and enable this mode."
        ));
    }

    // ── Resolve workspace ─────────────────────────────────────────────────────
    let (workspace, temp_workspace) = resolve_workspace(&args.share)?;

    // ── Load config ───────────────────────────────────────────────────────────
    let config = Config::load(Some(&workspace))?;

    // ── Find base image ───────────────────────────────────────────────────────
    let base_image = image::locate_base(args.browser, None)?;

    // ── Allocate session resources ────────────────────────────────────────────
    let mut session = Session::allocate(&base_image).await?;
    let session_id = session.id.clone();

    tracing::info!(session_id = %session_id, ssh_port = session.ssh_port, "session allocated");

    // ── Start proxy server ────────────────────────────────────────────────────
    let proxy = ProxyServer::start(
        &config,
        &args.net,
        args.tls_inspect,
        &session.runtime_dir,
    )
    .await?;

    // Use the port the proxy actually bound to (pre-allocated port is released before bind)
    session.proxy_port = proxy.port;

    // ── Start virtiofsd ───────────────────────────────────────────────────────
    let mut virtiofsd = Virtiofsd::start(&workspace, &session.virtiofs_sock).await?;
    if let Some(pid) = virtiofsd.id() {
        session.virtiofsd_pid = Some(pid);
    }

    // ── Memory / SMP (bumped for --browser) ──────────────────────────────────
    let memory_mb = config.vm.memory_mb.unwrap_or(if args.browser { 4096 } else { 2048 });
    let smp = config.vm.smp.unwrap_or(if args.browser { 4 } else { 2 });

    // ── Build env vars for agent ──────────────────────────────────────────────
    // Pass through ANTHROPIC_API_KEY and other common agent vars if set
    let env_vars: Vec<(String, String)> = [
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "GITHUB_TOKEN",
        "HOME",       // will be overridden inside guest, but useful to pass
    ]
    .iter()
    .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
    .collect();

    // ── SSH key ───────────────────────────────────────────────────────────────
    let ssh_pubkey_path = session.ssh_key_path.with_extension("pub");
    let pubkey_str = std::fs::read_to_string(&ssh_pubkey_path)
        .wrap_err("reading SSH public key")?;
    let pubkey_str = pubkey_str.trim_end_matches('\n');

    // Create a NoCloud seed disk (FAT12, 512 KB) for cloud-init key injection.
    // If TLS inspection is active, embed the CA cert so the guest can trust inspected certs.
    let cidata_path = session.runtime_dir.join("cidata.img");
    vm::cidata::create_cidata_seed(
        &session_id,
        pubkey_str,
        proxy.ca_cert_pem(),
        &cidata_path,
    )?;

    // Write env vars to workspace .seguro/ for the guest to pick up.
    inject_workspace_config(&workspace, &env_vars)?;

    // ── Launch QEMU ───────────────────────────────────────────────────────────
    let qemu_params = QemuParams {
        overlay_path: &session.overlay_path,
        virtiofs_sock: &session.virtiofs_sock,
        ssh_port: session.ssh_port,
        proxy_port: session.proxy_port,
        cidata_disk: &cidata_path,
        memory_mb,
        smp,
        env_vars: &env_vars,
        // Silence serial console when running a command non-interactively —
        // boot messages would otherwise interleave with command output.
        silent: !args.agent.is_empty(),
    };

    // Save terminal state — QEMU's -serial mon:stdio puts the terminal into raw
    // mode and doesn't restore it on exit, leaving the shell broken (no echo).
    use std::os::fd::AsFd;
    let saved_termios = nix::sys::termios::tcgetattr(std::io::stdin().as_fd()).ok();

    let mut qemu = vm::start_qemu(&qemu_params).await?;
    if let Some(pid) = qemu.id() {
        session.record_qemu_pid(pid)?;
    }

    // Write session metadata for other commands (shell, sessions ls, proxy log)
    std::fs::write(session.runtime_dir.join("ssh.port"), session.ssh_port.to_string())?;
    std::fs::write(session.runtime_dir.join("workspace.path"), workspace.display().to_string())?;
    std::fs::write(session.runtime_dir.join("base.path"), base_image.display().to_string())?;

    println!("Session {} started.", session_id);
    println!("Workspace: {}", workspace.display());
    println!("SSH port:  {}", session.ssh_port);

    // ── Wait for SSH to become available ──────────────────────────────────────
    println!("Waiting for guest SSH… (Ctrl+C to abort)");
    let ssh_timeout = Duration::from_secs(config.ssh_timeout() as u64);
    tokio::select! {
        r = vm::wait_for_ssh(session.ssh_port, ssh_timeout) => r?,
        _ = signal::ctrl_c() => {
            tracing::info!("Ctrl+C during SSH wait, shutting down");
            let _ = qemu.kill();
            let _ = virtiofsd.kill();
            if let Some(termios) = saved_termios {
                let _ = nix::sys::termios::tcsetattr(
                    std::io::stdin().as_fd(),
                    nix::sys::termios::SetArg::TCSANOW,
                    &termios,
                );
            }
            return Ok(());
        }
    }
    println!("Guest is ready.");

    // ── Execute agent command (or interactive shell) ───────────────────────────
    let agent_result = run_agent(&session, &args.agent).await;

    // ── Graceful shutdown ─────────────────────────────────────────────────────
    // Also handle Ctrl+C racing with normal exit
    tokio::select! {
        _ = signal::ctrl_c() => {
            tracing::info!("Ctrl+C received, shutting down");
        }
        _ = async { } => {}  // normal exit path falls through
    }

    // Kill QEMU and virtiofsd
    let _ = qemu.kill();
    let _ = virtiofsd.kill();

    // Restore terminal state (QEMU raw-mode side-effect)
    if let Some(termios) = saved_termios {
        let _ = nix::sys::termios::tcsetattr(
            std::io::stdin().as_fd(),
            nix::sys::termios::SetArg::TCSANOW,
            &termios,
        );
    }

    // Clean up runtime state unless --persistent
    if !args.persistent {
        session.cleanup().await?;
        // Remove temp workspace if we created one
        if let Some(tmp) = temp_workspace {
            let _ = std::fs::remove_dir_all(&tmp);
        }
    } else {
        println!("Session {} kept (--persistent).", session_id);
        println!("Overlay: {}", session.overlay_path.display());
    }

    agent_result
}

/// Write env vars to `workspace/.seguro/` so the guest can read them via virtiofs.
fn inject_workspace_config(
    workspace: &std::path::Path,
    env_vars: &[(String, String)],
) -> color_eyre::eyre::Result<()> {
    use color_eyre::eyre::WrapErr;
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

/// Resolve the workspace directory. Returns (workspace_path, temp_dir_if_created).
fn resolve_workspace(share: &Option<PathBuf>) -> Result<(PathBuf, Option<PathBuf>)> {
    if let Some(p) = share {
        if !p.exists() {
            return Err(eyre!("--share path does not exist: {}", p.display()));
        }
        return Ok((p.canonicalize()?, None));
    }

    // Create a temp directory
    let tmp = std::env::temp_dir()
        .join(format!("seguro-workspace-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp)?;
    println!("Workspace: {} (temp, will be deleted on exit; use --persistent to keep)", tmp.display());
    Ok((tmp.clone(), Some(tmp)))
}

/// SSH into the guest and execute the agent command (or an interactive shell).
async fn run_agent(session: &Session, agent: &[String]) -> Result<()> {
    let mut cmd = tokio::process::Command::new("ssh");
    cmd.args([
        "-i", session.ssh_key_path.to_str().unwrap(),
        "-p", &session.ssh_port.to_string(),
        // Strict host key checking off (ephemeral guest, key changes each session)
        "-o", "StrictHostKeyChecking=no",
        "-o", "UserKnownHostsFile=/dev/null",
        "-o", "IdentitiesOnly=yes",
        "-o", "IdentityAgent=none",
        "-o", "LogLevel=QUIET",
        "agent@127.0.0.1",
    ]);

    // Workspace is mounted at /home/agent/workspace via fstab (as root at boot).
    cmd.arg("cd ~/workspace 2>/dev/null || true;");

    if agent.is_empty() {
        // Interactive shell
        cmd.arg("exec bash -l");
    } else {
        cmd.arg(agent.join(" "));
    }

    let status = cmd.status().await?;
    if !status.success() {
        tracing::info!("agent exited with status {}", status);
    }
    Ok(())
}
