Rust HTTP/HTTPS proxy running as a tokio task inside the seguro process.

**ca.rs**: generate a CA keypair + self-signed cert with rcgen. Cache per-domain leaf certs (signed by the CA) in a DashMap. Write CA cert to /run/seguro/{id}/ca.crt for optional guest installation.

**filter.rs**: 
- SSRF block list (always on): RFC 1918, loopback, link-local, 10.0.2.2. Resolve hostname to IP before evaluating.
- Allow/deny list evaluation for api-only mode (from Config)
- Returns FilterVerdict: Allow | Deny(reason)

**log.rs**: write one JSONL line per request to /run/seguro/{id}/proxy.log: {timestamp, method, host, path, status, bytes}. Path is redacted for HTTPS without TLS inspection.

**mod.rs**: 
- Bind to a random localhost port, return the port number to the caller before starting
- full-outbound mode: forward proxy using hyper, no TLS termination (log SNI from CONNECT tunnel)
- api-only mode: same but run filter before forwarding; return 403 on deny
- --tls-inspect mode: use hudsucker for MITM; install CA in guest via rc.local snippet
- air-gapped mode: reject all CONNECT and forward requests with 403