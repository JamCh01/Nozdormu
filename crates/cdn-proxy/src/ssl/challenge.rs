use dashmap::DashMap;
use std::sync::Arc;

/// In-memory store for ACME HTTP-01 challenge tokens.
///
/// Maps domain/token → key_authorization.
/// In production, this would be backed by etcd with lease TTL.
pub struct ChallengeStore {
    /// Key: "{domain}/{token}", Value: key_authorization
    challenges: Arc<DashMap<String, String>>,
}

impl ChallengeStore {
    pub fn new() -> Self {
        Self {
            challenges: Arc::new(DashMap::new()),
        }
    }

    /// Store a challenge response for a domain/token pair.
    pub fn set_challenge(&self, domain: &str, token: &str, key_authorization: &str) {
        let key = format!("{}/{}", domain, token);
        self.challenges.insert(key, key_authorization.to_string());
        log::info!("[ACME] challenge set: domain={} token={}", domain, token);
    }

    /// Get the challenge response for a domain/token pair.
    pub fn get_challenge(&self, domain: &str, token: &str) -> Option<String> {
        let key = format!("{}/{}", domain, token);
        self.challenges.get(&key).map(|v| v.clone())
    }

    /// Remove a challenge after verification.
    pub fn remove_challenge(&self, domain: &str, token: &str) {
        let key = format!("{}/{}", domain, token);
        self.challenges.remove(&key);
    }

    /// Get challenge by path (extracts domain from host and token from path).
    /// Path format: /.well-known/acme-challenge/{token}
    pub fn get_by_path(&self, host: &str, path: &str) -> Option<String> {
        let token = path.strip_prefix("/.well-known/acme-challenge/")?;
        if token.is_empty() {
            return None;
        }
        // Validate token: ACME tokens are base64url — only allow [A-Za-z0-9_-]
        // Reject '/', '.', and other characters to prevent key collision across domains
        if !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return None;
        }
        // Strip port from host
        let domain = host.split(':').next().unwrap_or(host);
        self.get_challenge(domain, token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_and_get() {
        let store = ChallengeStore::new();
        store.set_challenge("example.com", "token123", "auth_value");
        assert_eq!(
            store.get_challenge("example.com", "token123"),
            Some("auth_value".to_string())
        );
    }

    #[test]
    fn test_missing_challenge() {
        let store = ChallengeStore::new();
        assert!(store.get_challenge("example.com", "missing").is_none());
    }

    #[test]
    fn test_remove_challenge() {
        let store = ChallengeStore::new();
        store.set_challenge("example.com", "token123", "auth_value");
        store.remove_challenge("example.com", "token123");
        assert!(store.get_challenge("example.com", "token123").is_none());
    }

    #[test]
    fn test_get_by_path() {
        let store = ChallengeStore::new();
        store.set_challenge("example.com", "abc123", "key_auth");
        assert_eq!(
            store.get_by_path("example.com", "/.well-known/acme-challenge/abc123"),
            Some("key_auth".to_string())
        );
    }

    #[test]
    fn test_get_by_path_with_port() {
        let store = ChallengeStore::new();
        store.set_challenge("example.com", "abc123", "key_auth");
        assert_eq!(
            store.get_by_path("example.com:80", "/.well-known/acme-challenge/abc123"),
            Some("key_auth".to_string())
        );
    }

    #[test]
    fn test_get_by_path_invalid() {
        let store = ChallengeStore::new();
        assert!(store.get_by_path("example.com", "/other/path").is_none());
        assert!(store
            .get_by_path("example.com", "/.well-known/acme-challenge/")
            .is_none());
    }

    #[test]
    fn test_get_by_path_rejects_traversal() {
        let store = ChallengeStore::new();
        // Token with slash — could cause cross-domain key collision
        store.set_challenge("a/b", "tok", "secret");
        assert!(store
            .get_by_path("a", "/.well-known/acme-challenge/b/tok")
            .is_none());
        // Token with dot-dot
        assert!(store
            .get_by_path("example.com", "/.well-known/acme-challenge/../../../etc")
            .is_none());
        // Token with dot
        assert!(store
            .get_by_path("example.com", "/.well-known/acme-challenge/foo.bar")
            .is_none());
    }
}
