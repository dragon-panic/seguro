pub mod ca;
pub mod filter;
pub mod log;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use color_eyre::eyre::{Result, WrapErr};
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Bytes;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use crate::cli::NetMode;
use crate::config::Config;
use filter::{FilterVerdict, check_ssrf, is_api_only_allowed, is_explicitly_denied};
use log::{RequestLog, RequestRecord};

// ── Public API ────────────────────────────────────────────────────────────────

/// A running proxy server.
pub struct ProxyServer {
    pub port: u16,
    _task: tokio::task::JoinHandle<()>,
}

impl ProxyServer {
    /// Bind to a random port, start the proxy task, and return the port.
    pub async fn start(
        config: &Config,
        mode: &NetMode,
        tls_inspect: bool,
        session_dir: &Path,
    ) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .wrap_err("binding proxy port")?;
        let port = listener.local_addr()?.port();

        let state = Arc::new(ProxyState::new(config, mode, tls_inspect, session_dir)?);

        let task = tokio::spawn(async move {
            run_proxy(listener, state).await;
        });

        tracing::info!(port, "proxy server started");
        Ok(Self { port, _task: task })
    }
}

// ── Internal state ────────────────────────────────────────────────────────────

struct ProxyState {
    mode: ProxyMode,
    allow_hosts: Vec<String>,
    deny_hosts: Vec<String>,
    log: RequestLog,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ProxyMode {
    AirGapped,
    ApiOnly,
    FullOutbound,
    DevBridge,
}

impl ProxyState {
    fn new(config: &Config, mode: &NetMode, _tls_inspect: bool, session_dir: &Path) -> Result<Self> {
        let pmode = match mode {
            NetMode::AirGapped => ProxyMode::AirGapped,
            NetMode::ApiOnly => ProxyMode::ApiOnly,
            NetMode::FullOutbound => ProxyMode::FullOutbound,
            NetMode::DevBridge => ProxyMode::DevBridge,
        };

        let log = RequestLog::open(session_dir).wrap_err("opening proxy log")?;

        Ok(Self {
            mode: pmode,
            allow_hosts: config.proxy.api_only.allow.hosts.clone(),
            deny_hosts: config.proxy.deny.hosts.clone(),
            log,
        })
    }

    /// Evaluate whether a request to `host:port` should be allowed.
    async fn filter(&self, host: &str, port: u16) -> FilterVerdict {
        // 1. Explicit deny list (all modes)
        if is_explicitly_denied(host, &self.deny_hosts) {
            return FilterVerdict::Deny(format!("host {} is in the deny list", host));
        }

        // 2. Mode-level decision
        match self.mode {
            ProxyMode::AirGapped => {
                return FilterVerdict::Deny("air-gapped mode: all outbound blocked".into());
            }
            ProxyMode::ApiOnly => {
                if !is_api_only_allowed(host, &self.allow_hosts) {
                    return FilterVerdict::Deny(format!(
                        "api-only mode: {} is not in the allow list",
                        host
                    ));
                }
            }
            ProxyMode::FullOutbound | ProxyMode::DevBridge => {
                // SSRF check (always on)
                let verdict = check_ssrf(host, port).await;
                if verdict != FilterVerdict::Allow {
                    return verdict;
                }
            }
        }

        // 3. For api-only, still run SSRF check after allow-list pass
        if self.mode == ProxyMode::ApiOnly {
            let verdict = check_ssrf(host, port).await;
            if verdict != FilterVerdict::Allow {
                return verdict;
            }
        }

        FilterVerdict::Allow
    }

    fn log_request(
        &self,
        method: &str,
        host: &str,
        path: &str,
        status: Option<u16>,
        bytes: Option<u64>,
        blocked: bool,
        reason: Option<String>,
    ) {
        let record = RequestRecord {
            ts: RequestRecord::now(),
            method: method.to_string(),
            host: host.to_string(),
            path: path.to_string(),
            status,
            bytes,
            blocked,
            block_reason: reason,
        };
        if let Err(e) = self.log.write(&record) {
            tracing::warn!("failed to write proxy log entry: {}", e);
        }
    }
}

// ── Server loop ───────────────────────────────────────────────────────────────

async fn run_proxy(listener: TcpListener, state: Arc<ProxyState>) {
    loop {
        match listener.accept().await {
            Ok((stream, _peer)) => {
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let service = hyper::service::service_fn(move |req| {
                        let state = Arc::clone(&state);
                        async move { handle_request(req, state).await }
                    });
                    if let Err(e) = hyper::server::conn::http1::Builder::new()
                        .preserve_header_case(true)
                        .title_case_headers(true)
                        .serve_connection(io, service)
                        .with_upgrades()
                        .await
                    {
                        tracing::debug!("proxy connection error: {}", e);
                    }
                });
            }
            Err(e) => {
                tracing::error!("proxy accept error: {}", e);
            }
        }
    }
}

// ── Request handler ───────────────────────────────────────────────────────────

type BoxBody = http_body_util::combinators::BoxBody<Bytes, hyper::Error>;

fn empty_body() -> BoxBody {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}

fn full_body(s: &'static str) -> BoxBody {
    Full::new(Bytes::from(s))
        .map_err(|never| match never {})
        .boxed()
}

async fn handle_request(
    req: Request<hyper::body::Incoming>,
    state: Arc<ProxyState>,
) -> Result<Response<BoxBody>, hyper::Error> {
    if req.method() == Method::CONNECT {
        handle_connect(req, state).await
    } else {
        handle_forward(req, state).await
    }
}

