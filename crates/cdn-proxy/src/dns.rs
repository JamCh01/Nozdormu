use moka::future::Cache;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// Async DNS resolver with moka cache (TTL 60s).
/// IP addresses are passed through without resolution.
pub struct DnsResolver {
    cache: Cache<String, IpAddr>,
    resolver: hickory_resolver::TokioAsyncResolver,
}

impl DnsResolver {
    pub fn new() -> Self {
        let cache = Cache::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(60))
            .build();

        let resolver = hickory_resolver::TokioAsyncResolver::tokio_from_system_conf()
            .unwrap_or_else(|_| {
                // Fallback to Google DNS if system config fails
                hickory_resolver::TokioAsyncResolver::tokio(
                    hickory_resolver::config::ResolverConfig::google(),
                    hickory_resolver::config::ResolverOpts::default(),
                )
            });

        Self { cache, resolver }
    }

    /// Resolve a host to an IP address.
    /// If the host is already an IP address, return it directly.
    /// Otherwise, perform async DNS resolution with caching.
    pub async fn resolve(&self, host: &str) -> Option<IpAddr> {
        // Fast path: already an IP address
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Some(ip);
        }

        // Check cache
        if let Some(ip) = self.cache.get(host).await {
            return Some(ip);
        }

        // Async DNS resolution
        match self.resolver.lookup_ip(host).await {
            Ok(response) => {
                if let Some(ip) = response.iter().next() {
                    self.cache.insert(host.to_string(), ip).await;
                    Some(ip)
                } else {
                    log::warn!("[DNS] no records for {}", host);
                    None
                }
            }
            Err(e) => {
                log::error!("[DNS] resolution failed for {}: {}", host, e);
                None
            }
        }
    }

    /// Resolve a host:port to a SocketAddr.
    pub async fn resolve_to_socket(&self, host: &str, port: u16) -> Option<SocketAddr> {
        self.resolve(host).await.map(|ip| SocketAddr::new(ip, port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_ip_passthrough_v4() {
        let resolver = DnsResolver::new();
        let ip = resolver.resolve("1.2.3.4").await.unwrap();
        assert_eq!(ip, "1.2.3.4".parse::<IpAddr>().unwrap());
    }

    #[tokio::test]
    async fn test_ip_passthrough_v6() {
        let resolver = DnsResolver::new();
        let ip = resolver.resolve("::1").await.unwrap();
        assert_eq!(ip, "::1".parse::<IpAddr>().unwrap());
    }

    #[tokio::test]
    async fn test_resolve_to_socket() {
        let resolver = DnsResolver::new();
        let addr = resolver.resolve_to_socket("127.0.0.1", 8080).await.unwrap();
        assert_eq!(addr, "127.0.0.1:8080".parse::<SocketAddr>().unwrap());
    }
}
