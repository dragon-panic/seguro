//! Integration tests for the proxy server.
//!
//! These tests start a real `ProxyServer` in-process and send HTTP requests
//! through it via async TCP. No QEMU or KVM is required — they run on every
//! `cargo test` invocation.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn start_proxy(mode: seguro::cli::NetMode) -> (seguro::proxy::ProxyServer, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = seguro::config::Config::default();
    let proxy = seguro::proxy::ProxyServer::start(&config, &mode, false, dir.path())
        .await
        .unwrap();
    (proxy, dir)
}

/// Send a CONNECT request and return the first response line.
async fn connect_status(proxy_port: u16, target: &str) -> String {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", proxy_port))
        .await
        .unwrap();
    let request = format!("CONNECT {} HTTP/1.1\r\nHost: {}\r\n\r\n", target, target);
    stream.write_all(request.as_bytes()).await.unwrap();

    let mut buf = vec![0u8; 1024];
    let n = stream.read(&mut buf).await.unwrap();
    let response = String::from_utf8_lossy(&buf[..n]);
    response.lines().next().unwrap_or("").to_string()
}

/// Send a plain HTTP GET and return the first response line.
async fn get_status(proxy_port: u16, host: &str, path: &str) -> String {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", proxy_port))
        .await
        .unwrap();
    let request = format!(
        "GET http://{}{} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        host, path, host
    );
    stream.write_all(request.as_bytes()).await.unwrap();

    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await.unwrap();
    let response = String::from_utf8_lossy(&buf[..n]);
    response.lines().next().unwrap_or("").to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn air_gapped_blocks_connect() {
    let (proxy, _dir) = start_proxy(seguro::cli::NetMode::AirGapped).await;
    let status = connect_status(proxy.port, "example.com:443").await;
    assert!(status.contains("403"), "expected 403, got: {}", status);
}

#[tokio::test]
async fn air_gapped_blocks_http() {
    let (proxy, _dir) = start_proxy(seguro::cli::NetMode::AirGapped).await;
    let status = get_status(proxy.port, "example.com", "/").await;
    assert!(status.contains("403"), "expected 403, got: {}", status);
}

#[tokio::test]
async fn ssrf_connect_blocked_in_full_outbound() {
    let (proxy, _dir) = start_proxy(seguro::cli::NetMode::FullOutbound).await;
    // 10.0.2.2 is the SLIRP gateway — always in SSRF block list
    let status = connect_status(proxy.port, "10.0.2.2:443").await;
    assert!(status.contains("403"), "expected 403 for SSRF target, got: {}", status);
}

#[tokio::test]
async fn ssrf_http_blocked_in_full_outbound() {
    let (proxy, _dir) = start_proxy(seguro::cli::NetMode::FullOutbound).await;
    let status = get_status(proxy.port, "192.168.1.1", "/").await;
    assert!(status.contains("403"), "expected 403 for RFC1918 HTTP, got: {}", status);
}

#[tokio::test]
async fn api_only_blocks_unlisted_connect() {
    let (proxy, _dir) = start_proxy(seguro::cli::NetMode::ApiOnly).await;
    let status = connect_status(proxy.port, "evil.example.com:443").await;
    assert!(status.contains("403"), "expected 403 for unlisted domain, got: {}", status);
}

/// api-only: CONNECT to api.anthropic.com (default allow list) passes the filter.
/// The proxy returns 200 before attempting upstream; upstream failure is irrelevant here.
#[tokio::test]
async fn api_only_allows_listed_domain() {
    let (proxy, _dir) = start_proxy(seguro::cli::NetMode::ApiOnly).await;
    let status = connect_status(proxy.port, "api.anthropic.com:443").await;
    assert!(
        !status.contains("403"),
        "filter should pass for api.anthropic.com, got: {}",
        status
    );
}
