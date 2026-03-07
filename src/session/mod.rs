pub mod image;
pub mod keys;
pub mod ports;

use color_eyre::eyre::{Result, WrapErr};
use std::path::PathBuf;
use uuid::Uuid;

/// All runtime state for a single sandboxed agent session.
#[derive(Debug)]
pub struct Session {
    pub id: String,
    pub ssh_port: u16,
    pub proxy_port: u16,
    pub ssh_key_path: PathBuf,
    pub overlay_path: PathBuf,
    pub runtime_dir: PathBuf,
    /// PID of the QEMU process (written to qemu.pid for orphan detection)
    pub qemu_pid: Option<u32>,
}

impl Session {
    pub fn new_id() -> String {
        Uuid::new_v4().to_string()
    }

    /// Allocate all per-session resources (dirs, ports, keys, overlay).
    pub async fn allocate(base_image: &std::path::Path) -> Result<Self> {
        let id = Self::new_id();
        let runtime_dir = crate::config::runtime_dir().join(&id);
        std::fs::create_dir_all(&runtime_dir)
            .wrap_err_with(|| format!("creating runtime dir {}", runtime_dir.display()))?;

        let ssh_port = ports::allocate_port().await?;
        let proxy_port = ports::allocate_port().await?;
        let ssh_key_path = runtime_dir.join("id_ed25519");
        let overlay_path = runtime_dir.join("session.qcow2");

        keys::generate(&ssh_key_path).await?;
        image::create_overlay(base_image, &overlay_path).await?;

        Ok(Self {
            id,
            ssh_port,
            proxy_port,
            ssh_key_path,
            overlay_path,
            runtime_dir,
            qemu_pid: None,
        })
    }

    /// Write QEMU PID to runtime_dir/qemu.pid for orphan detection.
    pub fn record_qemu_pid(&mut self, pid: u32) -> Result<()> {
        self.qemu_pid = Some(pid);
        std::fs::write(self.runtime_dir.join("qemu.pid"), pid.to_string())
            .wrap_err("writing qemu.pid")
    }

    /// Clean up all session resources: kill child processes, remove runtime dir.
    pub async fn cleanup(self) -> Result<()> {
        if let Some(pid) = self.qemu_pid {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
        // Give processes a moment to terminate gracefully
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        if self.runtime_dir.exists() {
            std::fs::remove_dir_all(&self.runtime_dir)
                .wrap_err_with(|| format!("removing runtime dir {}", self.runtime_dir.display()))?;
        }
        Ok(())
    }
}
