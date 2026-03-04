use color_eyre::eyre::Result;

use crate::cli::ShellArgs;

pub async fn execute(_args: ShellArgs) -> Result<()> {
    tracing::info!("seguro shell — not yet implemented");
    todo!("seguro shell not yet implemented")
}
