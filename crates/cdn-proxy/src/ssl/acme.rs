/// ACME provider configuration.
#[derive(Debug, Clone)]
pub struct AcmeProvider {
    pub name: String,
    pub directory_url: String,
    pub eab_kid: Option<String>,
    pub eab_hmac_key: Option<String>,
}

impl AcmeProvider {
    /// Get the directory URL for a known provider.
    pub fn from_name(name: &str, staging: bool) -> Option<Self> {
        let (prod_url, staging_url) = match name.to_lowercase().as_str() {
            "letsencrypt" => (
                "https://acme-v02.api.letsencrypt.org/directory",
                "https://acme-staging-v02.api.letsencrypt.org/directory",
            ),
            "zerossl" => (
                "https://acme.zerossl.com/v2/DV90",
                "https://acme.zerossl.com/v2/DV90", // ZeroSSL has no staging
            ),
            "buypass" => (
                "https://api.buypass.com/acme/directory",
                "https://api.test4.buypass.no/acme/directory",
            ),
            "google" => (
                "https://dv.acme-v02.api.pki.goog/directory",
                "https://dv.acme-v02.test.pki.goog/directory",
            ),
            _ => return None,
        };

        Some(Self {
            name: name.to_lowercase(),
            directory_url: if staging { staging_url } else { prod_url }.to_string(),
            eab_kid: None,
            eab_hmac_key: None,
        })
    }

    /// Set EAB credentials (required for ZeroSSL and Google).
    pub fn with_eab(mut self, kid: Option<String>, hmac_key: Option<String>) -> Self {
        self.eab_kid = kid;
        self.eab_hmac_key = hmac_key;
        self
    }

    /// Check if this provider requires EAB.
    pub fn requires_eab(&self) -> bool {
        matches!(self.name.as_str(), "zerossl" | "google")
    }
}

/// ACME client for certificate issuance.
///
/// Uses `instant-acme` crate for the ACME v2 protocol.
/// Supports multi-provider rotation: tries each provider in order.
///
/// Flow:
/// 1. Acquire distributed lock (etcd)
/// 2. Get ACME directory
/// 3. Create/load account (shared via etcd)
/// 4. Create order for domain(s)
/// 5. Authorize via HTTP-01 challenge
/// 6. Submit CSR
/// 7. Download certificate
/// 8. Store in etcd + filesystem
pub struct AcmeClient {
    providers: Vec<AcmeProvider>,
    email: Option<String>,
}

impl AcmeClient {
    /// Create a new ACME client with the given providers.
    pub fn new(providers: Vec<AcmeProvider>, email: Option<String>) -> Self {
        Self { providers, email }
    }

    /// Build providers from node config.
    pub fn from_config(
        provider_names: &[String],
        staging: bool,
        email: Option<String>,
        eab: &cdn_config::EabCredentials,
    ) -> Self {
        let providers: Vec<AcmeProvider> = provider_names
            .iter()
            .filter_map(|name| {
                let mut provider = AcmeProvider::from_name(name, staging)?;
                // Attach EAB credentials
                match name.to_lowercase().as_str() {
                    "zerossl" => {
                        provider = provider
                            .with_eab(eab.zerossl_kid.clone(), eab.zerossl_hmac_key.clone());
                    }
                    "google" => {
                        provider =
                            provider.with_eab(eab.google_kid.clone(), eab.google_hmac_key.clone());
                    }
                    _ => {}
                }
                Some(provider)
            })
            .collect();

        Self::new(providers, email)
    }

    /// Get the list of configured providers.
    pub fn providers(&self) -> &[AcmeProvider] {
        &self.providers
    }

    /// Get the configured email.
    pub fn email(&self) -> Option<&str> {
        self.email.as_deref()
    }

    /// Issue a certificate for the given domain(s).
    ///
    /// Tries each provider in order until one succeeds.
    /// Returns (cert_pem, key_pem, chain_pem, provider_name).
    ///
    /// NOTE: Actual ACME protocol implementation requires:
    /// - instant-acme crate for protocol handling
    /// - ChallengeStore for HTTP-01 token serving
    /// - etcd for distributed locking and account storage
    /// These will be wired in when infrastructure is available.
    pub async fn issue(&self, _domains: &[String]) -> Result<IssuedCert, AcmeError> {
        for provider in &self.providers {
            // Skip providers that require EAB but don't have credentials
            if provider.requires_eab()
                && (provider.eab_kid.is_none() || provider.eab_hmac_key.is_none())
            {
                log::info!(
                    "[ACME] skipping {} (missing EAB credentials)",
                    provider.name
                );
                continue;
            }

            log::info!(
                "[ACME] attempting issuance with provider={} directory={}",
                provider.name,
                provider.directory_url
            );

            // TODO: Actual ACME protocol flow using instant-acme
            // 1. Account::create or load from etcd
            // 2. Order::new(domains)
            // 3. For each authorization: get HTTP-01 challenge, set in ChallengeStore
            // 4. Validate challenges
            // 5. Finalize with CSR
            // 6. Download certificate

            // For now, return an error indicating the provider was attempted
            log::warn!(
                "[ACME] provider {} not yet implemented (requires instant-acme integration)",
                provider.name
            );
        }

        Err(AcmeError::AllProvidersFailed)
    }
}

/// Result of a successful certificate issuance.
pub struct IssuedCert {
    pub cert_pem: String,
    pub key_pem: String,
    pub chain_pem: Option<String>,
    pub provider: String,
}

#[derive(Debug)]
pub enum AcmeError {
    AllProvidersFailed,
    ProviderError(String, String), // (provider, error)
    AccountError(String),
    OrderError(String),
    ChallengeError(String),
}

impl std::fmt::Display for AcmeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcmeError::AllProvidersFailed => write!(f, "all ACME providers failed"),
            AcmeError::ProviderError(p, e) => write!(f, "ACME provider {} error: {}", p, e),
            AcmeError::AccountError(e) => write!(f, "ACME account error: {}", e),
            AcmeError::OrderError(e) => write!(f, "ACME order error: {}", e),
            AcmeError::ChallengeError(e) => write!(f, "ACME challenge error: {}", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_from_name() {
        let p = AcmeProvider::from_name("letsencrypt", false).unwrap();
        assert!(p.directory_url.contains("letsencrypt.org"));
        assert!(!p.requires_eab());
    }

    #[test]
    fn test_provider_staging() {
        let p = AcmeProvider::from_name("letsencrypt", true).unwrap();
        assert!(p.directory_url.contains("staging"));
    }

    #[test]
    fn test_provider_eab_required() {
        let p = AcmeProvider::from_name("zerossl", false).unwrap();
        assert!(p.requires_eab());
        let p = AcmeProvider::from_name("google", false).unwrap();
        assert!(p.requires_eab());
    }

    #[test]
    fn test_unknown_provider() {
        assert!(AcmeProvider::from_name("unknown", false).is_none());
    }

    #[test]
    fn test_all_providers() {
        for name in &["letsencrypt", "zerossl", "buypass", "google"] {
            assert!(AcmeProvider::from_name(name, false).is_some());
            assert!(AcmeProvider::from_name(name, true).is_some());
        }
    }
}
