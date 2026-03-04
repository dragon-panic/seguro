use color_eyre::eyre::{Result, WrapErr, eyre};
use std::path::Path;
use tokio::process::{Child, Command};

/// A running virtiofsd instance.
pub struct Virtiofsd {
    child: Child,
}

impl Virtiofsd {
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    /// Start virtiofsd, sharing `shared_dir` via `socket_path`.
    pub async fn start(shared_dir: &Path, socket_path: &Path) -> Result<Self> {
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent).wrap_err("creating socket dir")?;
        }

        if !shared_dir.exists() {
            std::fs::create_dir_all(shared_dir)
                .wrap_err_with(|| format!("creating share dir {}", shared_dir.display()))?;
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
