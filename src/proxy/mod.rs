pub mod ai_usage;
pub mod ca;
pub mod filter;
pub mod log;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use color_eyre::eyre::{Result, WrapErr};
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Bytes;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use crate::cli::NetMode;
use crate::config::Config;
use ai_usage::{ApiUsageLog, ApiUsageRecord, ProviderMap};
use ca::Ca;
use filter::{FilterVerdict, check_ssrf, is_api_only_allowed, is_explicitly_denied};
use log::{RequestLog, RequestRecord};

// ── Public API ────────────────────────────────────────────────────────────────

/// Atomic counters for proxy traffic, shared between proxy task and Sandbox.
#[derive(Debug, Default)]
pub struct ProxyStats {
    pub requests: AtomicU64,
    pub blocked: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub bytes_received: AtomicU64,
    /// AI API requests (subset of total requests).
    pub ai_requests: AtomicU64,
    /// Total input tokens across all AI API requests.
    pub ai_input_tokens: AtomicU64,
    /// Total output tokens across all AI API requests.
    pub ai_output_tokens: AtomicU64,
    /// Total cache-read tokens across all AI API requests.
    pub ai_cache_read_tokens: AtomicU64,
}

/// A running proxy server.
pub struct ProxyServer {
    pub port: u16,
    _task: tokio::task::JoinHandle<()>,
    /// PEM-encoded CA certificate if TLS inspection is active; None otherwise.
    ca_cert_pem: Option<String>,
    /// Shared traffic counters — readable by Sandbox::usage().
    pub stats: Arc<ProxyStats>,
}

impl ProxyServer {
    /// Returns the PEM CA cert when --tls-inspect is active, or None.
    pub fn ca_cert_pem(&self) -> Option<&str> {
        self.ca_cert_pem.as_deref()
    }
}

impl ProxyServer {
    /// Bind to a random port, start the proxy task, return the server handle.
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

        let stats = Arc::new(ProxyStats::default());
        let state = Arc::new(ProxyState::new(config, mode, tls_inspect, session_dir, Arc::clone(&stats))?);
        let ca_cert_pem = state.ca_cert_pem().map(|s| s.to_owned());

        let task = tokio::spawn(async move {
            run_proxy(listener, state).await;
        });

        tracing::info!(port, "proxy server started");
        Ok(Self { port, _task: task, ca_cert_pem, stats })
    }
}

// ── Internal state ────────────────────────────────────────────────────────────

struct ProxyState {
    mode: ProxyMode,
    allow_hosts: Vec<String>,
    deny_hosts: Vec<String>,
    log: RequestLog,
    /// Present when --tls-inspect was requested; used to sign per-domain certs.
    ca: Option<Arc<Ca>>,
    /// Shared atomic counters for traffic metering.
    stats: Arc<ProxyStats>,
    /// AI API provider hostname map.
    providers: ProviderMap,
    /// Per-session API usage log (only created when TLS inspection is active).
    ai_log: Option<ApiUsageLog>,
    /// Allow loopback addresses through SSRF filter (for testing / dev-bridge
    /// scenarios with localhost services).
    allow_loopback: bool,
    /// Skip upstream TLS certificate verification during MITM (for testing
    /// with self-signed upstream servers).
    danger_skip_upstream_verify: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ProxyMode {
    AirGapped,
    ApiOnly,
    FullOutbound,
    DevBridge,
}

impl ProxyState {
    fn new(config: &Config, mode: &NetMode, tls_inspect: bool, session_dir: &Path, stats: Arc<ProxyStats>) -> Result<Self> {
        let pmode = match mode {
            NetMode::AirGapped => ProxyMode::AirGapped,
            NetMode::ApiOnly => ProxyMode::ApiOnly,
            NetMode::FullOutbound => ProxyMode::FullOutbound,
            NetMode::DevBridge => ProxyMode::DevBridge,
        };

        let ca = if tls_inspect {
            Some(Arc::new(Ca::generate().wrap_err("generating TLS inspection CA")?))
        } else {
            None
        };

        let log = RequestLog::open(session_dir).wrap_err("opening proxy log")?;

        let providers = ProviderMap::new(&config.proxy.ai_providers);

        // Only create the API usage log when TLS inspection is active (we need
        // decrypted response bodies to extract token counts).
        let ai_log = if tls_inspect {
            Some(ApiUsageLog::open(session_dir).wrap_err("opening API usage log")?)
        } else {
            None
        };

        Ok(Self {
            mode: pmode,
            allow_hosts: config.proxy.api_only.allow.hosts.clone(),
            deny_hosts: config.proxy.deny.hosts.clone(),
            log,
            ca,
            stats,
            providers,
            ai_log,
            allow_loopback: config.proxy.allow_loopback.unwrap_or(false),
            danger_skip_upstream_verify: config.proxy.danger_skip_upstream_verify.unwrap_or(false),
        })
    }

