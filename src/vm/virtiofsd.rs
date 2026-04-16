use color_eyre::eyre::{Result, WrapErr, eyre};
use std::path::Path;
use tokio::process::{Child, Command};

/// UID/GID of the `agent` user inside the guest VM image.
/// cloud-init assigns 1001 because the default `ubuntu` user claims 1000.
const GUEST_AGENT_UID: u32 = 1001;
const GUEST_AGENT_GID: u32 = 1001;

/// A running virtiofsd instance.
pub struct Virtiofsd {
    child: Child,
}

impl Virtiofsd {
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    /// Start virtiofsd, sharing `shared_dir` via `socket_path`.
    ///
    /// When `readonly` is false, adds `--translate-uid` / `--translate-gid`
    /// mappings so the guest agent user can write through the virtiofs mount
    /// regardless of the host user's UID.
    pub async fn start(shared_dir: &Path, socket_path: &Path, readonly: bool) -> Result<Self> {
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent).wrap_err("creating socket dir")?;
        }

        if !shared_dir.exists() {
            std::fs::create_dir_all(shared_dir)
                .wrap_err_with(|| format!("creating share dir {}", shared_dir.display()))?;
        }

        // virtiofsd lives at /usr/lib/virtiofsd on Arch (virtiofsd package),
        // but may be on $PATH on other distros.
        let bin = if std::path::Path::new("/usr/lib/virtiofsd").exists() {
            "/usr/lib/virtiofsd"
        } else {
            "virtiofsd"
        };

        let mut args = vec![
            format!("--socket-path={}", socket_path.display()),
            format!("--shared-dir={}", shared_dir.display()),
            "--announce-submounts".to_string(),
            "--sandbox=namespace".to_string(),
            "--log-level=warn".to_string(),
        ];

        // For writable shares, map the guest agent UID/GID to the host user so
        // file creation works across the virtiofs boundary.
        if !readonly {
            let host_uid = nix::unistd::getuid();
            let host_gid = nix::unistd::getgid();
            args.push(format!(
                "--translate-uid=map:{GUEST_AGENT_UID}:{host_uid}:1"
            ));
            args.push(format!(
                "--translate-gid=map:{GUEST_AGENT_GID}:{host_gid}:1"
            ));
        }

        let child = Command::new(bin)
            .args(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .wrap_err("launching virtiofsd")?;

        tracing::info!(
            pid = child.id(),
            socket = %socket_path.display(),
            share = %shared_dir.display(),
            "virtiofsd started"
        );

        // Give virtiofsd a moment to create the socket before QEMU tries to connect
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !socket_path.exists() {
            if std::time::Instant::now() >= deadline {
                return Err(eyre!(
                    "virtiofsd socket {} did not appear after 5s",
                    socket_path.display()
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        Ok(Self { child })
    }

    pub fn kill(&mut self) -> Result<()> {
        self.child.start_kill().wrap_err("killing virtiofsd")
    }
}
