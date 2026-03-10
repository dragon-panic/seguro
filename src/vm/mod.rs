pub mod cidata;
pub mod fw_cfg;
pub mod virtiofsd;

use color_eyre::eyre::{Result, WrapErr, eyre};
use std::path::Path;
use std::time::Duration;
use tokio::process::{Child, Command};

/// A running QEMU instance.
pub struct QemuProcess {
    child: Child,
}

impl QemuProcess {
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    pub fn kill(&mut self) -> Result<()> {
        self.child.start_kill().wrap_err("killing QEMU")
    }

    pub async fn wait(&mut self) -> Result<std::process::ExitStatus> {
        self.child.wait().await.wrap_err("waiting for QEMU to exit")
    }

    /// Check if QEMU has exited without blocking.
    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        self.child.try_wait().wrap_err("checking QEMU exit status")
    }
}

/// A virtiofs mount to expose to the guest.
#[derive(Debug, Clone)]
pub struct MountSpec {
    /// Host-side virtiofsd socket path.
    pub socket: std::path::PathBuf,
    /// Virtio tag (e.g. "seguro0").
    pub tag: String,
    /// Guest mount point (e.g. "~/workspace", "/repo").
    pub guest: String,
    /// Mount read-only on the guest side.
    pub readonly: bool,
}

/// Parameters for building the QEMU command line.
pub struct QemuParams<'a> {
    pub overlay_path: &'a Path,
    /// virtiofs mount specifications (one virtiofsd per mount).
    pub mount_specs: &'a [MountSpec],
    pub ssh_port: u16,
    pub proxy_port: u16,
    /// Host path to the NoCloud seed disk (FAT12, 512 KB) for cloud-init.
    pub cidata_disk: &'a Path,
    pub memory_mb: u32,
    pub smp: u32,
    /// Additional environment variables to inject via fw_cfg
    pub env_vars: &'a [(String, String)],
    /// If true, redirect stdout/stderr to null (non-interactive)
    pub silent: bool,
}

/// Build and launch the QEMU process for a session.
pub async fn start_qemu(params: &QemuParams<'_>) -> Result<QemuProcess> {
    let mut cmd = Command::new("qemu-system-x86_64");

    // Machine type
    cmd.args(["-M", "q35"]);

    // CPU + KVM
    if Path::new("/dev/kvm").exists() {
        cmd.args(["-cpu", "host", "-enable-kvm"]);
    } else {
        tracing::warn!("KVM not available — running in TCG mode (slow)");
        cmd.args(["-cpu", "qemu64", "-accel", "tcg"]);
    }

    // Memory and vCPUs
    cmd.args(["-m", &format!("{}M", params.memory_mb)]);
    cmd.args(["-smp", &params.smp.to_string()]);

    // Root disk (COW overlay)
    cmd.args([
        "-drive",
        &format!("file={},format=qcow2,if=virtio", params.overlay_path.display()),
    ]);

    // Networking: SLIRP user-mode with SSH forward + proxy guestfwd
    cmd.args([
        "-netdev",
        &format!(
            "user,id=net0,\
             hostfwd=tcp:127.0.0.1:{ssh}-:22,\
             guestfwd=tcp:10.0.2.100:3128-tcp:127.0.0.1:{proxy}",
            ssh = params.ssh_port,
            proxy = params.proxy_port,
        ),
    ]);
    cmd.args(["-device", "virtio-net-pci,netdev=net0"]);

    // virtiofs mounts (one chardev + device per mount, all share the same memory backend)
    for (i, spec) in params.mount_specs.iter().enumerate() {
        cmd.args([
            "-chardev",
            &format!("socket,id=char{i},path={}", spec.socket.display()),
        ]);
        cmd.args([
            "-device",
            &format!("vhost-user-fs-pci,chardev=char{i},tag={}", spec.tag),
        ]);
    }

    // Shared memory backend required by virtiofs
    cmd.args([
        "-object",
        &format!(
            "memory-backend-file,id=mem,size={}M,mem-path=/dev/shm,share=on",
            params.memory_mb
        ),
    ]);
    cmd.args(["-numa", "node,memdev=mem"]);

    // NoCloud seed disk for cloud-init (FAT12, 512 KB)
    cmd.args([
        "-drive",
        &format!(
            "file={},format=raw,if=virtio,readonly=on",
            params.cidata_disk.display()
        ),
    ]);

    // fw_cfg: env vars
    for arg in fw_cfg::build_args(params.env_vars)? {
        cmd.arg(arg);
    }

    // Console
    if params.silent {
        cmd.args(["-display", "none", "-serial", "null"]);
    } else {
        cmd.args(["-display", "none", "-serial", "mon:stdio"]);
    }

    let child = cmd.spawn().wrap_err("launching qemu-system-x86_64")?;
    tracing::info!(pid = child.id(), "QEMU started");
    Ok(QemuProcess { child })
}

/// Poll `127.0.0.1:{port}` until the SSH banner is received, or the timeout elapses.
///
/// Checking only TCP connectivity is insufficient: QEMU's SLIRP user-mode networking
/// accepts TCP connections from the host before the guest sshd is listening, which
/// causes a connection reset mid-handshake. Reading the banner ("SSH-2.0-…") confirms
/// sshd is truly ready.
pub async fn wait_for_ssh(port: u16, timeout: Duration) -> Result<()> {
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpStream;
    use tokio::time::Instant;

    let addr = format!("127.0.0.1:{}", port);
    let deadline = Instant::now() + timeout;
    let mut delay = Duration::from_millis(200);

    loop {
        if let Ok(mut stream) = TcpStream::connect(&addr).await {
            let mut buf = [0u8; 20];
            let banner_ok =
                tokio::time::timeout(Duration::from_secs(3), stream.read(&mut buf))
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .map(|n| n >= 4 && &buf[..4] == b"SSH-")
                    .unwrap_or(false);
            if banner_ok {
                tracing::info!(port, "SSH port is ready");
                return Ok(());
            }
        }

        if Instant::now() >= deadline {
            return Err(eyre!(
                "SSH did not become available on port {} within {:?}.\n\
                 The guest may have failed to boot. Check QEMU output.",
                port,
                timeout
            ));
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(2));
    }
}
