use color_eyre::eyre::Result;

use crate::cli::{SessionsArgs, SessionsCommand};

pub async fn execute(args: SessionsArgs) -> Result<()> {
    match args.command {
        SessionsCommand::Ls => list().await,
        SessionsCommand::Prune => prune().await,
    }
}

async fn list() -> Result<()> {
    tracing::info!("seguro sessions ls — not yet implemented");
    todo!("sessions ls not yet implemented")
}

async fn prune() -> Result<()> {
    tracing::info!("seguro sessions prune — not yet implemented");
    todo!("sessions prune not yet implemented")
}
