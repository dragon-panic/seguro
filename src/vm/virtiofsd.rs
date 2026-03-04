use color_eyre::eyre::{Result, eyre};
use std::path::Path;
use tokio::process::{Child, Command};

/// virtiofsd process handle
pub struct Virtiofsd {
    child: Child,
}

impl Virtiofsd {
    /// Start virtiofsd for the given shared directory and socket path.
    pub async fn start(shared_dir: &Path, socket_path: &Path) -> Result<Self> {
        // Ensure socket directory exists
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let child = Command::new("virtiofsd")
            .args([
                &format!("--socket-path={}", socket_path.display()),
                &format!("--shared-dir={}", shared_dir.display()),
                "--announce-submounts",
                "--sandbox=namespace",
                "--log-level=warn",
            ])
            .spawn()
            .map_err(|e| eyre!("failed to launch virtiofsd: {}", e))?;

        tracing::info!(pid = child.id(), socket = %socket_path.display(), "virtiofsd started");
        Ok(Self { child })
    }

    /// Stop the virtiofsd process.
    pub fn kill(&mut self) -> Result<()> {
        self.child
            .start_kill()
            .map_err(|e| eyre!("failed to kill virtiofsd: {}", e))
    }
}
