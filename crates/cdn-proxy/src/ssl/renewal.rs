use crate::logging::metrics::ACME_RENEWAL_TOTAL;
use crate::ssl::acme::AcmeClient;
use crate::ssl::manager::CertManager;
use crate::ssl::storage::CertStorage;
use crate::utils::lock::{lock_names, DistributedLock};
use crate::utils::redis_pool::RedisPool;
use std::sync::Arc;
use std::time::Duration;

/// Certificate auto-renewal manager.
///
/// Periodically checks all certificates and renews those within
/// `renewal_days` of expiry.
///
/// Behavior:
/// - First check: 60s after startup
/// - Subsequent checks: every 24 hours
/// - 5s delay between renewals (ACME rate limit)
/// - Two-level distributed locks (scan lock + per-domain lock) via Redis
/// - Provider rotation: tries original provider first, then others
pub struct RenewalManager {
    storage: Arc<CertStorage>,
    cert_manager: Arc<CertManager>,
    acme: Arc<AcmeClient>,
    redis_pool: Arc<RedisPool>,
    node_id: String,
    renewal_days: u64,
}

impl RenewalManager {
    pub fn new(
        storage: Arc<CertStorage>,
        cert_manager: Arc<CertManager>,
        acme: Arc<AcmeClient>,
        redis_pool: Arc<RedisPool>,
        node_id: String,
        renewal_days: u64,
    ) -> Self {
        Self {
            storage,
            cert_manager,
            acme,
            redis_pool,
            node_id,
            renewal_days,
        }
    }

    /// Check all certificates and renew those that need it.
    /// Returns the number of certificates renewed.
    pub async fn check_and_renew(&self) -> u32 {
        // Acquire scan lock — only one node scans at a time
        let scan_lock = DistributedLock::new(
            &lock_names::renewal_scan(),
            &self.node_id,
            3600, // 1 hour TTL
        );

        if !scan_lock.acquire(&self.redis_pool).await {
            log::info!("[Renewal] another node is scanning, skipping");
            return 0;
        }

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
                domain,
                cert.expires_at
            );

            // Acquire per-domain lock
            let domain_lock = DistributedLock::new(
                &lock_names::renewal_domain(domain),
                &self.node_id,
                600, // 10 minute TTL
            );

            if !domain_lock.acquire(&self.redis_pool).await {
                log::info!("[Renewal] {} locked by another node, skipping", domain);
                ACME_RENEWAL_TOTAL
                    .with_label_values(&["skipped"])
                    .inc();
                continue;
            }

            // Double-check after acquiring lock (another node may have renewed)
            if let Some(fresh_cert) = self.storage.get(domain) {
                if !fresh_cert.needs_renewal(self.renewal_days) {
                    log::info!("[Renewal] {} already renewed by another node", domain);
                    let _ = domain_lock.release(&self.redis_pool).await;
                    ACME_RENEWAL_TOTAL
                        .with_label_values(&["skipped"])
                        .inc();
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
                    let expires_at =
                        crate::ssl::storage::CertData::parse_expiry(&issued.cert_pem)
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

                    // Invalidate CertManager cache so TLS callbacks use the new cert
                    self.cert_manager.invalidate(domain).await;

                    renewed += 1;
                    ACME_RENEWAL_TOTAL
                        .with_label_values(&["success"])
                        .inc();
                    log::info!("[Renewal] successfully renewed cert for {}", domain);
                }
                Err(e) => {
                    ACME_RENEWAL_TOTAL
                        .with_label_values(&["failure"])
                        .inc();
                    log::error!("[Renewal] failed to renew cert for {}: {}", domain, e);
                }
            }

            // Release per-domain lock
            let _ = domain_lock.release(&self.redis_pool).await;

            // Rate limit: 5s between renewals (ACME rate limits)
            tokio::time::sleep(Duration::from_secs(5)).await;
        }

        // Release scan lock
        let _ = scan_lock.release(&self.redis_pool).await;

        if renewed > 0 {
            log::info!("[Renewal] renewed {} certificates", renewed);
        }

        renewed
    }
}
