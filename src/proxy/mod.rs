pub mod ca;
pub mod filter;
pub mod log;

use color_eyre::eyre::Result;

use crate::cli::NetMode;
use crate::config::Config;

/// A running proxy server instance.
pub struct ProxyServer {
    pub port: u16,
    _task: tokio::task::JoinHandle<()>,
}

impl ProxyServer {
    /// Start the proxy server and return its listening port.
    pub async fn start(
        config: &Config,
        mode: &NetMode,
        tls_inspect: bool,
        session_id: &str,
    ) -> Result<Self> {
        let _ = (config, mode, tls_inspect, session_id);
        todo!("proxy server not yet implemented — tracked in QyyE")
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}
