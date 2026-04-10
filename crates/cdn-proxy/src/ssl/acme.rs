use crate::logging::metrics::{ACME_ISSUANCE_DURATION, ACME_ISSUANCE_TOTAL};
use crate::ssl::challenge::ChallengeStore;
use crate::utils::redis_pool::RedisPool;
use cdn_common::RedisOps;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, ExternalAccountKey,
    Identifier, NewAccount, NewOrder, OrderStatus,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
/// Uses `instant-acme` crate for the ACME v2 protocol (RFC 8555).
/// Supports multi-provider rotation: tries each provider in order.
///
/// Flow per provider:
/// 1. Get or create ACME account (persisted in Redis)
/// 2. Create order for domain(s)
/// 3. Authorize via HTTP-01 challenge (tokens stored in ChallengeStore)
/// 4. Generate CSR via rcgen
/// 5. Finalize order
/// 6. Download certificate
pub struct AcmeClient {
    providers: Vec<AcmeProvider>,
    email: Option<String>,
    challenge_store: Arc<ChallengeStore>,
    redis_pool: Arc<RedisPool>,
}

impl AcmeClient {
    /// Create a new ACME client with the given providers.
    pub fn new(
        providers: Vec<AcmeProvider>,
        email: Option<String>,
        challenge_store: Arc<ChallengeStore>,
        redis_pool: Arc<RedisPool>,
    ) -> Self {
        Self {
            providers,
            email,
            challenge_store,
            redis_pool,
        }
    }

