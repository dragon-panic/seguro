use std::path::PathBuf;

use color_eyre::eyre::{Result, eyre};
use tokio::signal;

use crate::api::{Sandbox, SandboxConfig};
use crate::cli::{NetMode, RunArgs};

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

    // ── Build env vars (pass through from host) ─────────────────────────────
    let env_vars: Vec<(String, String)> = [
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "GITHUB_TOKEN",
    ]
    .iter()
    .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
    .collect();

    // ── Memory / SMP (bumped for --browser) ────────────────────────────────
    let memory_mb = if args.browser { 4096 } else { 2048 };
    let smp = if args.browser { 4 } else { 2 };

    let config = SandboxConfig {
        workspace: workspace.clone(),
        net: args.net.clone(),
        tls_inspect: args.tls_inspect,
        env_vars,
        memory_mb,
        smp,
        persistent: args.persistent,
        browser: args.browser,
    };

    // ── Save terminal state ────────────────────────────────────────────────
    // QEMU's -serial mon:stdio puts the terminal into raw mode and doesn't
    // restore it on exit, leaving the shell broken (no echo).
    use std::os::fd::AsFd;
    let saved_termios = nix::sys::termios::tcgetattr(std::io::stdin().as_fd()).ok();

    println!("Starting sandbox…");

    let sandbox = Sandbox::start(config).await?;
    let session_id = sandbox.id().to_owned();

    println!("Session {} started.", session_id);
    println!("Workspace: {}", workspace.display());
    println!("SSH port:  {}", sandbox.ssh_port());
    println!("Guest is ready.");

    // ── Execute agent command (or interactive shell) ───────────────────────
    let agent_result = sandbox.exec(&args.agent).await;

    // ── Graceful shutdown ─────────────────────────────────────────────────
    tokio::select! {
        _ = signal::ctrl_c() => {
            tracing::info!("Ctrl+C received, shutting down");
        }
        _ = async { } => {}
    }

    let persistent = args.persistent;
    sandbox.kill().await?;

    // Restore terminal state
    if let Some(termios) = saved_termios {
        let _ = nix::sys::termios::tcsetattr(
            std::io::stdin().as_fd(),
            nix::sys::termios::SetArg::TCSANOW,
            &termios,
        );
    }

    // Clean up temp workspace if we created one
    if !persistent {
        if let Some(tmp) = temp_workspace {
            let _ = std::fs::remove_dir_all(&tmp);
        }
    } else {
        println!("Session {} kept (--persistent).", session_id);
    }

    agent_result.map(|_| ())
}

/// Resolve the workspace directory. Returns (workspace_path, temp_dir_if_created).
fn resolve_workspace(share: &Option<PathBuf>) -> Result<(PathBuf, Option<PathBuf>)> {
    if let Some(p) = share {
        if !p.exists() {
            return Err(eyre!("--share path does not exist: {}", p.display()));
        }
        return Ok((p.canonicalize()?, None));
    }

    let tmp = std::env::temp_dir()
        .join(format!("seguro-workspace-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp)?;
    println!("Workspace: {} (temp, will be deleted on exit; use --persistent to keep)", tmp.display());
    Ok((tmp.clone(), Some(tmp)))
}
