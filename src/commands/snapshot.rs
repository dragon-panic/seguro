use color_eyre::eyre::Result;

use crate::cli::{SnapshotArgs, SnapshotCommand};

pub async fn execute(args: SnapshotArgs) -> Result<()> {
    match args.command {
        SnapshotCommand::Save { name } => save(&name).await,
        SnapshotCommand::Restore { name } => restore(&name).await,
    }
}

async fn save(name: &str) -> Result<()> {
    let _ = name;
    tracing::info!("seguro snapshot save — not yet implemented");
    todo!("snapshot save not yet implemented")
}

async fn restore(name: &str) -> Result<()> {
    let _ = name;
    tracing::info!("seguro snapshot restore — not yet implemented");
    todo!("snapshot restore not yet implemented")
}
