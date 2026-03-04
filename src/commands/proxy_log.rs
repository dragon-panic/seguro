use color_eyre::eyre::Result;

use crate::cli::ProxyLogArgs;

pub async fn execute(_args: ProxyLogArgs) -> Result<()> {
    tracing::info!("seguro proxy-log — not yet implemented");
    todo!("proxy-log not yet implemented")
}
