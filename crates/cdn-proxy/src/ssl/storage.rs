use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Certificate data stored per domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertData {
    pub cert_pem: String,
    pub key_pem: String,
    pub chain_pem: Option<String>,
    pub expires_at: i64,
    pub provider: Option<String>,
    pub domains: Vec<String>,
}

impl CertData {
    /// Parse the certificate to extract expiry date.
    pub fn parse_expiry(cert_pem: &str) -> Option<i64> {
        let pem = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes())
            .ok()?
            .1;
        let cert = pem.parse_x509().ok()?;
        Some(cert.validity().not_after.timestamp())
    }

    /// Check if the certificate is expired.
    pub fn is_expired(&self) -> bool {
        let now = chrono::Utc::now().timestamp();
        now >= self.expires_at
    }

    /// Check if the certificate needs renewal (within `days` of expiry).
    pub fn needs_renewal(&self, days: u64) -> bool {
        let now = chrono::Utc::now().timestamp();
        let threshold = self.expires_at - (days as i64 * 86400);
        now >= threshold
    }

    /// Get the full chain PEM (cert + chain).
    pub fn fullchain_pem(&self) -> String {
        match &self.chain_pem {
            Some(chain) => format!("{}\n{}", self.cert_pem.trim(), chain.trim()),
            None => self.cert_pem.clone(),
        }
    }
}

/// Certificate storage: file-based with in-memory cache.
/// In production, etcd would be the primary store.
pub struct CertStorage {
    /// In-memory cache: domain → CertData
    cache: Arc<DashMap<String, CertData>>,
    /// Base directory for certificate files
    certs_dir: PathBuf,
}

impl CertStorage {
    pub fn new(certs_dir: &Path) -> Self {
        Self {
            cache: Arc::new(DashMap::new()),
            certs_dir: certs_dir.to_path_buf(),
        }
    }

    /// Load a certificate for a domain.
    /// Checks: memory cache → filesystem → None
    pub fn get(&self, domain: &str) -> Option<CertData> {
        // Memory cache
        if let Some(entry) = self.cache.get(domain) {
            return Some(entry.clone());
        }

        // Filesystem fallback
        if let Some(cert) = self.load_from_file(domain) {
            self.cache.insert(domain.to_string(), cert.clone());
            return Some(cert);
        }

        None
    }

    /// Store a certificate for a domain.
    pub fn put(&self, domain: &str, cert: CertData) {
        // Save to filesystem
        if let Err(e) = self.save_to_file(domain, &cert) {
            log::error!("[CertStorage] failed to save cert for {}: {}", domain, e);
        }
        // Update memory cache
        self.cache.insert(domain.to_string(), cert);
    }

    /// Remove a certificate for a domain.
    pub fn remove(&self, domain: &str) {
        self.cache.remove(domain);
        let dir = self.cert_dir(domain);
        if dir.exists() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// List all cached domains.
    pub fn list_domains(&self) -> Vec<String> {
        self.cache.iter().map(|e| e.key().clone()).collect()
    }

    /// Get the default certificate (if configured).
    pub fn get_default(&self) -> Option<CertData> {
        self.get("_default")
    }

    /// Find a wildcard certificate matching the domain.
    /// e.g., for "sub.example.com", look up "*.example.com"
    pub fn get_wildcard(&self, domain: &str) -> Option<CertData> {
        // Extract parent domain for wildcard lookup
        if let Some(dot_pos) = domain.find('.') {
            let wildcard = format!("*.{}", &domain[dot_pos + 1..]);
            return self.get(&wildcard);
        }
        None
    }

    /// Load all certificates from the filesystem into memory cache.
    pub fn load_all(&self) {
        let acme_dir = self.certs_dir.join("acme");
        let custom_dir = self.certs_dir.join("custom");
        let default_dir = self.certs_dir.join("default");

        self.load_dir(&acme_dir);
        self.load_dir(&custom_dir);

        // Load default cert
        if let Some(cert) = self.load_cert_from_dir(&default_dir) {
            self.cache.insert("_default".to_string(), cert);
        }
    }

    fn cert_dir(&self, domain: &str) -> PathBuf {
        self.certs_dir.join("acme").join(domain)
    }

    fn load_from_file(&self, domain: &str) -> Option<CertData> {
        let dir = self.cert_dir(domain);
        self.load_cert_from_dir(&dir)
    }

    fn load_cert_from_dir(&self, dir: &Path) -> Option<CertData> {
        let cert_path = dir.join("fullchain.pem");
        let key_path = dir.join("privkey.pem");

        let cert_pem = std::fs::read_to_string(&cert_path).ok()?;
        let key_pem = std::fs::read_to_string(&key_path).ok()?;

        let expires_at = CertData::parse_expiry(&cert_pem).unwrap_or(0);

        Some(CertData {
            cert_pem,
            key_pem,
            chain_pem: None,
            expires_at,
            provider: None,
            domains: vec![],
        })
    }

    fn save_to_file(&self, domain: &str, cert: &CertData) -> std::io::Result<()> {
        let dir = self.cert_dir(domain);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("fullchain.pem"), cert.fullchain_pem())?;

        // Write private key with restrictive permissions (0600)
        let key_path = dir.join("privkey.pem");
        std::fs::write(&key_path, &cert.key_pem)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
        }

