use ipnet::{IpNet, Ipv4Net, Ipv6Net};
use prefix_trie::PrefixSet;
use std::net::IpAddr;

/// LPM Trie-backed CIDR set for O(log n) IP lookups.
/// Uses separate tries for IPv4 and IPv6 since `prefix-trie` requires
/// concrete prefix types (`Ipv4Net`/`Ipv6Net`), not the combined `IpNet` enum.
pub struct IpCidrSet {
    v4: PrefixSet<Ipv4Net>,
    v6: PrefixSet<Ipv6Net>,
}

impl IpCidrSet {
    /// Build tries from a slice of CIDRs.
    pub fn new(cidrs: &[IpNet]) -> Self {
        let mut v4 = PrefixSet::new();
        let mut v6 = PrefixSet::new();
        for cidr in cidrs {
            match cidr {
                IpNet::V4(net) => { v4.insert(*net); }
                IpNet::V6(net) => { v6.insert(*net); }
            }
        }
        Self { v4, v6 }
    }

    /// Check if an IP address matches any CIDR in the set — O(log n).
    pub fn contains(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => {
                let host = Ipv4Net::from(v4);
                self.v4.get_lpm(&host).is_some()
            }
            IpAddr::V6(v6) => {
                let host = Ipv6Net::from(v6);
                self.v6.get_lpm(&host).is_some()
            }
        }
    }

    /// Return the longest matching prefix for the given IP, if any.
    pub fn longest_match(&self, ip: IpAddr) -> Option<IpNet> {
        match ip {
            IpAddr::V4(v4) => {
                let host = Ipv4Net::from(v4);
                self.v4.get_lpm(&host).map(|n| IpNet::V4(*n))
            }
            IpAddr::V6(v6) => {
                let host = Ipv6Net::from(v6);
                self.v6.get_lpm(&host).map(|n| IpNet::V6(*n))
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.v4.is_empty() && self.v6.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn net(s: &str) -> IpNet {
        IpNet::from_str(s).unwrap()
    }

    fn ip(s: &str) -> IpAddr {
        IpAddr::from_str(s).unwrap()
    }

    #[test]
    fn test_ipv4_exact() {
        let set = IpCidrSet::new(&[net("192.168.1.1/32")]);
        assert!(set.contains(ip("192.168.1.1")));
        assert!(!set.contains(ip("192.168.1.2")));
    }

    #[test]
    fn test_ipv4_cidr() {
        let set = IpCidrSet::new(&[net("10.0.0.0/8")]);
        assert!(set.contains(ip("10.0.0.1")));
        assert!(set.contains(ip("10.255.255.255")));
        assert!(!set.contains(ip("11.0.0.1")));
    }

    #[test]
    fn test_ipv6_cidr() {
        let set = IpCidrSet::new(&[net("2001:db8::/32")]);
        assert!(set.contains(ip("2001:db8::1")));
        assert!(!set.contains(ip("2001:db9::1")));
    }

    #[test]
    fn test_mixed_v4_v6() {
        let set = IpCidrSet::new(&[net("10.0.0.0/8"), net("::1/128")]);
        assert!(set.contains(ip("10.1.2.3")));
        assert!(set.contains(ip("::1")));
        assert!(!set.contains(ip("192.168.0.1")));
    }

    #[test]
    fn test_empty_set() {
        let set = IpCidrSet::new(&[]);
        assert!(!set.contains(ip("1.2.3.4")));
        assert!(set.is_empty());
    }

    #[test]
    fn test_longest_match_returns_most_specific() {
        let set = IpCidrSet::new(&[net("10.0.0.0/8"), net("10.0.0.0/16")]);
        let matched = set.longest_match(ip("10.0.0.1"));
        assert_eq!(matched, Some(net("10.0.0.0/16"))); // /16 is more specific
    }

    #[test]
    fn test_longest_match_specific_network() {
        let set = IpCidrSet::new(&[net("10.0.0.0/8"), net("192.168.0.0/16")]);
        let matched = set.longest_match(ip("192.168.1.1"));
        assert_eq!(matched, Some(net("192.168.0.0/16")));
    }

    #[test]
    fn test_no_match() {
        let set = IpCidrSet::new(&[net("10.0.0.0/8")]);
        assert!(set.longest_match(ip("192.168.0.1")).is_none());
    }

    #[test]
    fn test_overlapping_prefixes() {
        let set = IpCidrSet::new(&[
            net("10.0.0.0/8"),
            net("10.0.0.0/16"),
            net("10.0.0.0/24"),
        ]);
        // Should return the most specific match
        assert_eq!(set.longest_match(ip("10.0.0.1")), Some(net("10.0.0.0/24")));
        assert_eq!(set.longest_match(ip("10.0.1.1")), Some(net("10.0.0.0/16")));
        assert_eq!(set.longest_match(ip("10.1.0.1")), Some(net("10.0.0.0/8")));
        assert!(!set.contains(ip("11.0.0.1")));
    }
}
