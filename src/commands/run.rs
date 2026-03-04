use color_eyre::eyre::{Result, eyre};

use crate::cli::{NetMode, RunArgs};

pub async fn execute(args: RunArgs) -> Result<()> {
    // Validate dev-bridge safety gate
    if matches!(args.net, NetMode::DevBridge) && !args.unsafe_dev_bridge {
        return Err(eyre!(
            "--net dev-bridge allows guest access to your LAN and is dangerous.\n\
             Pass --unsafe-dev-bridge to acknowledge the risk and enable it."
        ));
    }

    tracing::info!("seguro run — not yet implemented (tracked in LPha + QyyE)");
    todo!("seguro run not yet implemented")
}
