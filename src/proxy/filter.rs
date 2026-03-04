use std::net::IpAddr;

/// SSRF block list — always enforced regardless of network mode.
///
/// Blocked ranges:
///   10.0.0.0/8       RFC 1918
///   172.16.0.0/12    RFC 1918
///   192.168.0.0/16   RFC 1918
///   127.0.0.0/8      Loopback
///   ::1              IPv6 loopback
///   169.254.0.0/16   Link-local (cloud metadata)
///   10.0.2.2         SLIRP gateway
pub fn is_ssrf_blocked(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(a) => {
            let o = a.octets();
            // 10.0.0.0/8
            if o[0] == 10 { return true; }
            // 172.16.0.0/12
            if o[0] == 172 && (o[1] & 0xf0) == 16 { return true; }
            // 192.168.0.0/16
            if o[0] == 192 && o[1] == 168 { return true; }
            // 127.0.0.0/8
            if o[0] == 127 { return true; }
            // 169.254.0.0/16 (link-local / cloud metadata)
            if o[0] == 169 && o[1] == 254 { return true; }
            false
        }
        IpAddr::V6(a) => a.is_loopback(),
    }
}

/// Evaluate whether `host` is permitted under the api-only allow list.
pub fn is_api_only_allowed(host: &str, allow_hosts: &[String]) -> bool {
    let host = host.trim_end_matches('.');
    allow_hosts.iter().any(|h| {
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
        assert!(is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))));
    }

    #[test]
    fn ssrf_allows_public() {
        assert!(!is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        assert!(!is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }

    #[test]
    fn api_only_allows_listed_host() {
        let allow = vec!["api.anthropic.com".into(), "github.com".into()];
        assert!(is_api_only_allowed("api.anthropic.com", &allow));
        assert!(is_api_only_allowed("github.com", &allow));
        assert!(!is_api_only_allowed("evil.com", &allow));
    }
}