        log::info!(
            "[CertStorage] saved cert for {} to {}",
            domain,
            dir.display()
        );
        Ok(())
    }

    fn load_dir(&self, dir: &Path) {
        if !dir.exists() {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let domain = entry.file_name().to_string_lossy().to_string();
                    if let Some(cert) = self.load_cert_from_dir(&entry.path()) {
                        self.cache.insert(domain, cert);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cert_data_expired() {
        let cert = CertData {
            cert_pem: String::new(),
            key_pem: String::new(),
            chain_pem: None,
            expires_at: 0, // epoch = expired
            provider: None,
            domains: vec![],
        };
        assert!(cert.is_expired());
    }

    #[test]
    fn test_cert_data_not_expired() {
        let cert = CertData {
            cert_pem: String::new(),
            key_pem: String::new(),
            chain_pem: None,
            expires_at: chrono::Utc::now().timestamp() + 86400 * 90,
            provider: None,
            domains: vec![],
        };
        assert!(!cert.is_expired());
    }

    #[test]
    fn test_needs_renewal() {
        let cert = CertData {
            cert_pem: String::new(),
            key_pem: String::new(),
            chain_pem: None,
            expires_at: chrono::Utc::now().timestamp() + 86400 * 20, // 20 days left
            provider: None,
            domains: vec![],
        };
        assert!(cert.needs_renewal(30)); // within 30 days
        assert!(!cert.needs_renewal(10)); // not within 10 days
    }

    #[test]
    fn test_fullchain_pem() {
        let cert = CertData {
            cert_pem: "CERT".to_string(),
            key_pem: "KEY".to_string(),
            chain_pem: Some("CHAIN".to_string()),
            expires_at: 0,
            provider: None,
            domains: vec![],
        };
        assert_eq!(cert.fullchain_pem(), "CERT\nCHAIN");
    }

    #[test]
    fn test_fullchain_no_chain() {
        let cert = CertData {
            cert_pem: "CERT".to_string(),
            key_pem: "KEY".to_string(),
            chain_pem: None,
            expires_at: 0,
            provider: None,
            domains: vec![],
        };
        assert_eq!(cert.fullchain_pem(), "CERT");
    }

    #[test]
    fn test_storage_memory_cache() {
        let storage = CertStorage::new(Path::new("/tmp/nozdormu-test-certs"));
        let cert = CertData {
            cert_pem: "CERT".to_string(),
            key_pem: "KEY".to_string(),
            chain_pem: None,
            expires_at: chrono::Utc::now().timestamp() + 86400,
            provider: Some("test".to_string()),
            domains: vec!["example.com".to_string()],
        };
        storage
            .cache
            .insert("example.com".to_string(), cert.clone());
        let loaded = storage.get("example.com").unwrap();
        assert_eq!(loaded.cert_pem, "CERT");
    }

    #[test]
    fn test_wildcard_lookup() {
        let storage = CertStorage::new(Path::new("/tmp/nozdormu-test-certs"));
        let cert = CertData {
            cert_pem: "WILDCARD".to_string(),
            key_pem: "KEY".to_string(),
            chain_pem: None,
            expires_at: chrono::Utc::now().timestamp() + 86400,
            provider: None,
            domains: vec![],
        };
        storage.cache.insert("*.example.com".to_string(), cert);
        let loaded = storage.get_wildcard("sub.example.com").unwrap();
        assert_eq!(loaded.cert_pem, "WILDCARD");
    }
}
