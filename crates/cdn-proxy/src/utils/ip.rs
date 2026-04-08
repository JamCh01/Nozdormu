use ipnet::IpNet;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::OnceLock;

/// Default trusted proxy CIDRs (private networks).
const TRUSTED_PROXIES: &[&str] = &[
    "127.0.0.0/8",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "::1/128",
    "fc00::/7",
];

/// Pre-built default trusted proxy list (built once on first use).
static DEFAULT_TRUSTED: OnceLock<Vec<IpNet>> = OnceLock::new();

fn default_trusted() -> &'static [IpNet] {
    DEFAULT_TRUSTED.get_or_init(|| {
        TRUSTED_PROXIES
            .iter()
            .filter_map(|s| IpNet::from_str(s).ok())
            .collect()
    })
}

/// Extract the real client IP from X-Forwarded-For header.
///
/// Traverses the XFF chain from right to left, returning the first
/// IP that is NOT in the trusted proxy list.
///
/// This prevents spoofing: an attacker can prepend fake IPs to XFF,
/// but they can't control the rightmost entries added by trusted proxies.
pub fn real_ip_from_xff(
    xff: &str,
    remote_addr: IpAddr,
    extra_trusted: &[IpNet],
) -> IpAddr {
    let defaults = default_trusted();

    // Parse XFF: "client, proxy1, proxy2"
    let ips: Vec<&str> = xff.split(',').map(|s| s.trim()).collect();

    // Traverse from right to left
    for ip_str in ips.iter().rev() {
        if let Ok(ip) = IpAddr::from_str(ip_str) {
            if !is_trusted(ip, defaults) && !is_trusted(ip, extra_trusted) {
                return ip;
            }
        }
    }

    // All IPs in XFF are trusted → use remote_addr
    remote_addr
}

/// Check if an IP is a private/internal address.
pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                return true;
            }
            let segments = v6.segments();
            // Unique Local Address (fc00::/7): first byte is fc or fd
            let is_ula = (segments[0] & 0xfe00) == 0xfc00;
            // Link-local (fe80::/10): first 10 bits are 1111111010
            let is_link_local = (segments[0] & 0xffc0) == 0xfe80;
            is_ula || is_link_local
        }
    }
}

fn is_trusted(ip: IpAddr, trusted: &[IpNet]) -> bool {
    trusted.iter().any(|cidr| cidr.contains(&ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote() -> IpAddr {
        "203.0.113.1".parse().unwrap()
    }

    #[test]
    fn test_single_client() {
        let ip = real_ip_from_xff("1.2.3.4", remote(), &[]);
        assert_eq!(ip, "1.2.3.4".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn test_client_behind_proxy() {
        // Client → trusted proxy → CDN
        let ip = real_ip_from_xff("1.2.3.4, 10.0.0.1", remote(), &[]);
        assert_eq!(ip, "1.2.3.4".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn test_spoofed_xff() {
        // Attacker prepends fake IP, but real client is 5.6.7.8
        let ip = real_ip_from_xff("1.1.1.1, 5.6.7.8, 10.0.0.1", remote(), &[]);
        assert_eq!(ip, "5.6.7.8".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn test_all_trusted_uses_remote() {
        let ip = real_ip_from_xff("10.0.0.1, 192.168.1.1", remote(), &[]);
        assert_eq!(ip, remote());
    }

    #[test]
    fn test_empty_xff_uses_remote() {
        let ip = real_ip_from_xff("", remote(), &[]);
        assert_eq!(ip, remote());
    }

    #[test]
    fn test_extra_trusted() {
        let extra = vec![IpNet::from_str("203.0.113.0/24").unwrap()];
        // 203.0.113.50 is in extra trusted, so skip it
        let ip = real_ip_from_xff("1.2.3.4, 203.0.113.50", remote(), &extra);
        assert_eq!(ip, "1.2.3.4".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn test_is_private() {
        assert!(is_private_ip("127.0.0.1".parse().unwrap()));
        assert!(is_private_ip("10.0.0.1".parse().unwrap()));
        assert!(is_private_ip("192.168.1.1".parse().unwrap()));
        assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
        // IPv6
        assert!(is_private_ip("::1".parse().unwrap()));           // loopback
        assert!(is_private_ip("fc00::1".parse().unwrap()));       // ULA
        assert!(is_private_ip("fd12:3456::1".parse().unwrap())); // ULA
        assert!(is_private_ip("fe80::1".parse().unwrap()));       // link-local
        assert!(!is_private_ip("2001:db8::1".parse().unwrap())); // public
    }
}
