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

/// Per-session path layout.
///
/// Two distinct roots, by design:
///   - `runtime_dir`  → `$XDG_RUNTIME_DIR/seguro/<id>/` (tmpfs): sockets, pids,
///     ssh key, cidata.img, session.json. Small, ephemeral, cleared on reboot.
///   - `overlay_path` → `overlay_dir()/<id>.qcow2` (real disk): the only artifact
///     that grows during guest work.
///
/// Before this split, everything shared `runtime_dir` and a concurrent build
/// could fill tmpfs in minutes, wedging guests with silent write errors.
pub struct SessionLayout {
    pub runtime_dir: PathBuf,
    pub overlay_path: PathBuf,
}

/// Compute the on-disk layout for a session id. Pure — no side effects.
pub fn session_layout(id: &str) -> SessionLayout {
    SessionLayout {
        runtime_dir: crate::config::runtime_dir().join(id),
        overlay_path: crate::config::overlay_dir().join(format!("{id}.qcow2")),
    }
}

impl Session {
    pub fn new_id() -> String {
        Uuid::new_v4().to_string()
    }

    /// Allocate all per-session resources (dirs, ports, keys, overlay).
    pub async fn allocate(base_image: &std::path::Path) -> Result<Self> {
        let id = Self::new_id();
        let SessionLayout { runtime_dir, overlay_path } = session_layout(&id);

        // Runtime dir first. If we crash between this and overlay creation,
        // the orphan is a Dead runtime dir (prune handles) — never a qcow2
        // without a runtime dir to link it to.
        std::fs::create_dir_all(&runtime_dir)
            .wrap_err_with(|| format!("creating runtime dir {}", runtime_dir.display()))?;
        if let Some(parent) = overlay_path.parent() {
            std::fs::create_dir_all(parent)
                .wrap_err_with(|| format!("creating overlay dir {}", parent.display()))?;
        }

        let ssh_port = ports::allocate_port().await?;
        let proxy_port = ports::allocate_port().await?;
        let ssh_key_path = runtime_dir.join("id_ed25519");

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

    /// Clean up all session resources: kill child processes, remove runtime dir
    /// AND the overlay file (which lives separately from the runtime dir).
    pub async fn cleanup(self) -> Result<()> {
        if let Some(pid) = self.qemu_pid {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
        // Give processes a moment to terminate gracefully
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        remove_session_artifacts(&self.runtime_dir, &self.overlay_path)?;
        Ok(())
    }
}

/// Remove both per-session artifacts: the runtime subdir (tmpfs) and the
/// overlay file (disk). Pulled out so `Session::cleanup` and the prune path
/// share exactly one definition of "what a session owns on disk."
pub fn remove_session_artifacts(runtime_dir: &std::path::Path, overlay_path: &std::path::Path) -> Result<()> {
    if overlay_path.exists() {
        std::fs::remove_file(overlay_path)
            .wrap_err_with(|| format!("removing overlay {}", overlay_path.display()))?;
    }
    if runtime_dir.exists() {
        std::fs::remove_dir_all(runtime_dir)
            .wrap_err_with(|| format!("removing runtime dir {}", runtime_dir.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `session_layout` pins the qcow2 under `overlay_dir()`, not under
    /// `runtime_dir`. This is the load-bearing invariant of slice 1 — if the
    /// overlay lives anywhere under `runtime_dir`, tmpfs fills again.
    #[test]
    fn layout_puts_overlay_outside_runtime_dir() {
        let prior_overlay = std::env::var("SEGURO_OVERLAY_DIR").ok();
        let prior_xdg = std::env::var("XDG_RUNTIME_DIR").ok();

        std::env::set_var("XDG_RUNTIME_DIR", "/tmp/seguro-layout-test/run");
        std::env::set_var("SEGURO_OVERLAY_DIR", "/tmp/seguro-layout-test/overlays");

        let layout = session_layout("sess-abc");
        assert_eq!(
            layout.runtime_dir,
            PathBuf::from("/tmp/seguro-layout-test/run/seguro/sess-abc"),
        );
        assert_eq!(
            layout.overlay_path,
            PathBuf::from("/tmp/seguro-layout-test/overlays/sess-abc.qcow2"),
        );
        assert!(
            !layout.overlay_path.starts_with(&layout.runtime_dir),
            "overlay lives under runtime dir — tmpfs regression",
        );

        match prior_overlay {
            Some(v) => std::env::set_var("SEGURO_OVERLAY_DIR", v),
            None => std::env::remove_var("SEGURO_OVERLAY_DIR"),
        }
        match prior_xdg {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
    }

    /// `remove_session_artifacts` removes both the runtime subdir and the
    /// overlay file, even when they live under different roots.
    #[test]
    fn cleanup_removes_both_runtime_dir_and_overlay_file() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("run/sess-xyz");
        let overlay_root = tmp.path().join("overlays");
        let overlay = overlay_root.join("sess-xyz.qcow2");

        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(runtime.join("qemu.pid"), "0").unwrap();
        std::fs::create_dir_all(&overlay_root).unwrap();
        std::fs::write(&overlay, b"fake qcow2").unwrap();

        remove_session_artifacts(&runtime, &overlay).unwrap();

        assert!(!runtime.exists(), "runtime dir not removed");
        assert!(!overlay.exists(), "overlay file not removed");
    }

    /// Missing overlay (already cleaned) must not fail. Cleanup is best-effort
    /// and may race with a separate prune pass.
    #[test]
    fn cleanup_is_idempotent_on_missing_artifacts() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("run/nothing");
        let overlay = tmp.path().join("overlays/nothing.qcow2");
        // Neither exists.

        remove_session_artifacts(&runtime, &overlay).unwrap();
    }
}
