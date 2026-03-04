pub mod fw_cfg;
pub mod virtiofsd;

use color_eyre::eyre::{Result, eyre};
use std::path::Path;
use tokio::process::{Child, Command};

/// QEMU process handle
pub struct QemuProcess {
    child: Child,
}

impl QemuProcess {
    /// Build and launch the QEMU process for a session.
    pub async fn start(params: &QemuParams) -> Result<Self> {
        let mut cmd = Command::new("qemu-system-x86_64");

        // Machine type and CPU
        cmd.args(["-M", "q35"]);
        if Path::new("/dev/kvm").exists() {
            cmd.args(["-cpu", "host", "-enable-kvm"]);
        } else {
            tracing::warn!("KVM not available — running in TCG (slow) mode");
            cmd.args(["-cpu", "qemu64"]);
        }

        // Memory and SMP
        cmd.args(["-m", &format!("{}M", params.memory_mb)]);
        cmd.args(["-smp", &params.smp.to_string()]);

        // Root disk (COW overlay)
        cmd.args([
            "-drive",
            &format!(
                "file={},format=qcow2,if=virtio",
                params.overlay_path.display()
            ),
        ]);

        // Networking: user-mode SLIRP with SSH port forward + proxy guestfwd
        cmd.args([
            "-netdev",
            &format!(
                "user,id=net0,hostfwd=tcp:127.0.0.1:{}-:22,guestfwd=tcp:10.0.2.100:3128-tcp:127.0.0.1:{}",
                params.ssh_port, params.proxy_port
            ),
        ]);
        cmd.args(["-device", "virtio-net-pci,netdev=net0"]);

        // virtio-fs workspace mount
        cmd.args([
            "-chardev",
            &format!("socket,id=char0,path={}", params.virtiofs_sock.display()),
        ]);
        cmd.args(["-device", "vhost-user-fs-pci,chardev=char0,tag=workspace"]);

        // Shared memory required for vhost-user-fs
        cmd.args([
            "-object",
            &format!("memory-backend-file,id=mem,size={}M,mem-path=/dev/shm,share=on", params.memory_mb),
        ]);
        cmd.args(["-numa", "node,memdev=mem"]);

        // fw_cfg entries (SSH public key, env vars)
        for arg in fw_cfg::build_args(params)? {
            cmd.arg(arg);
        }

        // Console: no graphical output
        cmd.args(["-nographic", "-serial", "stdio"]);

        let child = cmd
            .spawn()
            .map_err(|e| eyre!("failed to launch qemu-system-x86_64: {}", e))?;

        tracing::info!(pid = child.id(), "QEMU started");
        Ok(Self { child })
    }

    /// Wait for the QEMU process to exit.
    pub async fn wait(&mut self) -> Result<()> {
        let status = self.child.wait().await?;
        if !status.success() {
            return Err(eyre!("QEMU exited with status {}", status));
        }
        Ok(())
    }

    /// Send SIGTERM to the QEMU process.
    pub fn kill(&mut self) -> Result<()> {
        self.child
            .start_kill()
            .map_err(|e| eyre!("failed to kill QEMU: {}", e))
    }
}

/// Parameters for launching a QEMU VM.
pub struct QemuParams {
    pub overlay_path: std::path::PathBuf,
    pub virtiofs_sock: std::path::PathBuf,
    pub ssh_port: u16,
    pub proxy_port: u16,
    pub ssh_pubkey: String,
    pub memory_mb: u32,
    pub smp: u32,
    pub env_vars: Vec<(String, String)>,
}
