use color_eyre::eyre::{Result, eyre};

use crate::cli::{ImagesArgs, ImagesBuildArgs, ImagesCommand};
use crate::config::{Config, images_dir};
use crate::session::image::list_images;

pub async fn execute(args: ImagesArgs) -> Result<()> {
    match args.command {
        ImagesCommand::Ls => list().await,
        ImagesCommand::Build(build_args) => build(build_args).await,
    }
}

async fn list() -> Result<()> {
    let dir = images_dir();
    let images = list_images(&dir)?;
    if images.is_empty() {
        println!("No base images found in {}.", dir.display());
        println!("Run `seguro images build` to create one.");
        return Ok(());
    }
    println!("{:<40} {:>10}", "IMAGE", "SIZE");
    for (path, bytes) in images {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        println!("{:<40} {:>10}", name, human_bytes(bytes));
    }
    Ok(())
}

async fn build(args: ImagesBuildArgs) -> Result<()> {
    let profile_name = args.effective_profile();
    let file_config = Config::load(None)?;
    let profile = file_config.profile(profile_name);

    // Locate the build script relative to the binary or via $SEGURO_SCRIPTS
    let script_path = find_build_script()?;

    let mut cmd = tokio::process::Command::new("bash");
    cmd.arg(&script_path);

    // Pass --browser for backwards compat with existing build script
    if profile.image_suffix.as_deref() == Some("browser") {
        cmd.arg("--browser");
    } else if let Some(ref suffix) = profile.image_suffix {
        cmd.args(["--suffix", suffix]);
    }

    tracing::info!(
        script = %script_path.display(),
        profile = profile_name,
        "building base image"
    );

    let status = cmd.status().await?;
    if !status.success() {
        return Err(eyre!("build-image.sh failed with exit code {}", status));
    }

    let image_name = crate::session::image::image_name(profile.image_suffix.as_deref());
    println!("Image '{}' built successfully.", image_name);
    Ok(())
}

fn find_build_script() -> Result<std::path::PathBuf> {
    // 1. Env override
    if let Ok(p) = std::env::var("SEGURO_SCRIPTS") {
        let path = std::path::PathBuf::from(p).join("build-image.sh");
        if path.exists() {
            return Ok(path);
        }
    }

    // 2. Next to the binary
    if let Ok(exe) = std::env::current_exe() {
        let path = exe.parent().unwrap_or(std::path::Path::new("."))
            .join("scripts")
            .join("build-image.sh");
        if path.exists() {
            return Ok(path);
        }
    }

    // 3. Relative to cwd (development)
    let dev_path = std::path::PathBuf::from("scripts/build-image.sh");
    if dev_path.exists() {
        return Ok(dev_path);
    }

    Err(eyre!(
        "build-image.sh not found. Set SEGURO_SCRIPTS=/path/to/scripts or \
         place scripts/build-image.sh next to the seguro binary."
    ))
}

fn human_bytes(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    if bytes >= MB {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    } else {
        format!("{} B", bytes)
    }
}