    /// Build providers from node config.
    pub fn from_config(
        provider_names: &[String],
        staging: bool,
        email: Option<String>,
        eab: &cdn_config::EabCredentials,
        challenge_store: Arc<ChallengeStore>,
        redis_pool: Arc<RedisPool>,
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

        Self::new(providers, email, challenge_store, redis_pool)
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
    pub async fn issue(&self, domains: &[String]) -> Result<IssuedCert, AcmeError> {
        if domains.is_empty() {
            return Err(AcmeError::OrderError("no domains provided".to_string()));
        }

        for provider in &self.providers {
            // Skip providers that require EAB but don't have credentials
            if provider.requires_eab()
                && (provider.eab_kid.is_none() || provider.eab_hmac_key.is_none())
            {
                log::info!(
                    "[ACME] skipping {} (missing EAB credentials)",
                    provider.name
                );
                ACME_ISSUANCE_TOTAL
                    .with_label_values(&[provider.name.as_str(), "skipped"])
                    .inc();
                continue;
            }

            log::info!(
                "[ACME] attempting issuance: provider={} domains={:?}",
                provider.name,
                domains
            );

            let start = Instant::now();
            match self.issue_with_provider(provider, domains).await {
                Ok(cert) => {
                    let elapsed = start.elapsed();
                    ACME_ISSUANCE_TOTAL
                        .with_label_values(&[provider.name.as_str(), "success"])
                        .inc();
                    ACME_ISSUANCE_DURATION
                        .with_label_values(&[provider.name.as_str()])
                        .observe(elapsed.as_secs_f64());
                    log::info!(
                        "[ACME] issuance succeeded: provider={} domains={:?} elapsed={:.1}s",
                        provider.name,
                        domains,
                        elapsed.as_secs_f64()
                    );
                    return Ok(cert);
                }
                Err(e) => {
                    let elapsed = start.elapsed();
                    ACME_ISSUANCE_TOTAL
                        .with_label_values(&[provider.name.as_str(), "failure"])
                        .inc();
                    ACME_ISSUANCE_DURATION
                        .with_label_values(&[provider.name.as_str()])
                        .observe(elapsed.as_secs_f64());
                    log::error!(
                        "[ACME] issuance failed: provider={} error={} elapsed={:.1}s",
                        provider.name,
                        e,
                        elapsed.as_secs_f64()
                    );
                }
            }
        }

        Err(AcmeError::AllProvidersFailed)
    }

    /// Issue a certificate using a specific provider.
    async fn issue_with_provider(
        &self,
        provider: &AcmeProvider,
        domains: &[String],
    ) -> Result<IssuedCert, AcmeError> {
        // 1. Get or create ACME account
        let account = self.get_or_create_account(provider).await?;

        // 2. Create order
        let identifiers: Vec<Identifier> = domains
            .iter()
            .map(|d| Identifier::Dns(d.clone()))
            .collect();

        let mut order = account
            .new_order(&NewOrder {
                identifiers: &identifiers,
            })
            .await
            .map_err(|e| AcmeError::OrderError(format!("new_order: {}", e)))?;

        // 3. Process authorizations — set up HTTP-01 challenges
        let authorizations = order
            .authorizations()
            .await
            .map_err(|e| AcmeError::ChallengeError(format!("authorizations: {}", e)))?;

        let mut challenge_tokens: Vec<(String, String)> = Vec::new();

        for authz in &authorizations {
            let Identifier::Dns(ref domain) = authz.identifier;

            if authz.status == AuthorizationStatus::Valid {
                log::info!("[ACME] authorization already valid for {}", domain);
                continue;
            }

            let challenge = authz
                .challenges
                .iter()
                .find(|c| c.r#type == ChallengeType::Http01)
                .ok_or_else(|| {
                    AcmeError::ChallengeError(format!("no HTTP-01 challenge for {}", domain))
                })?;

            let key_auth = order.key_authorization(challenge);
            self.challenge_store
                .set_challenge(domain, &challenge.token, key_auth.as_str());
            challenge_tokens.push((domain.clone(), challenge.token.clone()));

            order
                .set_challenge_ready(&challenge.url)
                .await
                .map_err(|e| {
                    self.cleanup_challenges(&challenge_tokens);
                    AcmeError::ChallengeError(format!(
                        "set_challenge_ready for {}: {}",
                        domain, e
                    ))
                })?;
        }

        // 4. Poll order until ready or invalid (max 300s)
        let poll_start = Instant::now();
        let max_poll = Duration::from_secs(300);
        let mut delay = Duration::from_secs(2);

        loop {
            if poll_start.elapsed() > max_poll {
                self.cleanup_challenges(&challenge_tokens);
                return Err(AcmeError::OrderError(
                    "order validation timed out after 300s".to_string(),
                ));
            }

            tokio::time::sleep(delay).await;

            let state = order.refresh().await.map_err(|e| {
                self.cleanup_challenges(&challenge_tokens);
                AcmeError::OrderError(format!("refresh: {}", e))
            })?;

            match state.status {
                OrderStatus::Ready | OrderStatus::Valid => {
                    log::info!("[ACME] order ready, proceeding to finalize");
                    break;
                }
                OrderStatus::Invalid => {
                    self.cleanup_challenges(&challenge_tokens);
                    let detail = state
                        .error
                        .as_ref()
                        .map(|e| format!("{:?}", e))
                        .unwrap_or_else(|| "unknown".to_string());
                    return Err(AcmeError::OrderError(format!(
                        "order invalid: {}",
                        detail
                    )));
                }
                _ => {
                    // Pending or Processing — exponential backoff up to 10s
                    delay = (delay * 2).min(Duration::from_secs(10));
                }
            }
        }

        // 5. Clean up challenge tokens (no longer needed after validation)
        self.cleanup_challenges(&challenge_tokens);

        // 6. Generate CSR using rcgen
        let private_key = rcgen::KeyPair::generate()
            .map_err(|e| AcmeError::OrderError(format!("key generation: {}", e)))?;
        let mut cert_params = rcgen::CertificateParams::new(domains.to_vec())
            .map_err(|e| AcmeError::OrderError(format!("cert params: {}", e)))?;
        cert_params.distinguished_name = rcgen::DistinguishedName::new();
        let csr = cert_params
            .serialize_request(&private_key)
            .map_err(|e| AcmeError::OrderError(format!("CSR generation: {}", e)))?;

        // 7. Finalize order with CSR
        order
            .finalize(csr.der())
            .await
            .map_err(|e| AcmeError::OrderError(format!("finalize: {}", e)))?;

        // 8. Download certificate (poll until available, reuse same timeout)
        let cert_pem = loop {
            if poll_start.elapsed() > max_poll {
                return Err(AcmeError::OrderError(
                    "certificate download timed out".to_string(),
                ));
            }

            match order
                .certificate()
                .await
                .map_err(|e| AcmeError::OrderError(format!("certificate: {}", e)))?
            {
                Some(pem) => break pem,
                None => tokio::time::sleep(Duration::from_secs(1)).await,
            }
        };

        let key_pem = private_key.serialize_pem();

        Ok(IssuedCert {
            cert_pem,
            key_pem,
            chain_pem: None, // instant-acme returns full chain in cert_pem
            provider: provider.name.clone(),
        })
    }

    /// Get or create an ACME account for the given provider.
    ///
    /// Account credentials are persisted in Redis for reuse across issuances
    /// and cluster nodes.
    async fn get_or_create_account(
        &self,
        provider: &AcmeProvider,
    ) -> Result<Account, AcmeError> {
        let redis_key = Self::account_redis_key(&provider.name, self.email.as_deref());

        // Try to restore from Redis
        if let Ok(Some(json)) = self.redis_pool.get(&redis_key).await {
            match serde_json::from_str::<AccountCredentials>(&json) {
                Ok(creds) => match Account::from_credentials(creds).await {
                    Ok(account) => {
                        log::info!(
                            "[ACME] restored account for provider={}",
                            provider.name
                        );
                        return Ok(account);
                    }
                    Err(e) => {
                        log::warn!(
                            "[ACME] failed to restore account for {}: {}, creating new",
                            provider.name,
                            e
                        );
                    }
                },
                Err(e) => {
                    log::warn!(
                        "[ACME] failed to deserialize credentials for {}: {}",
                        provider.name,
                        e
                    );
                }
            }
        }

        // Create new account
        let contact: Vec<String> = self
            .email
            .as_ref()
            .map(|e| vec![format!("mailto:{}", e)])
            .unwrap_or_default();
        let contact_refs: Vec<&str> = contact.iter().map(|s| s.as_str()).collect();

        let eab = self.build_eab(provider)?;

        let (account, credentials) = Account::create(
            &NewAccount {
                contact: &contact_refs,
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            &provider.directory_url,
            eab.as_ref(),
        )
        .await
        .map_err(|e| AcmeError::AccountError(format!("{}", e)))?;

        // Persist credentials to Redis (TTL: 365 days)
        match serde_json::to_string(&credentials) {
            Ok(json) => {
                if let Err(e) = self
                    .redis_pool
                    .setex(&redis_key, 365 * 86400, &json)
                    .await
                {
                    log::warn!("[ACME] failed to persist account to Redis: {}", e);
                }
            }
            Err(e) => {
                log::warn!("[ACME] failed to serialize credentials: {}", e);
            }
        }

        log::info!(
            "[ACME] created new account for provider={}",
            provider.name
        );
        Ok(account)
    }

    /// Build ExternalAccountKey for providers that require EAB.
    fn build_eab(
        &self,
        provider: &AcmeProvider,
    ) -> Result<Option<ExternalAccountKey>, AcmeError> {
        match (&provider.eab_kid, &provider.eab_hmac_key) {
            (Some(kid), Some(hmac_b64)) => {
                use base64::prelude::{Engine, BASE64_STANDARD, BASE64_URL_SAFE_NO_PAD};
                let hmac_bytes = BASE64_URL_SAFE_NO_PAD
                    .decode(hmac_b64)
                    .or_else(|_| BASE64_STANDARD.decode(hmac_b64))
                    .map_err(|e| {
                        AcmeError::AccountError(format!("EAB HMAC decode: {}", e))
                    })?;
                Ok(Some(ExternalAccountKey::new(kid.clone(), &hmac_bytes)))
            }
            _ => Ok(None),
        }
    }

    /// Redis key for persisting ACME account credentials.
    fn account_redis_key(provider_name: &str, email: Option<&str>) -> String {
        let email_tag = match email {
            Some(e) => {
                use base64::prelude::{Engine, BASE64_URL_SAFE_NO_PAD};
                let digest = ring::digest::digest(&ring::digest::SHA256, e.as_bytes());
                BASE64_URL_SAFE_NO_PAD.encode(&digest.as_ref()[..12])
            }
            None => "noemail".to_string(),
        };
        format!("nozdormu:acme:account:{}:{}", provider_name, email_tag)
    }

    /// Remove challenge tokens from the ChallengeStore.
    fn cleanup_challenges(&self, tokens: &[(String, String)]) {
        for (domain, token) in tokens {
            self.challenge_store.remove_challenge(domain, token);
        }
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

    #[test]
    fn test_account_redis_key() {
        let key = AcmeClient::account_redis_key("letsencrypt", Some("admin@example.com"));
        assert!(key.starts_with("nozdormu:acme:account:letsencrypt:"));
        assert!(!key.ends_with("noemail"));

        let key_no_email = AcmeClient::account_redis_key("letsencrypt", None);
        assert!(key_no_email.ends_with("noemail"));
    }

    #[test]
    fn test_account_redis_key_deterministic() {
        let k1 = AcmeClient::account_redis_key("zerossl", Some("test@test.com"));
        let k2 = AcmeClient::account_redis_key("zerossl", Some("test@test.com"));
        assert_eq!(k1, k2);

        let k3 = AcmeClient::account_redis_key("zerossl", Some("other@test.com"));
        assert_ne!(k1, k3);
    }
}
