use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "seguro", about = "Sandbox CLI coding agents inside a QEMU VM")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run a CLI coding agent in a sandboxed VM
    Run(RunArgs),
    /// Open a shell in a running session
    Shell(ShellArgs),
    /// Manage sessions
    Sessions(SessionsArgs),
    /// Manage named snapshots
    Snapshot(SnapshotArgs),
    /// Manage base images
    Images(ImagesArgs),
    /// View proxy request logs
    #[command(name = "proxy-log")]
    ProxyLog(ProxyLogArgs),
}

#[derive(Args)]
pub struct RunArgs {
    /// Directory to share with the VM (read-write); defaults to a temp dir
    #[arg(long)]
    pub share: Option<PathBuf>,

    /// Keep the session overlay and workspace after exit
    #[arg(long)]
    pub persistent: bool,

    /// VM profile to use (defines image, RAM, CPU, env). See [profiles.*] in config.
    #[arg(long)]
    pub profile: Option<String>,

    /// Alias for --profile browser (bumps RAM to 4G, SMP to 4, uses browser image)
    #[arg(long)]
    pub browser: bool,

    /// Network isolation mode
    #[arg(long, default_value = "full-outbound")]
    pub net: NetMode,

    /// Required when --net dev-bridge is used (acknowledges security risk)
    #[arg(long)]
    pub unsafe_dev_bridge: bool,

    /// Enable TLS inspection (MITM CA injected into guest, enables full URL logging)
    #[arg(long)]
    pub tls_inspect: bool,

    /// Kill the session after this many seconds
    #[arg(long, value_name = "SECONDS")]
    pub timeout: Option<u64>,

    /// Show boot progress, virtiofsd output, and session metadata
    #[arg(long, short)]
    pub verbose: bool,

    /// Agent command to run inside the VM (omit for an interactive shell)
    #[arg(last = true)]
    pub agent: Vec<String>,
}

impl RunArgs {
    /// Resolve the effective profile name from --profile / --browser flags.
    pub fn effective_profile(&self) -> &str {
        if let Some(ref p) = self.profile {
            p
        } else if self.browser {
            "browser"
        } else {
            "default"
        }
    }
}

impl ImagesBuildArgs {
    /// Resolve the effective profile name from --profile / --browser flags.
    pub fn effective_profile(&self) -> &str {
        if let Some(ref p) = self.profile {
            p
        } else if self.browser {
            "browser"
        } else {
            "default"
        }
    }
}

#[derive(ValueEnum, Clone, Debug)]
pub enum NetMode {
    /// No outbound connectivity whatsoever
    AirGapped,
    /// Allow only explicitly listed hosts (deny-default)
    ApiOnly,
    /// Allow all internet, block RFC1918/link-local (default)
    FullOutbound,
    /// Allow LAN access — DANGEROUS, requires --unsafe-dev-bridge
    DevBridge,
}

#[derive(Args)]
pub struct ShellArgs {
    /// Session ID to attach to (uses most recent session if omitted)
    pub session_id: Option<String>,
}

#[derive(Args)]
pub struct SessionsArgs {
    #[command(subcommand)]
    pub command: SessionsCommand,
}

#[derive(Subcommand)]
pub enum SessionsCommand {
    /// List active and saved sessions
    Ls,
    /// Remove orphaned session overlays and stale /run state
    Prune,
}

#[derive(Args)]
pub struct SnapshotArgs {
    #[command(subcommand)]
    pub command: SnapshotCommand,
}

#[derive(Subcommand)]
pub enum SnapshotCommand {
    /// Save the running session state as a named snapshot
    Save { name: String },
    /// Start a new session from a named snapshot
    Restore { name: String },
}

#[derive(Args)]
pub struct ImagesArgs {
    #[command(subcommand)]
    pub command: ImagesCommand,
}

#[derive(Subcommand)]
pub enum ImagesCommand {
    /// List available base images and their sizes
    Ls,
    /// Build base image(s)
    Build(ImagesBuildArgs),
}

#[derive(Args)]
pub struct ImagesBuildArgs {
    /// Profile to build the image for. See [profiles.*] in config.
    #[arg(long)]
    pub profile: Option<String>,

    /// Alias for --profile browser
    #[arg(long)]
    pub browser: bool,
}

#[derive(Args)]
pub struct ProxyLogArgs {
    /// Session ID whose log to tail (uses most recent session if omitted)
    pub session_id: Option<String>,
}
