use color_eyre::eyre::Result;

use crate::cli::{SessionsArgs, SessionsCommand};
use crate::config::runtime_dir;
use crate::session::image::list_orphaned_overlays;

pub async fn execute(args: SessionsArgs) -> Result<()> {
    match args.command {
        SessionsCommand::Ls => list(),
        SessionsCommand::Prune { force } => prune(force),
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

fn prune(force: bool) -> Result<()> {
    let run_dir = runtime_dir();
    let orphans = list_orphaned_overlays(&run_dir)?;

    if orphans.is_empty() {
        println!("Nothing to prune.");
        return Ok(());
    }

    let mut pruned = 0;
    let mut skipped = 0;

    for orphan_dir in &orphans {
        // Try to read workspace path from session.json to check git state
        if !force {
            if let Some(workspace) = read_session_workspace(orphan_dir) {
                match crate::api::check_workspace_git_state(&workspace) {
                    Ok(state) if state.has_uncommitted || state.has_unpushed => {
                        eprintln!(
                            "Skipping {} — workspace {} has {} (use --force to override)",
                            orphan_dir.display(),
                            workspace.display(),
                            if state.has_unpushed { "unpushed commits" } else { "uncommitted changes" },
                        );
                        skipped += 1;
                        continue;
                    }
                    _ => {} // clean, not a git repo, or git not available — safe to prune
                }
            }
        }

        println!("Removing orphaned session: {}", orphan_dir.display());
        if let Err(e) = std::fs::remove_dir_all(orphan_dir) {
            eprintln!("  warning: failed to remove {}: {}", orphan_dir.display(), e);
        }
        pruned += 1;
    }

    if pruned > 0 {
        println!("Pruned {} orphaned session(s).", pruned);
    }
    if skipped > 0 {
        println!("Skipped {} session(s) with dirty workspaces.", skipped);
    }
    Ok(())
}

/// Read the workspace path from a session's `session.json`.
fn read_session_workspace(session_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let meta_path = session_dir.join("session.json");
    let content = std::fs::read_to_string(meta_path).ok()?;
    let meta: crate::api::SessionMeta = serde_json::from_str(&content).ok()?;
    Some(meta.workspace)
}
