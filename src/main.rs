mod cli;
mod commands;
mod config;
mod proxy;
mod session;
mod vm;

use clap::Parser;
use cli::{Cli, Commands};
use color_eyre::eyre::{Result, eyre};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    startup_checks()?;

    let cli = Cli::parse();
    match cli.command {
        Commands::Run(args) => commands::run::execute(args).await,
        Commands::Shell(args) => commands::shell::execute(args).await,
        Commands::Sessions(args) => commands::sessions::execute(args).await,
        Commands::Snapshot(args) => commands::snapshot::execute(args).await,
        Commands::Images(args) => commands::images::execute(args).await,
        Commands::ProxyLog(args) => commands::proxy_log::execute(args).await,
    }
}

fn startup_checks() -> Result<()> {
    use std::process::Command;

    // 1. qemu-system-x86_64 >= 7.2
    match Command::new("qemu-system-x86_64").arg("--version").output() {
        Ok(out) => {
            let version_str = String::from_utf8_lossy(&out.stdout);
            check_qemu_version(&version_str)?;
        }
        Err(_) => {
            return Err(eyre!(
                "qemu-system-x86_64 not found on $PATH.\n\
                 Install QEMU >= 7.2 (e.g. `sudo pacman -S qemu-full` or \
                 `sudo apt install qemu-system-x86`)."
            ));
        }
    }

    // 2. virtiofsd
    if Command::new("virtiofsd").arg("--version").output().is_err() {
        return Err(eyre!(
            "virtiofsd not found on $PATH.\n\
             Install virtiofsd (e.g. `sudo pacman -S virtiofsd` or \
             `sudo apt install virtiofsd`)."
        ));
    }

    // 3. /dev/kvm (non-fatal — TCG fallback)
    if !std::path::Path::new("/dev/kvm").exists() {
        tracing::warn!(
            "/dev/kvm is not accessible. QEMU will run in TCG (software emulation) \
             mode — expect 5–10× slower boot and execution. \
             Enable KVM in your BIOS or add yourself to the 'kvm' group."
        );
    }

    Ok(())
}

fn check_qemu_version(output: &str) -> Result<()> {
    // Expected: "QEMU emulator version 8.2.0\n..."
    let version_line = output.lines().next().unwrap_or("");
    let ver_str = version_line
        .split_whitespace()
        .nth(3)
        .unwrap_or("0.0.0");

    let parts: Vec<u32> = ver_str
        .split('.')
        .take(2)
        .filter_map(|s| s.parse().ok())
        .collect();

    match parts.as_slice() {
        [major, minor] if *major > 7 || (*major == 7 && *minor >= 2) => Ok(()),
        _ => Err(eyre!(
            "QEMU version {} is too old. seguro requires >= 7.2.\n\
             Upgrade QEMU to continue.",
            ver_str
        )),
    }
}