    /// Returns the CA cert PEM if TLS inspection is active, or None.
    pub fn ca_cert_pem(&self) -> Option<&str> {
        self.ca.as_deref().map(|ca| ca.cert_pem())
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
                let verdict = check_ssrf(host, port, self.allow_loopback).await;
                if verdict != FilterVerdict::Allow {
                    return verdict;
                }
            }
        }

        // 3. For api-only, still run SSRF check after allow-list pass
        if self.mode == ProxyMode::ApiOnly {
            let verdict = check_ssrf(host, port, self.allow_loopback).await;
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
        // Update atomic counters
        self.stats.requests.fetch_add(1, Ordering::Relaxed);
        if blocked {
            self.stats.blocked.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(b) = bytes {
            self.stats.bytes_received.fetch_add(b, Ordering::Relaxed);
        }

        let ai_provider = self.providers.lookup(host).is_some();

        let record = RequestRecord {
            ts: RequestRecord::now(),
            method: method.to_string(),
            host: host.to_string(),
            path: path.to_string(),
            status,
            bytes,
            blocked,
            block_reason: reason,
            ai_provider,
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

    // Branch: TLS inspection (MITM) or blind tunnel
    if let Some(ca) = state.ca.clone() {
        handle_connect_mitm(req, host, port, ca, state)
    } else {
        handle_connect_tunnel(req, host, port)
    }
}

/// Blind TCP tunnel — default behaviour when --tls-inspect is off.
fn handle_connect_tunnel(
    req: Request<hyper::body::Incoming>,
    host: String,
    port: u16,
) -> Result<Response<BoxBody>, hyper::Error> {
    tokio::task::spawn(async move {
        match hyper::upgrade::on(req).await {
            Ok(upgraded) => {
                let remote_addr = format!("{}:{}", host, port);
                match tokio::net::TcpStream::connect(&remote_addr).await {
                    Ok(server) => {
                        let mut client_io = TokioIo::new(upgraded);
                        let mut server_io = server;
                        if let Err(e) =
                            tokio::io::copy_bidirectional(&mut client_io, &mut server_io).await
                        {
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

/// TLS MITM tunnel — used when --tls-inspect is on.
///
/// Accepts TLS from the client using a CA-signed leaf cert, connects to the
/// upstream server over TLS, then proxies individual HTTP/1.1 requests so
/// that full URLs and response codes can be logged.
fn handle_connect_mitm(
    req: Request<hyper::body::Incoming>,
    host: String,
    port: u16,
    ca: Arc<Ca>,
    state: Arc<ProxyState>,
) -> Result<Response<BoxBody>, hyper::Error> {
    tokio::task::spawn(async move {
        match hyper::upgrade::on(req).await {
            Ok(upgraded) => {
                if let Err(e) = mitm_tunnel(host, port, ca, upgraded, state).await {
                    tracing::debug!("MITM tunnel error: {}", e);
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

async fn mitm_tunnel(
    host: String,
    port: u16,
    ca: Arc<Ca>,
    upgraded: hyper::upgrade::Upgraded,
    state: Arc<ProxyState>,
) -> color_eyre::eyre::Result<()> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

    // Sign a leaf cert for this host
    let (cert_der, key_der) = ca.sign_for_host(&host)?;
    let cert = CertificateDer::from(cert_der);
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der));

    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .wrap_err("building TLS server config")?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

    // Accept TLS from the client
    let client_io = TokioIo::new(upgraded);
    let tls_client = acceptor.accept(client_io).await.wrap_err("TLS accept from client")?;

    // Build a reusable TLS client config for upstream connections
    let upstream_tls_config = if state.danger_skip_upstream_verify {
        let cfg = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
            .with_no_client_auth();
        Arc::new(cfg)
    } else {
        let root_store = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth(),
        )
    };

    // Serve HTTP/1.1 on the decrypted client stream, forwarding each request upstream
    let host_arc = Arc::new(host);
    let state_arc = Arc::clone(&state);
    let tls_cfg = Arc::clone(&upstream_tls_config);

    let service = hyper::service::service_fn(move |req: Request<hyper::body::Incoming>| {
        let host = Arc::clone(&host_arc);
        let state = Arc::clone(&state_arc);
        let tls_cfg = Arc::clone(&tls_cfg);
        async move { forward_inspected(req, &host, port, state, tls_cfg).await }
    });

    hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(tls_client), service)
        .await
        .wrap_err("serving MITM HTTP/1.1")
}

/// Forward a single decrypted HTTPS request to the upstream server and return its response.
async fn forward_inspected(
    req: Request<hyper::body::Incoming>,
    host: &str,
    port: u16,
    state: Arc<ProxyState>,
    tls_cfg: Arc<rustls::ClientConfig>,
) -> Result<Response<BoxBody>, hyper::Error> {
    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .to_string();
    let method = req.method().to_string();
    let timer = ai_usage::start_timer();

    // Check if this is an AI API host before we consume the request
    let provider = state.providers.lookup(host);

    // Measure request body size
    let content_length = req
        .headers()
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    // For AI API hosts, collect the request body to measure its size,
    // then rebuild the request.
    let (req, request_bytes) = if provider.is_some() && content_length.is_none() {
        // No Content-Length header — collect body to measure
        let (parts, body) = req.into_parts();
        match body.collect().await {
            Ok(collected) => {
                let bytes = collected.to_bytes();
                let len = bytes.len() as u64;
                let new_body = Full::new(bytes).map_err(|never| match never {}).boxed();
                (Request::from_parts(parts, new_body), len)
            }
            Err(e) => {
                tracing::warn!("failed to read request body: {}", e);
                return Ok(Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(full_body("502 Bad Gateway"))
                    .unwrap());
            }
        }
    } else {
        let rb = content_length.unwrap_or(0);
        let req = req.map(|b| b.map_err(|e| e).boxed());
        (req, rb)
    };

    // Connect to upstream over TCP + TLS
    let tcp = match tokio::net::TcpStream::connect(format!("{}:{}", host, port)).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("upstream TCP connect failed for {}: {}", host, e);
            state.log_request(&method, host, &path, Some(502), None, false, None);
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(full_body("502 Bad Gateway"))
                .unwrap());
        }
    };

    let server_name = match rustls::pki_types::ServerName::try_from(host.to_string()) {
        Ok(n) => n,
        Err(_) => {
            state.log_request(&method, host, &path, Some(502), None, false, None);
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(full_body("502 Bad Gateway"))
                .unwrap());
        }
    };

    let connector = tokio_rustls::TlsConnector::from(tls_cfg);
    let tls_stream = match connector.connect(server_name, tcp).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("upstream TLS handshake failed for {}: {}", host, e);
            state.log_request(&method, host, &path, Some(502), None, false, None);
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(full_body("502 Bad Gateway"))
                .unwrap());
        }
    };

    let io = TokioIo::new(tls_stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;
    tokio::spawn(conn);

    match sender.send_request(req).await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let latency_ms = ai_usage::elapsed_ms(timer);

            // For AI API hosts: buffer the response body for token extraction,
            // then re-wrap it for the client.
            if let (Some(prov), Some(ai_log)) = (provider, state.ai_log.as_ref()) {
                let is_streaming = resp
                    .headers()
                    .get(hyper::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(|ct| ct.contains("text/event-stream"))
                    .unwrap_or(false);

                let (parts, body) = resp.into_parts();

                match body.collect().await {
                    Ok(collected) => {
                        let body_bytes = collected.to_bytes();
                        let response_bytes = body_bytes.len() as u64;

                        // Extract usage from the response body
                        let usage = if is_streaming {
                            // For SSE, find the last `data:` line with content
                            extract_sse_usage(prov, &body_bytes)
                        } else {
                            ai_usage::extract_usage(prov, &body_bytes)
                        };

                        // Update atomic counters
                        state.stats.ai_requests.fetch_add(1, Ordering::Relaxed);
                        if let Some(t) = usage.input_tokens {
                            state.stats.ai_input_tokens.fetch_add(t, Ordering::Relaxed);
                        }
                        if let Some(t) = usage.output_tokens {
                            state.stats.ai_output_tokens.fetch_add(t, Ordering::Relaxed);
                        }
                        if let Some(t) = usage.cache_read_tokens {
                            state.stats.ai_cache_read_tokens.fetch_add(t, Ordering::Relaxed);
                        }

                        // Write usage log record
                        let record = ApiUsageRecord {
                            ts: log::RequestRecord::now(),
                            provider: prov,
                            model: usage.model,
                            endpoint: path.clone(),
                            latency_ms,
                            request_bytes,
                            response_bytes,
                            input_tokens: usage.input_tokens,
                            output_tokens: usage.output_tokens,
                            cache_read_tokens: usage.cache_read_tokens,
                            cache_creation_tokens: usage.cache_creation_tokens,
                            status,
                            streaming: is_streaming,
                        };
                        if let Err(e) = ai_log.write(&record) {
                            tracing::warn!("failed to write API usage log: {}", e);
                        }

                        state.log_request(&method, host, &path, Some(status), Some(response_bytes), false, None);
                        let new_body = Full::new(body_bytes).map_err(|never| match never {}).boxed();
                        Ok(Response::from_parts(parts, new_body))
                    }
                    Err(e) => {
                        tracing::warn!("failed to read AI API response body: {}", e);
                        state.log_request(&method, host, &path, Some(status), None, false, None);
                        Ok(Response::from_parts(parts, empty_body()))
                    }
                }
            } else {
                // Not an AI API host — pass through without buffering
                state.log_request(&method, host, &path, Some(status), None, false, None);
                Ok(resp.map(|b| b.map_err(|e| e).boxed()))
            }
        }
        Err(e) => {
            tracing::warn!("upstream request failed for {}: {}", host, e);
            state.log_request(&method, host, &path, Some(502), None, false, None);
            Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(full_body("502 Bad Gateway"))
                .unwrap())
        }
    }
}

/// Extract usage from a buffered SSE response by finding the last meaningful
/// `data:` line that contains a usage object.
fn extract_sse_usage(provider: ai_usage::Provider, body: &[u8]) -> ai_usage::ExtractedUsage {
    let text = String::from_utf8_lossy(body);
    let mut last_usage_data: Option<&str> = None;

    for line in text.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if data == "[DONE]" {
                continue;
            }
            // Check if this data line contains a usage field
            if data.contains("\"usage\"") {
                last_usage_data = Some(data);
            }
        }
    }

    if let Some(data) = last_usage_data {
        ai_usage::extract_usage_from_sse(provider, data.as_bytes())
    } else {
        ai_usage::ExtractedUsage::default()
    }
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

// ── TLS helpers ──────────────────────────────────────────────────────────────

/// Certificate verifier that accepts any cert. Used when
/// `danger_skip_upstream_verify` is set (testing with self-signed upstreams).
#[derive(Debug)]
struct AcceptAnyCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

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
