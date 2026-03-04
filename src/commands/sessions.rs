use color_eyre::eyre::Result;

use crate::cli::{SessionsArgs, SessionsCommand};
use crate::config::runtime_dir;
use crate::session::image::list_orphaned_overlays;

pub async fn execute(args: SessionsArgs) -> Result<()> {
    match args.command {
        SessionsCommand::Ls => list(),
        SessionsCommand::Prune => prune(),
    }
}

fn list() -> Result<()> {
    let run_dir = runtime_dir();
    if !run_dir.exists() {
        println!("No sessions running.");
        return Ok(());
    }

    let mut sessions: Vec<_> = std::fs::read_dir(&run_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir() && e.path().join("qemu.pid").exists())
        .collect();

    if sessions.is_empty() {
        println!("No active sessions.");
        return Ok(());
    }

    sessions.sort_by_key(|e| e.path());

    println!("{:<38} {:>10} {}", "SESSION ID", "SSH PORT", "WORKSPACE");
    for entry in sessions {
        let id = entry.file_name().to_string_lossy().to_string();
        let dir = entry.path();

        let ssh_port = std::fs::read_to_string(dir.join("ssh.port"))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "?".into());

        let workspace = std::fs::read_to_string(dir.join("workspace.path"))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "(unknown)".into());

        println!("{:<38} {:>10} {}", id, ssh_port, workspace);
    }
    Ok(())
}

fn prune() -> Result<()> {
    let run_dir = runtime_dir();
    let orphans = list_orphaned_overlays(&run_dir)?;

    if orphans.is_empty() {
        println!("Nothing to prune.");
        return Ok(());
    }

    for orphan_dir in &orphans {
        println!("Removing orphaned session: {}", orphan_dir.display());
        if let Err(e) = std::fs::remove_dir_all(orphan_dir) {
            eprintln!("  warning: failed to remove {}: {}", orphan_dir.display(), e);
        }
    }

    println!("Pruned {} orphaned session(s).", orphans.len());
    Ok(())
}
