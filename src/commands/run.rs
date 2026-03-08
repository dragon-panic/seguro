use std::path::PathBuf;
use std::time::Duration;

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

    let verbose = args.verbose;

    // ── Resolve workspace ─────────────────────────────────────────────────────
    let (workspace, temp_workspace) = resolve_workspace(&args.share, verbose)?;

    // ── Build env vars (pass through from host) ─────────────────────────────
    let env_vars: Vec<(String, String)> = [
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "GITHUB_TOKEN",
    ]
    .iter()
    .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
    .collect();

    let config = SandboxConfig {
        workspace: workspace.clone(),
        net: args.net.clone(),
        tls_inspect: args.tls_inspect,
        env_vars,
        persistent: args.persistent,
        profile: Some(args.effective_profile().to_owned()),
        timeout: args.timeout.map(Duration::from_secs),
        ..Default::default()
    };

    // ── Save terminal state ────────────────────────────────────────────────
    use std::os::fd::AsFd;
    let saved_termios = nix::sys::termios::tcgetattr(std::io::stdin().as_fd()).ok();

    if verbose {
        eprintln!("Starting sandbox…");
    }

    let sandbox = Sandbox::start(config).await?;
    let session_id = sandbox.id().to_owned();

    if verbose {
        eprintln!("Session {} started.", session_id);
        eprintln!("Workspace: {}", workspace.display());
        eprintln!("SSH port:  {}", sandbox.ssh_port());
        eprintln!("Guest is ready.");
    }

    // ── Execute agent command (or interactive shell) ───────────────────────
    let result = sandbox.exec(&args.agent).await?;

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
    } else if verbose {
        eprintln!("Session {} kept (--persistent).", session_id);
    }

    if result.timed_out {
        return Err(eyre!("session timed out after {}s", result.duration.as_secs()));
    }
    match result.exit_code {
        Some(0) => Ok(()),
        Some(code) => Err(eyre!("agent exited with code {}", code)),
        None => Err(eyre!("agent process was killed")),
    }
}

/// Resolve the workspace directory. Returns (workspace_path, temp_dir_if_created).
fn resolve_workspace(share: &Option<PathBuf>, verbose: bool) -> Result<(PathBuf, Option<PathBuf>)> {
    if let Some(p) = share {
        if !p.exists() {
            return Err(eyre!("--share path does not exist: {}", p.display()));
        }
        return Ok((p.canonicalize()?, None));
    }

    let tmp = std::env::temp_dir()
        .join(format!("seguro-workspace-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp)?;
    if verbose {
        eprintln!("Workspace: {} (temp, will be deleted on exit; use --persistent to keep)", tmp.display());
    }
    Ok((tmp.clone(), Some(tmp)))
}
