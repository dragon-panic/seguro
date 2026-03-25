use std::net::IpAddr;

/// Result of a filter evaluation.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterVerdict {
    Allow,
    Deny(String),
}

/// SSRF block list — always enforced regardless of network mode.
///
/// Blocked ranges:
///   10.0.0.0/8        RFC 1918
///   172.16.0.0/12     RFC 1918
///   192.168.0.0/16    RFC 1918
///   127.0.0.0/8       Loopback
///   ::1               IPv6 loopback
///   169.254.0.0/16    Link-local (cloud metadata)
///   10.0.2.2          SLIRP gateway (always blocked even in dev-bridge)
pub fn is_ssrf_blocked(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(a) => {
            let o = a.octets();
            o[0] == 10
                || (o[0] == 172 && (o[1] & 0xf0) == 16)
                || (o[0] == 192 && o[1] == 168)
                || o[0] == 127
                || (o[0] == 169 && o[1] == 254)
        }
        IpAddr::V6(a) => a.is_loopback(),
    }
}

/// Resolve `host` to all IP addresses and check each against the SSRF block list.
/// When `allow_loopback` is true, 127.0.0.0/8 is not blocked (for testing or
/// dev-bridge scenarios with localhost services).
pub async fn check_ssrf(host: &str, port: u16, allow_loopback: bool) -> FilterVerdict {
    match tokio::net::lookup_host((host, port)).await {
        Ok(addrs) => {
            for addr in addrs {
                if allow_loopback && addr.ip().is_loopback() {
                    continue;
                }
                if is_ssrf_blocked(addr.ip()) {
                    return FilterVerdict::Deny(format!(
                        "SSRF: {} resolves to {} which is in a blocked range",
                        host,
                        addr.ip()
                    ));
                }
            }
            FilterVerdict::Allow
        }
        Err(e) => FilterVerdict::Deny(format!("DNS resolution failed for {}: {}", host, e)),
    }
}

/// Evaluate whether `host` is permitted under the api-only allow list.
/// Subdomains of listed hosts are also allowed.
pub fn is_api_only_allowed(host: &str, allow_hosts: &[String]) -> bool {
    let host = host.trim_end_matches('.');
    allow_hosts.iter().any(|h| {
        host == h.as_str() || host.ends_with(&format!(".{}", h))
    })
}

/// Check whether `host` appears in the supplemental deny list.
pub fn is_explicitly_denied(host: &str, deny_hosts: &[String]) -> bool {
    let host = host.trim_end_matches('.');
    deny_hosts.iter().any(|h| {
        host == h.as_str() || host.ends_with(&format!(".{}", h))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn ssrf_blocks_rfc1918() {
        assert!(is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(10, 0, 2, 2))));
        assert!(is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255))));
        assert!(is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))));
    }

    #[test]
    fn ssrf_allows_public() {
        assert!(!is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        assert!(!is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(172, 15, 0, 1))));
        assert!(!is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(172, 32, 0, 1))));
    }

    #[test]
    fn api_only_allows_listed_and_subdomains() {
        let allow = vec!["api.anthropic.com".into(), "github.com".into()];
        assert!(is_api_only_allowed("api.anthropic.com", &allow));
        assert!(is_api_only_allowed("github.com", &allow));
        assert!(is_api_only_allowed("api.github.com", &allow));
        assert!(!is_api_only_allowed("evil.com", &allow));
        assert!(!is_api_only_allowed("notgithub.com", &allow));
    }

    #[test]
    fn deny_list_blocks_host_and_subdomains() {
        let deny = vec!["malicious.com".into()];
        assert!(is_explicitly_denied("malicious.com", &deny));
        assert!(is_explicitly_denied("sub.malicious.com", &deny));
        assert!(!is_explicitly_denied("benign.com", &deny));
    }
}
