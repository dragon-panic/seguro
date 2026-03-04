use color_eyre::eyre::{Result, eyre};

use crate::cli::ShellArgs;
use crate::config::runtime_dir;

pub async fn execute(args: ShellArgs) -> Result<()> {
    let session_id = resolve_session_id(args.session_id)?;
    let session_dir = runtime_dir().join(&session_id);

    // Read SSH port and key path from runtime dir
    let ssh_port_str = std::fs::read_to_string(session_dir.join("ssh.port"))
        .map_err(|_| eyre!("session {} not found or not running", session_id))?;
    let ssh_port: u16 = ssh_port_str
        .trim()
        .parse()
        .map_err(|_| eyre!("invalid ssh.port in session dir"))?;

    let ssh_key = session_dir.join("id_ed25519");

    println!("Opening shell in session {}…", session_id);

    let status = tokio::process::Command::new("ssh")
        .args([
            "-i", ssh_key.to_str().unwrap(),
            "-p", &ssh_port.to_string(),
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "LogLevel=ERROR",
            "-t",
            "agent@127.0.0.1",
        ])
        .status()
        .await?;

    if !status.success() {
        tracing::debug!("shell session exited with {}", status);
    }
    Ok(())
}

/// Resolve the session ID to use.
/// If a specific ID is given, use it. If exactly one session is running, use that.
fn resolve_session_id(explicit: Option<String>) -> Result<String> {
    if let Some(id) = explicit {
        return Ok(id);
    }

    let run_dir = runtime_dir();
    if !run_dir.exists() {
        return Err(eyre!("no sessions running (runtime dir does not exist)"));
    }

    let sessions: Vec<String> = std::fs::read_dir(&run_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir() && e.path().join("qemu.pid").exists())
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .collect();

    match sessions.len() {
        0 => Err(eyre!("no active sessions found")),
        1 => Ok(sessions.into_iter().next().unwrap()),
        _ => Err(eyre!(
            "multiple sessions running: {}\nSpecify SESSION_ID explicitly.",
            sessions.join(", ")
        )),
    }
}
