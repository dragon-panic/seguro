use color_eyre::eyre::{Result, eyre};

use crate::cli::{SnapshotArgs, SnapshotCommand};
use crate::config::runtime_dir;
use crate::session::image::{snapshot_restore, snapshot_save};

pub async fn execute(args: SnapshotArgs) -> Result<()> {
    match args.command {
        SnapshotCommand::Save { name } => save(&name).await,
        SnapshotCommand::Restore { name } => restore(&name).await,
    }
}

async fn save(name: &str) -> Result<()> {
    let (overlay, _) = active_session_overlay()?;
    println!("Saving snapshot '{}' to {}…", name, overlay.display());
    snapshot_save(&overlay, name).await?;
    println!("Snapshot '{}' saved.", name);
    Ok(())
}

async fn restore(name: &str) -> Result<()> {
    let (overlay, base) = active_session_overlay()?;
    let base = base.ok_or_else(|| {
        eyre!("cannot restore snapshot: base image path not recorded for this session")
    })?;
    println!("Restoring snapshot '{}' into {}…", name, overlay.display());
    snapshot_restore(&base, name, &overlay).await?;
    println!("Snapshot '{}' restored. Restart the session to apply.", name);
    Ok(())
}

/// Find the overlay qcow2 path for the currently active session.
/// Returns (overlay_path, Option<base_path>).
fn active_session_overlay() -> Result<(std::path::PathBuf, Option<std::path::PathBuf>)> {
    let run_dir = runtime_dir();
    let sessions: Vec<_> = std::fs::read_dir(&run_dir)
        .map_err(|_| eyre!("no active sessions"))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path().is_dir()
                && e.path().join("qemu.pid").exists()
                && e.path().join("session.qcow2").exists()
        })
        .collect();

    match sessions.len() {
        0 => Err(eyre!("no active session with a disk overlay found")),
        1 => {
            let dir = sessions[0].path();
            let overlay = dir.join("session.qcow2");
            let base = std::fs::read_to_string(dir.join("base.path"))
                .ok()
                .map(|s| std::path::PathBuf::from(s.trim()));
            Ok((overlay, base))
        }
        _ => Err(eyre!(
            "multiple sessions running; snapshot commands require exactly one active session"
        )),
    }
}
