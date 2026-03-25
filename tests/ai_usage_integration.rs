//! Integration tests for AI API usage capture.
//!
//! These tests start a real `ProxyServer` with TLS inspection, a fake HTTPS
//! upstream mimicking an AI API provider, and verify that `api-usage.jsonl`
//! records are written with correct token counts.
//!
//! No QEMU or KVM required — runs on every `cargo test` invocation.

use std::sync::Arc;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Start a proxy WITHOUT TLS inspection.
async fn start_proxy_no_tls(
) -> (seguro::proxy::ProxyServer, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = seguro::config::Config::default();
    let proxy = seguro::proxy::ProxyServer::start(
        &config,
        &seguro::cli::NetMode::FullOutbound,
        false,
        dir.path(),
    )
    .await
    .unwrap();
    (proxy, dir)
}

/// Spawn a fake HTTPS server on localhost that returns `response_body` with the
/// given `content_type`. Returns the port it's listening on and the DER cert
/// for the server (self-signed).
async fn fake_https_server(
    response_body: &'static str,
    content_type: &'static str,
    status: u16,
) -> (u16, Vec<u8>, tokio::task::JoinHandle<()>) {
    // Generate a self-signed cert for localhost
    let params = rcgen::CertificateParams::new(vec!["127.0.0.1".into()]).unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    let cert_der = cert.der().to_vec();
    let key_der = key.serialize_der();

    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert_der.clone())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der)),
        )
        .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

    let handle = tokio::spawn(async move {
        // Accept one connection, serve one request, exit
        let (stream, _) = listener.accept().await.unwrap();
        let tls_stream = acceptor.accept(stream).await.unwrap();
        let io = TokioIo::new(tls_stream);

        let service = hyper::service::service_fn(move |_req| {
            let body = response_body;
            let ct = content_type;
            let st = status;
            async move {
                Ok::<_, hyper::Error>(
                    hyper::Response::builder()
                        .status(st)
                        .header("content-type", ct)
                        .body(Full::new(Bytes::from(body)))
                        .unwrap(),
                )
            }
        });

        hyper::server::conn::http1::Builder::new()
            .serve_connection(io, service)
            .await
            .ok();
    });

    (port, cert_der, handle)
}

/// Send a CONNECT + POST through the MITM proxy to a given host:port, using
/// raw TCP. The proxy will TLS-terminate and re-connect to upstream.
///
/// We send a POST (like a real AI API call) and read back the full response.
async fn post_through_proxy(
    proxy_port: u16,
    target_host: &str,
    target_port: u16,
    path: &str,
    body: &str,
) -> String {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", proxy_port))
        .await
        .unwrap();

    // 1. CONNECT
    let connect_req = format!(
        "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\n\r\n",
        target_host, target_port, target_host, target_port
    );
    stream.write_all(connect_req.as_bytes()).await.unwrap();

    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await.unwrap();
    let connect_resp = String::from_utf8_lossy(&buf[..n]);
    assert!(
        connect_resp.starts_with("HTTP/1.1 200"),
        "CONNECT failed: {}",
        connect_resp
    );

    // 2. TLS handshake with the proxy's MITM cert (trust any cert)
    let mut client_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
        .with_no_client_auth();
    client_config.alpn_protocols = vec![b"http/1.1".to_vec()];

    let connector = tokio_rustls::TlsConnector::from(Arc::new(client_config));
    let server_name =
        rustls::pki_types::ServerName::try_from(target_host.to_string()).unwrap();
    let tls_stream = connector.connect(server_name, stream).await.unwrap();

    // 3. HTTP POST over the TLS connection
    let io = TokioIo::new(tls_stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await.unwrap();
    tokio::spawn(conn);

    let req = hyper::Request::builder()
        .method("POST")
        .uri(path)
        .header("host", format!("{}:{}", target_host, target_port))
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap();

    let resp = sender.send_request(req).await.unwrap();
    let status = resp.status();
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    format!("{} {}", status.as_u16(), String::from_utf8_lossy(&body_bytes))
}

/// A rustls certificate verifier that accepts anything (for test clients
/// connecting through the MITM proxy).
#[derive(Debug)]
struct AcceptAnyCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
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
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::ED448,
        ]
    }
}

