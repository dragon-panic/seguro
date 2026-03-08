use color_eyre::eyre::{Result, WrapErr, eyre};
use std::path::{Path, PathBuf};

/// Locate the base image to use for a session.
///
/// Search order:
///   1. `config_override` if provided
///   2. `~/.local/share/seguro/images/base.qcow2`
pub fn locate_base(browser: bool, config_override: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = config_override {
        if p.exists() {
            return Ok(p.to_path_buf());
        }
        return Err(eyre!("configured base image not found: {}", p.display()));
    }

    let name = if browser { "base-browser.qcow2" } else { "base.qcow2" };
    let path = crate::config::images_dir().join(name);
    if path.exists() {
        Ok(path)
    } else {
        Err(eyre!(
            "base image not found at {}.\n\
             Run `seguro images build{}` to create it.",
            path.display(),
            if browser { " --browser" } else { "" }
        ))
    }
}

/// Create a qcow2 copy-on-write overlay on top of `base`.
pub async fn create_overlay(base: &Path, overlay: &Path) -> Result<()> {
    let status = tokio::process::Command::new("qemu-img")
        .args([
            "create", "-q",
            "-f", "qcow2",
            "-b", base.to_str().ok_or_else(|| eyre!("non-UTF8 path"))?,
            "-F", "qcow2",
            overlay.to_str().ok_or_else(|| eyre!("non-UTF8 path"))?,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .wrap_err("launching qemu-img create")?;

    if !status.success() {
        return Err(eyre!("qemu-img create overlay failed ({})", status));
    }
    Ok(())
}

/// Save a snapshot of the running disk image.
///
/// Calls `qemu-img snapshot -c <name> <image>`.
pub async fn snapshot_save(image: &Path, name: &str) -> Result<()> {
    let status = tokio::process::Command::new("qemu-img")
        .args(["snapshot", "-c", name, image.to_str().unwrap()])
        .status()
        .await
        .wrap_err("launching qemu-img snapshot -c")?;

    if !status.success() {
        return Err(eyre!("qemu-img snapshot save '{}' failed ({})", name, status));
    }
    Ok(())
}

/// Restore a named snapshot into `target_overlay` from `base`.
///
/// Creates a fresh overlay then applies the snapshot.
pub async fn snapshot_restore(base: &Path, name: &str, target_overlay: &Path) -> Result<()> {
    create_overlay(base, target_overlay).await?;

    let status = tokio::process::Command::new("qemu-img")
        .args(["snapshot", "-a", name, target_overlay.to_str().unwrap()])
        .status()
        .await
        .wrap_err("launching qemu-img snapshot -a")?;

    if !status.success() {
        return Err(eyre!("qemu-img snapshot restore '{}' failed ({})", name, status));
    }
    Ok(())
}

/// List all *.qcow2 files in `images_dir` with their on-disk sizes.
pub fn list_images(images_dir: &Path) -> Result<Vec<(PathBuf, u64)>> {
    let mut out = Vec::new();
    if !images_dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(images_dir).wrap_err("reading images dir")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("qcow2") {
            let size = entry.metadata()?.len();
            out.push((path, size));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// List session overlay paths in `runtime_dir` that have no corresponding running QEMU process.
pub fn list_orphaned_overlays(runtime_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut orphans = Vec::new();
    if !runtime_dir.exists() {
        return Ok(orphans);
    }
    for entry in std::fs::read_dir(runtime_dir)? {
        let entry = entry?;
        let session_dir = entry.path();
        if !session_dir.is_dir() {
            continue;
        }
        let pid_file = session_dir.join("qemu.pid");
        let overlay = session_dir.join("session.qcow2");
        if overlay.exists() && !is_qemu_running(&pid_file) {
            orphans.push(session_dir);
        }
    }
    Ok(orphans)
}

fn is_qemu_running(pid_file: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(pid_file) else { return false; };
    let Ok(pid) = content.trim().parse::<i32>() else { return false; };
    // Check /proc/<pid>/comm for "qemu-system-x86" to confirm it's still our process
    let comm_path = format!("/proc/{}/comm", pid);
    std::fs::read_to_string(comm_path)
        .map(|s| s.trim().starts_with("qemu-system-x86"))
        .unwrap_or(false)
}
