use cdn_common::ForceHttpsConfig;

/// Result of a protocol redirect check.
pub struct ProtocolRedirectResult {
    pub target_url: String,
    pub status_code: u16,
}

/// Check if the request should be redirected for protocol enforcement (HTTP→HTTPS).
///
/// Rules:
/// - ACME challenge paths are always excluded (avoid cert issuance deadlock)
/// - `exclude_paths` are excluded
/// - Non-standard ports are preserved in the redirect URL
pub fn check_protocol_redirect(
    scheme: &str,
    host: &str,
    uri: &str,
    config: &ForceHttpsConfig,
) -> Option<ProtocolRedirectResult> {
    if !config.enable {
        return None;
    }

    // Already on HTTPS
    if scheme == "https" {
        return None;
    }

    let path = uri.split('?').next().unwrap_or(uri);

    // ACME challenge paths are always excluded
    if path.starts_with("/.well-known/acme-challenge/") {
        return None;
    }

    // Check exclude paths
    for exclude in &config.exclude_paths {
        if path.starts_with(exclude.as_str()) {
            return None;
        }
    }

    // Build redirect URL
    // Strip port from host, then add the target port if non-standard
    let host_no_port = host.split(':').next().unwrap_or(host);
    let target_url = if let Some(port) = config.https_port {
        if port != 443 {
            format!("https://{}:{}{}", host_no_port, port, uri)
        } else {
            format!("https://{}{}", host_no_port, uri)
        }
    } else {
        format!("https://{}{}", host_no_port, uri)
    };

    Some(ProtocolRedirectResult {
        target_url,
        status_code: config.redirect_code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_https() -> ForceHttpsConfig {
        ForceHttpsConfig {
            enable: true,
            redirect_code: 301,
            ..Default::default()
        }
    }

    #[test]
    fn test_no_enforcement() {
        let cfg = ForceHttpsConfig::default();
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
        let cfg = ForceHttpsConfig {
            enable: true,
            redirect_code: 301,
            exclude_paths: vec!["/api/webhook".to_string()],
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
    fn test_non_standard_port() {
        let cfg = ForceHttpsConfig {
            enable: true,
            redirect_code: 301,
            https_port: Some(8443),
            ..Default::default()
        };
        let result = check_protocol_redirect("http", "example.com", "/path", &cfg).unwrap();
        assert_eq!(result.target_url, "https://example.com:8443/path");
    }

    #[test]
    fn test_standard_port_not_shown() {
        let cfg = ForceHttpsConfig {
            enable: true,
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

    #[test]
    fn test_custom_redirect_code() {
        let cfg = ForceHttpsConfig {
            enable: true,
            redirect_code: 302,
            ..Default::default()
        };
        let result = check_protocol_redirect("http", "example.com", "/", &cfg).unwrap();
        assert_eq!(result.status_code, 302);
    }
}