/// Read api-usage.jsonl from a session dir and return parsed records.
fn read_usage_records(dir: &std::path::Path) -> Vec<serde_json::Value> {
    let path = dir.join("api-usage.jsonl");
    if !path.exists() {
        return vec![];
    }
    let content = std::fs::read_to_string(&path).unwrap();
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// Non-streaming Anthropic response: verify token extraction.
#[tokio::test]
async fn ai_usage_anthropic_non_streaming() {
    let response_body = r#"{
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": "Hello"}],
        "model": "claude-sonnet-4-20250514",
        "usage": {
            "input_tokens": 1520,
            "output_tokens": 380,
            "cache_read_input_tokens": 1000,
            "cache_creation_input_tokens": 0
        }
    }"#;

    let (upstream_port, _cert, _server) =
        fake_https_server(response_body, "application/json", 200).await;

    // Map 127.0.0.1 to "anthropic" so the provider map matches our localhost server.
    let dir2 = tempfile::tempdir().unwrap();
    let mut config = seguro::config::Config::default();
    config.proxy.allow_loopback = Some(true);
    config.proxy.danger_skip_upstream_verify = Some(true);
    config.proxy.ai_providers.insert(
        "anthropic".into(),
        vec!["127.0.0.1".into()],
    );
    let proxy = seguro::proxy::ProxyServer::start(
        &config,
        &seguro::cli::NetMode::FullOutbound,
        true,
        dir2.path(),
    )
    .await
    .unwrap();

    let resp = post_through_proxy(
        proxy.port,
        "127.0.0.1",
        upstream_port,
        "/v1/messages",
        r#"{"model":"claude-sonnet-4-20250514","messages":[{"role":"user","content":"hi"}]}"#,
    )
    .await;

    assert!(resp.starts_with("200"), "expected 200, got: {}", resp);

    // Give the log writer a moment to flush
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let records = read_usage_records(dir2.path());
    assert_eq!(records.len(), 1, "expected 1 usage record, got: {:?}", records);

    let r = &records[0];
    assert_eq!(r["provider"], "anthropic");
    assert_eq!(r["model"], "claude-sonnet-4-20250514");
    assert_eq!(r["input_tokens"], 1520);
    assert_eq!(r["output_tokens"], 380);
    assert_eq!(r["cache_read_tokens"], 1000);
    assert_eq!(r["endpoint"], "/v1/messages");
    assert_eq!(r["status"], 200);
    assert_eq!(r["streaming"], false);
    assert!(r["latency_ms"].as_u64().unwrap() > 0);
    assert!(r["request_bytes"].as_u64().unwrap() > 0);
    assert!(r["response_bytes"].as_u64().unwrap() > 0);
}

/// Non-streaming OpenAI response: verify token extraction.
#[tokio::test]
async fn ai_usage_openai_non_streaming() {
    let response_body = r#"{
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "model": "gpt-4o",
        "choices": [{"message": {"role": "assistant", "content": "Hello"}}],
        "usage": {
            "prompt_tokens": 800,
            "completion_tokens": 200,
            "total_tokens": 1000
        }
    }"#;

    let (upstream_port, _cert, _server) =
        fake_https_server(response_body, "application/json", 200).await;

    let dir = tempfile::tempdir().unwrap();
    let mut config = seguro::config::Config::default();
    config.proxy.allow_loopback = Some(true);
    config.proxy.danger_skip_upstream_verify = Some(true);
    config.proxy.ai_providers.insert(
        "openai".into(),
        vec!["127.0.0.1".into()],
    );
    let proxy = seguro::proxy::ProxyServer::start(
        &config,
        &seguro::cli::NetMode::FullOutbound,
        true,
        dir.path(),
    )
    .await
    .unwrap();

    let resp = post_through_proxy(
        proxy.port,
        "127.0.0.1",
        upstream_port,
        "/v1/chat/completions",
        r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#,
    )
    .await;

    assert!(resp.starts_with("200"), "expected 200, got: {}", resp);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let records = read_usage_records(dir.path());
    assert_eq!(records.len(), 1);

    let r = &records[0];
    assert_eq!(r["provider"], "openai");
    assert_eq!(r["model"], "gpt-4o");
    assert_eq!(r["input_tokens"], 800);
    assert_eq!(r["output_tokens"], 200);
    assert_eq!(r["endpoint"], "/v1/chat/completions");
}

