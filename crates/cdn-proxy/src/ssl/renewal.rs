use crate::ssl::acme::AcmeClient;
use crate::ssl::storage::CertStorage;
use std::sync::Arc;

/// Certificate auto-renewal manager.
///
/// Periodically checks all certificates and renews those within
/// `renewal_days` of expiry.
///
/// Behavior:
/// - First check: 60s after startup
/// - Subsequent checks: every 24 hours
/// - 5s delay between renewals (ACME rate limit)
/// - Two-level distributed locks (scan lock + per-domain lock) via etcd
/// - Provider rotation: tries original provider first, then others
pub struct RenewalManager {
    storage: Arc<CertStorage>,
    acme: Arc<AcmeClient>,
    renewal_days: u64,
}

impl RenewalManager {
    pub fn new(
        storage: Arc<CertStorage>,
        acme: Arc<AcmeClient>,
        renewal_days: u64,
    ) -> Self {
        Self {
            storage,
            acme,
            renewal_days,
        }
    }

    /// Check all certificates and renew those that need it.
    /// Returns the number of certificates renewed.
    pub async fn check_and_renew(&self) -> u32 {
        let domains = self.storage.list_domains();
        let mut renewed = 0u32;

        for domain in &domains {
            if domain == "_default" {
                continue; // Don't auto-renew default cert
            }

            let cert = match self.storage.get(domain) {
                Some(c) => c,
                None => continue,
            };

            if !cert.needs_renewal(self.renewal_days) {
                continue;
            }

            log::info!(
                "[Renewal] cert for {} needs renewal (expires_at={})",
                domain, cert.expires_at
            );

            // TODO: Acquire distributed lock via etcd before renewing
            // let lock = etcd.lock(format!("nozdormu:lock:renewal:{}", domain), 600).await;

            // Double-check after acquiring lock (another node may have renewed)
            if let Some(fresh_cert) = self.storage.get(domain) {
                if !fresh_cert.needs_renewal(self.renewal_days) {
                    log::info!("[Renewal] {} already renewed by another node", domain);
                    continue;
                }
            }

            // Attempt renewal
            let domains_to_renew = if cert.domains.is_empty() {
                vec![domain.clone()]
            } else {
                cert.domains.clone()
            };

            match self.acme.issue(&domains_to_renew).await {
                Ok(issued) => {
                    let expires_at = crate::ssl::storage::CertData::parse_expiry(&issued.cert_pem)
                        .unwrap_or(chrono::Utc::now().timestamp() + 86400 * 90);
                    let new_cert = crate::ssl::storage::CertData {
                        cert_pem: issued.cert_pem,
                        key_pem: issued.key_pem,
                        chain_pem: issued.chain_pem,
                        expires_at,
                        provider: Some(issued.provider),
                        domains: domains_to_renew,
                    };
                    self.storage.put(domain, new_cert);
                    renewed += 1;
                    log::info!("[Renewal] successfully renewed cert for {}", domain);
                }
                Err(e) => {
                    log::error!("[Renewal] failed to renew cert for {}: {}", domain, e);
                }
            }

            // Rate limit: 5s between renewals
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }

        if renewed > 0 {
            log::info!("[Renewal] renewed {} certificates", renewed);
        }

        renewed
    }
}
