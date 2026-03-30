use std::path::PathBuf;
use std::time::Duration;

use color_eyre::eyre::{Result, eyre};

use crate::api::{Mount, Sandbox, SandboxConfig};
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

    // ── Parse mounts ──────────────────────────────────────────────────────────
    let (mounts, temp_workspace) = resolve_mounts(&args.share, verbose)?;

    // ── Build env vars (pass through from host + explicit --env) ─────────────
    let mut env_vars: Vec<(String, String)> = [
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "GITHUB_TOKEN",
    ]
    .iter()
    .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
    .collect();

    // Merge explicit --env KEY=VALUE (these override passthrough)
    for spec in &args.extra_env {
        let (k, v) = spec.split_once('=')
            .ok_or_else(|| eyre!("--env must be KEY=VALUE, got: {spec}"))?;
        if let Some(existing) = env_vars.iter_mut().find(|(ek, _)| ek == k) {
            existing.1 = v.to_string();
        } else {
            env_vars.push((k.to_string(), v.to_string()));
        }
    }

    let config = SandboxConfig {
        mounts: mounts.clone(),
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
        for m in &mounts {
            let ro = if m.readonly { " (ro)" } else { "" };
            eprintln!("  mount: {} → {}{}", m.host.display(), m.guest, ro);
        }
    }

    let sandbox = Sandbox::start(config).await?;
    let session_id = sandbox.id().to_owned();

    if verbose {
        eprintln!("Session {} started.", session_id);
        eprintln!("SSH port:  {}", sandbox.ssh_port());
        eprintln!("Guest is ready.");
    }

    // ── Register signal handlers ─────────────────────────────────────────────
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigterm = signal(SignalKind::terminate())
        .expect("failed to register SIGTERM handler");
    let mut sighup = signal(SignalKind::hangup())
        .expect("failed to register SIGHUP handler");

    // ── Execute agent command, racing against signals ────────────────────────
    let result = tokio::select! {
        r = sandbox.exec(&args.agent) => r,
        _ = sigterm.recv() => {
            tracing::info!("SIGTERM received, shutting down");
            Err(eyre!("terminated by signal"))
        }
        _ = sighup.recv() => {
            tracing::info!("SIGHUP received, shutting down");
            Err(eyre!("terminated by signal"))
        }
    };

    // ── Always kill the sandbox — regardless of how exec ended ───────────────
    if let Err(e) = sandbox.kill().await {
        tracing::warn!("sandbox cleanup error: {e:#}");
    }

    // ── Restore terminal state ───────────────────────────────────────────────
    if let Some(termios) = saved_termios {
        let _ = nix::sys::termios::tcsetattr(
            std::io::stdin().as_fd(),
            nix::sys::termios::SetArg::TCSANOW,
            &termios,
        );
    }

    // Clean up temp workspace if we created one
    let persistent = args.persistent;
    if !persistent {
        if let Some(tmp) = temp_workspace {
            let _ = std::fs::remove_dir_all(&tmp);
        }
    } else if verbose {
        eprintln!("Session {} kept (--persistent).", session_id);
    }

    // ── Report exit status ────────────────────────────────────────────────────
    let result = result?;
    if result.timed_out {
        return Err(eyre!("session timed out after {}s", result.duration.as_secs()));
    }
    match result.exit_code {
        Some(0) => Ok(()),
        Some(code) => Err(eyre!("agent exited with code {}", code)),
        None => Err(eyre!("agent process was killed")),
    }
}

/// Parse `--share` arguments into `Mount` structs.
///
/// Formats:
///   /host/path              → mount at ~/workspace (backwards compat)
///   /host/path:/guest/path  → explicit guest path
///   /host/path:/guest:ro    → read-only mount
///
/// If no `--share` is given, creates a temp dir mounted at ~/workspace.
fn resolve_mounts(
    shares: &[String],
    verbose: bool,
) -> Result<(Vec<Mount>, Option<PathBuf>)> {
    if shares.is_empty() {
        let tmp = std::env::temp_dir()
            .join(format!("seguro-workspace-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp)?;
        if verbose {
            eprintln!("Workspace: {} (temp, will be deleted on exit; use --persistent to keep)", tmp.display());
        }
        return Ok((
            vec![Mount {
                host: tmp.clone(),
                guest: "~/workspace".into(),
                readonly: false,
            }],
            Some(tmp),
        ));
    }

    let mut mounts = Vec::with_capacity(shares.len());
    for spec in shares {
        let mount = parse_share_spec(spec)?;
        if !mount.host.exists() {
            return Err(eyre!("--share path does not exist: {}", mount.host.display()));
        }
        mounts.push(mount);
    }
    Ok((mounts, None))
}

/// Parse a single `--share` spec string into a `Mount`.
fn parse_share_spec(spec: &str) -> Result<Mount> {
    // Split on ':', but be careful with Windows-style paths (not relevant on Linux
    // but we still handle the common case). On Linux, paths don't contain ':'.
    let parts: Vec<&str> = spec.split(':').collect();
    match parts.len() {
        1 => Ok(Mount {
            host: PathBuf::from(parts[0]),
            guest: "~/workspace".into(),
            readonly: false,
        }),
        2 => Ok(Mount {
            host: PathBuf::from(parts[0]),
            guest: parts[1].into(),
            readonly: false,
        }),
        3 if parts[2] == "ro" => Ok(Mount {
            host: PathBuf::from(parts[0]),
            guest: parts[1].into(),
            readonly: true,
        }),
        _ => Err(eyre!(
            "invalid --share format: {spec}\n\
             Expected: /host/path, /host/path:/guest/path, or /host/path:/guest/path:ro"
        )),
    }
}