/// Handle HTTP CONNECT (HTTPS tunnel establishment).
async fn handle_connect(
    req: Request<hyper::body::Incoming>,
    state: Arc<ProxyState>,
) -> Result<Response<BoxBody>, hyper::Error> {
    let host_port = req.uri().authority().map(|a| a.to_string()).unwrap_or_default();
    let (host, port) = split_host_port(&host_port, 443);

    let verdict = state.filter(&host, port).await;
    if verdict != FilterVerdict::Allow {
        let reason = match &verdict { FilterVerdict::Deny(r) => r.clone(), _ => String::new() };
        tracing::info!(host = %host, reason = %reason, "CONNECT denied");
        state.log_request("CONNECT", &host, "-", Some(403), None, true, Some(reason));
        return Ok(Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(full_body("403 Forbidden"))
            .unwrap());
    }

    tracing::debug!(host = %host, port = port, "CONNECT allowed");
    state.log_request("CONNECT", &host, "-", Some(200), None, false, None);

    // Upgrade and tunnel
    tokio::task::spawn(async move {
        match hyper::upgrade::on(req).await {
            Ok(upgraded) => {
                let remote_addr = format!("{}:{}", host, port);
                match tokio::net::TcpStream::connect(&remote_addr).await {
                    Ok(server) => {
                        let mut client_io = TokioIo::new(upgraded);
                        let mut server_io = server;
                        if let Err(e) = tokio::io::copy_bidirectional(&mut client_io, &mut server_io).await {
                            tracing::debug!("tunnel {} closed: {}", remote_addr, e);
                        }
                    }
                    Err(e) => tracing::warn!("CONNECT to {} failed: {}", remote_addr, e),
                }
            }
            Err(e) => tracing::warn!("CONNECT upgrade failed: {}", e),
        }
    });

    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(empty_body())
        .unwrap())
}

/// Handle plain HTTP forward proxy requests.
async fn handle_forward(
    req: Request<hyper::body::Incoming>,
    state: Arc<ProxyState>,
) -> Result<Response<BoxBody>, hyper::Error> {
    let host = req
        .headers()
        .get(hyper::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let path = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/").to_string();
    let method = req.method().to_string();
    let (host_only, port) = split_host_port(&host, 80);

    let verdict = state.filter(&host_only, port).await;
    if verdict != FilterVerdict::Allow {
        let reason = match &verdict { FilterVerdict::Deny(r) => r.clone(), _ => String::new() };
        tracing::info!(host = %host_only, reason = %reason, "request denied");
        state.log_request(&method, &host_only, &path, Some(403), None, true, Some(reason));
        return Ok(Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(full_body("403 Forbidden"))
            .unwrap());
    }

    // Forward the request
    let target_addr: SocketAddr = match format!("{}:{}", host_only, port).parse::<SocketAddr>() {
        Ok(a) => a,
        Err(_) => {
            // Resolve via DNS
            match tokio::net::lookup_host(format!("{}:{}", host_only, port)).await {
                Ok(mut addrs) => match addrs.next() {
                    Some(a) => a,
                    None => {
                        state.log_request(&method, &host_only, &path, Some(502), None, false, None);
                        return Ok(Response::builder()
                            .status(StatusCode::BAD_GATEWAY)
                            .body(full_body("502 Bad Gateway"))
                            .unwrap());
                    }
                },
                Err(_) => {
                    state.log_request(&method, &host_only, &path, Some(502), None, false, None);
                    return Ok(Response::builder()
                        .status(StatusCode::BAD_GATEWAY)
                        .body(full_body("502 Bad Gateway"))
                        .unwrap());
                }
            }
        }
    };

    let stream = match tokio::net::TcpStream::connect(target_addr).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("forward connect to {} failed: {}", target_addr, e);
            state.log_request(&method, &host_only, &path, Some(502), None, false, None);
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(full_body("502 Bad Gateway"))
                .unwrap());
        }
    };

    // Strip proxy headers and forward
    let (mut parts, body) = req.into_parts();
    parts.headers.remove(hyper::header::PROXY_AUTHORIZATION);
    parts.headers.remove("proxy-connection");
    let outbound_req = Request::from_parts(parts, body);

    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;
    tokio::spawn(conn);

    match sender.send_request(outbound_req).await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            state.log_request(&method, &host_only, &path, Some(status), None, false, None);
            Ok(resp.map(|b| b.map_err(|e| e).boxed()))
        }
        Err(e) => {
            tracing::warn!("forward request failed: {}", e);
            state.log_request(&method, &host_only, &path, Some(502), None, false, None);
            Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(full_body("502 Bad Gateway"))
                .unwrap())
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Split "host:port" or "host" into (host, port), using `default_port` if absent.
fn split_host_port(authority: &str, default_port: u16) -> (String, u16) {
    if let Some(pos) = authority.rfind(':') {
        let port_str = &authority[pos + 1..];
        if let Ok(p) = port_str.parse::<u16>() {
            return (authority[..pos].to_string(), p);
        }
    }
    (authority.to_string(), default_port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_host_port_with_port() {
        assert_eq!(split_host_port("example.com:8080", 80), ("example.com".into(), 8080));
    }

    #[test]
    fn split_host_port_without_port() {
        assert_eq!(split_host_port("example.com", 80), ("example.com".into(), 80));
    }

    #[test]
    fn split_host_port_connect_style() {
        assert_eq!(split_host_port("api.github.com:443", 443), ("api.github.com".into(), 443));
    }
}