/// Streaming SSE response with usage in the final event.
#[tokio::test]
async fn ai_usage_streaming_sse() {
    // Anthropic streaming format: multiple data lines, usage in message_delta
    let response_body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-sonnet-4-20250514\",\"usage\":{\"input_tokens\":1200,\"cache_read_input_tokens\":500}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"Hello\"}}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":450}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    let (upstream_port, _cert, _server) =
        fake_https_server(response_body, "text/event-stream", 200).await;

    let dir = tempfile::tempdir().unwrap();
    let mut config = seguro::config::Config::default();
    config.proxy.allow_loopback = Some(true);
    config.proxy.danger_skip_upstream_verify = Some(true);
    config.proxy.ai_providers.insert(
        "anthropic".into(),
        vec!["127.0.0.1".into()],
    );
    let proxy = seguro::proxy::ProxyServer::start(
        &config,
        &seguro::cli::NetMode::FullOutbound,
        true,
        dir.path(),
    )
    .await
    .unwrap();

    let resp = post_through_proxy(
        proxy.port,
        "127.0.0.1",
        upstream_port,
        "/v1/messages",
        r#"{"model":"claude-sonnet-4-20250514","stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
    )
    .await;

    assert!(resp.starts_with("200"), "expected 200, got: {}", resp);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let records = read_usage_records(dir.path());
    assert_eq!(records.len(), 1, "expected 1 record, got: {:?}", records);

    let r = &records[0];
    assert_eq!(r["provider"], "anthropic");
    assert_eq!(r["streaming"], true);
    // The last data line with "usage" is the message_delta with output_tokens
    assert_eq!(r["output_tokens"], 450);
}

/// Requests to non-AI hosts should NOT produce api-usage.jsonl entries.
#[tokio::test]
async fn ai_usage_non_ai_host_no_record() {
    let response_body = r#"{"ok": true}"#;

    let (upstream_port, _cert, _server) =
        fake_https_server(response_body, "application/json", 200).await;

    let dir = tempfile::tempdir().unwrap();
    // Default config — 127.0.0.1 is NOT in the AI provider map
    let mut config = seguro::config::Config::default();
    config.proxy.allow_loopback = Some(true);
    config.proxy.danger_skip_upstream_verify = Some(true);
    let proxy = seguro::proxy::ProxyServer::start(
        &config,
        &seguro::cli::NetMode::FullOutbound,
        true,
        dir.path(),
    )
    .await
    .unwrap();

    let resp = post_through_proxy(
        proxy.port,
        "127.0.0.1",
        upstream_port,
        "/api/something",
        r#"{"data": "hello"}"#,
    )
    .await;

    assert!(resp.starts_with("200"), "expected 200, got: {}", resp);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let records = read_usage_records(dir.path());
    assert_eq!(
        records.len(),
        0,
        "non-AI host should not produce usage records, got: {:?}",
        records
    );
}

/// Without --tls-inspect, api-usage.jsonl should not be created at all.
#[tokio::test]
async fn ai_usage_no_tls_inspect_no_file() {
    let (_proxy, dir) = start_proxy_no_tls().await;

    // The file should not exist even before any requests
    let path = dir.path().join("api-usage.jsonl");
    assert!(
        !path.exists(),
        "api-usage.jsonl should not exist without --tls-inspect"
    );
}

/// Verify that ProxyStats atomic counters are updated for AI requests.
#[tokio::test]
async fn ai_usage_updates_proxy_stats() {
    let response_body = r#"{
        "model": "claude-sonnet-4-20250514",
        "usage": {
            "input_tokens": 500,
            "output_tokens": 100,
            "cache_read_input_tokens": 200,
            "cache_creation_input_tokens": 0
        }
    }"#;

    let (upstream_port, _cert, _server) =
        fake_https_server(response_body, "application/json", 200).await;

    let dir = tempfile::tempdir().unwrap();
    let mut config = seguro::config::Config::default();
    config.proxy.allow_loopback = Some(true);
    config.proxy.danger_skip_upstream_verify = Some(true);
    config.proxy.ai_providers.insert(
        "anthropic".into(),
        vec!["127.0.0.1".into()],
    );
    let proxy = seguro::proxy::ProxyServer::start(
        &config,
        &seguro::cli::NetMode::FullOutbound,
        true,
        dir.path(),
    )
    .await
    .unwrap();

    post_through_proxy(
        proxy.port,
        "127.0.0.1",
        upstream_port,
        "/v1/messages",
        r#"{"messages":[]}"#,
    )
    .await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    use std::sync::atomic::Ordering::Relaxed;
    assert_eq!(proxy.stats.ai_requests.load(Relaxed), 1);
    assert_eq!(proxy.stats.ai_input_tokens.load(Relaxed), 500);
    assert_eq!(proxy.stats.ai_output_tokens.load(Relaxed), 100);
    assert_eq!(proxy.stats.ai_cache_read_tokens.load(Relaxed), 200);
}
