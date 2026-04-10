use crate::ssl::manager::CertManager;
use async_trait::async_trait;
use pingora::listeners::TlsAccept;
use pingora::protocols::tls::TlsRef;
use pingora::tls::ext;
use pingora::tls::ssl::NameType;
use pingora::tls::x509::X509;
use std::any::Any;
use std::sync::Arc;

// Required for TlsRef::as_ptr()
use foreign_types_shared::ForeignTypeRef as _;

/// Data stored in SslDigest.extension after TLS handshake.
/// Accessible via `session.digest().ssl_digest.extension.get::<TlsHandshakeData>()`.
#[derive(Debug, Clone)]
pub struct TlsHandshakeData {
    /// Whether TLS 1.3 early data (0-RTT) was accepted for this connection.
    pub early_data_accepted: bool,
}

/// SSL_get_early_data_status constants and FFI.
/// Not exposed in openssl-sys 0.9.112, but available in OpenSSL 1.1.1+.
mod early_data_ffi {
    use std::os::raw::c_int;

    pub const SSL_EARLY_DATA_ACCEPTED: c_int = 2;

    extern "C" {
        pub fn SSL_get_early_data_status(s: *const pingora::tls::ssl_sys::SSL) -> c_int;
    }
}

/// TLS accept callbacks for Nozdormu CDN.
///
/// Provides dynamic certificate provisioning via `CertManager` and
/// early data status detection for TLS 1.3 0-RTT support.
pub struct CdnTlsAccept {
    cert_manager: Arc<CertManager>,
}

impl CdnTlsAccept {
    pub fn new(cert_manager: Arc<CertManager>) -> Self {
        Self { cert_manager }
    }
}

#[async_trait]
impl TlsAccept for CdnTlsAccept {
    async fn certificate_callback(&self, ssl: &mut TlsRef) {
        // Extract SNI from the ClientHello
        let sni = match ssl.servername(NameType::HOST_NAME) {
            Some(name) => name.to_string(),
            None => {
                log::warn!("[TLS] no SNI in ClientHello, using default cert");
                "_default".to_string()
            }
        };

        // Look up certificate via CertManager (cache -> storage -> wildcard -> default)
        let cert_data = match self.cert_manager.get_cert(&sni).await {
            Some(data) => data,
            None => {
                log::warn!("[TLS] no certificate found for {}", sni);
                return;
            }
        };

        // Load leaf certificate
        let fullchain = cert_data.fullchain_pem();
        let certs = match X509::stack_from_pem(fullchain.as_bytes()) {
            Ok(certs) if !certs.is_empty() => certs,
            Ok(_) => {
                log::error!("[TLS] empty certificate chain for {}", sni);
                return;
            }
            Err(e) => {
                log::error!("[TLS] X509::stack_from_pem failed for {}: {}", sni, e);
                return;
            }
        };

        // Set leaf certificate
        if let Err(e) = ext::ssl_use_certificate(ssl, &certs[0]) {
            log::error!("[TLS] ssl_use_certificate failed for {}: {}", sni, e);
            return;
        }

        // Add chain certificates (skip leaf)
        for chain_cert in &certs[1..] {
            if let Err(e) = ext::ssl_add_chain_cert(ssl, chain_cert) {
                log::error!("[TLS] ssl_add_chain_cert failed for {}: {}", sni, e);
            }
        }

        // Load private key
        match pingora::tls::pkey::PKey::private_key_from_pem(cert_data.key_pem.as_bytes()) {
            Ok(key) => {
                if let Err(e) = ext::ssl_use_private_key(ssl, &key) {
                    log::error!("[TLS] ssl_use_private_key failed for {}: {}", sni, e);
                }
            }
            Err(e) => {
                log::error!("[TLS] PKey::from_pem failed for {}: {}", sni, e);
            }
        }
    }

    async fn handshake_complete_callback(
        &self,
        ssl: &TlsRef,
    ) -> Option<Arc<dyn Any + Send + Sync>> {
        let early_data_accepted = unsafe {
            early_data_ffi::SSL_get_early_data_status(ssl.as_ptr())
                == early_data_ffi::SSL_EARLY_DATA_ACCEPTED
        };

        if early_data_accepted {
            log::debug!("[TLS] early data (0-RTT) accepted for connection");
        }

        Some(Arc::new(TlsHandshakeData {
            early_data_accepted,
        }))
    }
}
