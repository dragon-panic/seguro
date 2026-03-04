use color_eyre::eyre::{Result, eyre};
use std::path::Path;

/// Create a qcow2 copy-on-write overlay on top of `base`.
pub async fn create_overlay(base: &Path, overlay: &Path) -> Result<()> {
    let base_str = base
        .to_str()
        .ok_or_else(|| eyre!("non-UTF8 base image path"))?;
    let overlay_str = overlay
        .to_str()
        .ok_or_else(|| eyre!("non-UTF8 overlay path"))?;

    let status = tokio::process::Command::new("qemu-img")
        .args(["create", "-f", "qcow2", "-b", base_str, "-F", "qcow2", overlay_str])
        .status()
        .await?;

    if !status.success() {
        return Err(eyre!("qemu-img create overlay failed (exit {})", status));
    }
    Ok(())
}

/// Compact and convert `src` to `dst` using qcow2 compression.
pub async fn compact(src: &Path, dst: &Path) -> Result<()> {
    let status = tokio::process::Command::new("qemu-img")
        .args([
            "convert",
            "-c",
            "-O", "qcow2",
            src.to_str().unwrap(),
            dst.to_str().unwrap(),
        ])
        .status()
        .await?;

    if !status.success() {
        return Err(eyre!("qemu-img convert failed (exit {})", status));
    }
    Ok(())
}

/// List all *.qcow2 files in the images directory with their sizes.
pub fn list_images(images_dir: &Path) -> Result<Vec<(std::path::PathBuf, u64)>> {
    let mut images = Vec::new();
    if !images_dir.exists() {
        return Ok(images);
    }
    for entry in std::fs::read_dir(images_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("qcow2") {
            let size = entry.metadata()?.len();
            images.push((path, size));
        }
    }
    images.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(images)
}
