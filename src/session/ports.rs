use color_eyre::eyre::{Result, eyre};
use std::net::TcpListener;

/// Allocate a free host port by binding to :0 and immediately releasing.
/// The caller must use the port promptly before another process claims it.
pub async fn allocate_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| eyre!("failed to bind port: {}", e))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}
