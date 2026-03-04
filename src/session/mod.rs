pub mod image;
pub mod keys;
pub mod ports;

use color_eyre::eyre::Result;
use std::path::PathBuf;
use uuid::Uuid;

/// A sandboxed agent session.
#[derive(Debug)]
pub struct Session {
    pub id: String,
    pub ssh_port: u16,
    pub proxy_port: u16,
    pub virtiofs_sock: PathBuf,
    pub ssh_key_path: PathBuf,
    pub overlay_path: PathBuf,
    pub workspace_path: PathBuf,
    pub runtime_dir: PathBuf,
}

impl Session {
    pub fn new_id() -> String {
        Uuid::new_v4().to_string()
    }

    /// Allocate all per-session resources (ports, keys, paths).
    pub async fn allocate(
        workspace: PathBuf,
        base_image: PathBuf,
    ) -> Result<Self> {
        let id = Self::new_id();
        let runtime_dir = crate::config::runtime_dir().join(&id);
        std::fs::create_dir_all(&runtime_dir)?;

        let ssh_port = ports::allocate_port().await?;
        let proxy_port = ports::allocate_port().await?;
        let virtiofs_sock = runtime_dir.join("virtiofs.sock");
        let ssh_key_path = runtime_dir.join("id_ed25519");
        let overlay_path = runtime_dir.join("session.qcow2");

        keys::generate(&ssh_key_path).await?;
        image::create_overlay(&base_image, &overlay_path).await?;

        Ok(Self {
            id,
            ssh_port,
            proxy_port,
            virtiofs_sock,
            ssh_key_path,
            overlay_path,
            workspace_path: workspace,
            runtime_dir,
        })
    }

    /// Clean up all session resources.
    pub async fn cleanup(&self) -> Result<()> {
        if self.runtime_dir.exists() {
            std::fs::remove_dir_all(&self.runtime_dir)?;
        }
        Ok(())
    }
}
