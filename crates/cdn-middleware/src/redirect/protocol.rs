use cdn_common::ProtocolConfig;

/// Result of a protocol redirect check.
pub struct ProtocolRedirectResult {
    pub target_url: String,
    pub status_code: u16,
}

/// Check if the request should be redirected for protocol enforcement (HTTP→HTTPS or HTTPS→HTTP).
///
/// Rules:
/// - ACME challenge paths are always excluded (avoid cert issuance deadlock)
/// - `https_exclude_paths` are excluded
/// - force_https + force_http both set → HTTPS wins
/// - Non-standard ports are preserved in the redirect URL
pub fn check_protocol_redirect(
    scheme: &str,
    host: &str,
    uri: &str,
    config: &ProtocolConfig,
) -> Option<ProtocolRedirectResult> {
    let path = uri.split('?').next().unwrap_or(uri);

    // ACME challenge paths are always excluded
    if path.starts_with("/.well-known/acme-challenge/") {
        return None;
    }

    // Check exclude paths
    for exclude in &config.https_exclude_paths {
        if path.starts_with(exclude.as_str()) {
            return None;
        }
    }

    // Determine desired scheme
    let target_scheme = if config.force_https {
        "https"
    } else if config.force_http {
        "http"
    } else {
        return None; // No protocol enforcement
    };

    // Already on the correct scheme
    if scheme == target_scheme {
        return None;
    }

    // Build redirect URL
    // Strip port from host, then add the target port if non-standard
    let host_no_port = host.split(':').next().unwrap_or(host);
    let target_url = if target_scheme == "https" {
        if let Some(port) = config.https_port {
            if port != 443 {
                format!("{}://{}:{}{}", target_scheme, host_no_port, port, uri)
            } else {
                format!("{}://{}{}", target_scheme, host_no_port, uri)
            }
        } else {
            format!("{}://{}{}", target_scheme, host_no_port, uri)
        }
    } else {
        format!("{}://{}{}", target_scheme, host_no_port, uri)
    };

    Some(ProtocolRedirectResult {
        target_url,
        status_code: config.redirect_code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_https() -> ProtocolConfig {
        ProtocolConfig {
            force_https: true,
            redirect_code: 301,
            ..Default::default()
        }
    }

    #[test]
    fn test_no_enforcement() {
        let cfg = ProtocolConfig::default();
        assert!(check_protocol_redirect("http", "example.com", "/path", &cfg).is_none());
    }

    #[test]
    fn test_force_https_from_http() {
        let cfg = config_https();
        let result = check_protocol_redirect("http", "example.com", "/path?q=1", &cfg).unwrap();
        assert_eq!(result.target_url, "https://example.com/path?q=1");
        assert_eq!(result.status_code, 301);
    }

    #[test]
    fn test_already_https() {
        let cfg = config_https();
        assert!(check_protocol_redirect("https", "example.com", "/path", &cfg).is_none());
    }

    #[test]
    fn test_acme_excluded() {
        let cfg = config_https();
        assert!(check_protocol_redirect(
            "http",
            "example.com",
            "/.well-known/acme-challenge/token123",
            &cfg
        )
        .is_none());
    }

    #[test]
    fn test_exclude_paths() {
        let cfg = ProtocolConfig {
            force_https: true,
            redirect_code: 301,
            https_exclude_paths: vec!["/api/webhook".to_string()],
            ..Default::default()
        };
        assert!(
            check_protocol_redirect("http", "example.com", "/api/webhook/callback", &cfg)
                .is_none()
        );
        // Non-excluded path should redirect
        assert!(
            check_protocol_redirect("http", "example.com", "/other", &cfg).is_some()
        );
    }

    #[test]
    fn test_force_http() {
        let cfg = ProtocolConfig {
            force_http: true,
            redirect_code: 302,
            ..Default::default()
        };
        let result = check_protocol_redirect("https", "example.com", "/path", &cfg).unwrap();
        assert_eq!(result.target_url, "http://example.com/path");
        assert_eq!(result.status_code, 302);
    }

    #[test]
    fn test_both_force_https_wins() {
        let cfg = ProtocolConfig {
            force_https: true,
            force_http: true,
            redirect_code: 301,
            ..Default::default()
        };
        let result = check_protocol_redirect("http", "example.com", "/", &cfg).unwrap();
        assert_eq!(result.target_url, "https://example.com/");
    }

    #[test]
    fn test_non_standard_port() {
        let cfg = ProtocolConfig {
            force_https: true,
            redirect_code: 301,
            https_port: Some(8443),
            ..Default::default()
        };
        let result = check_protocol_redirect("http", "example.com", "/path", &cfg).unwrap();
        assert_eq!(result.target_url, "https://example.com:8443/path");
    }

    #[test]
    fn test_standard_port_not_shown() {
        let cfg = ProtocolConfig {
            force_https: true,
            redirect_code: 301,
            https_port: Some(443),
            ..Default::default()
        };
        let result = check_protocol_redirect("http", "example.com", "/path", &cfg).unwrap();
        assert_eq!(result.target_url, "https://example.com/path");
    }

    #[test]
    fn test_host_with_port_stripped() {
        let cfg = config_https();
        let result =
            check_protocol_redirect("http", "example.com:8080", "/path", &cfg).unwrap();
        assert_eq!(result.target_url, "https://example.com/path");
    }
}
