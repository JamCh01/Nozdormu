use cdn_common::SiteConfig;

#[derive(Debug)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// Validate a site configuration. Returns a list of errors (empty = valid).
pub fn validate_site_config(config: &SiteConfig) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    // site_id required
    if config.site_id.is_empty() {
        errors.push(ValidationError {
            path: "site_id".to_string(),
            message: "required field missing".to_string(),
        });
    }

    // domains: at least 1
    if config.domains.is_empty() {
        errors.push(ValidationError {
            path: "domains".to_string(),
            message: "at least one domain is required".to_string(),
        });
    }
    for (i, domain) in config.domains.iter().enumerate() {
        if let Err(msg) = validate_domain(domain) {
            errors.push(ValidationError {
                path: format!("domains[{}]", i),
                message: msg,
            });
        }
    }

    // origins: at least 1
    if config.origins.is_empty() {
        errors.push(ValidationError {
            path: "origins".to_string(),
            message: "at least one origin is required".to_string(),
        });
    }
    for (i, origin) in config.origins.iter().enumerate() {
        if origin.id.is_empty() {
            errors.push(ValidationError {
                path: format!("origins[{}].id", i),
                message: "required field missing".to_string(),
            });
        }
        if origin.host.is_empty() {
            errors.push(ValidationError {
                path: format!("origins[{}].host", i),
                message: "required field missing".to_string(),
            });
        }
        if origin.port == 0 {
            errors.push(ValidationError {
                path: format!("origins[{}].port", i),
                message: "must be between 1 and 65535".to_string(),
            });
        }
        if origin.weight == 0 || origin.weight > 100 {
            errors.push(ValidationError {
                path: format!("origins[{}].weight", i),
                message: "must be between 1 and 100".to_string(),
            });
        }
    }

    // load_balancer.retries
    if config.load_balancer.retries > 10 {
        errors.push(ValidationError {
            path: "load_balancer.retries".to_string(),
            message: "must be between 0 and 10".to_string(),
        });
    }

    // protocol.force_https.redirect_code
    if config.protocol.force_https.enable
        && !matches!(config.protocol.force_https.redirect_code, 301 | 302 | 303 | 307 | 308)
    {
        errors.push(ValidationError {
            path: "protocol.force_https.redirect_code".to_string(),
            message: "must be one of: 301, 302, 303, 307, 308".to_string(),
        });
    }

    errors
}

/// Validate a domain string.
/// Supports wildcards: *.example.com
/// Length 1-253, characters [a-zA-Z0-9-.*]
pub fn validate_domain(domain: &str) -> Result<(), String> {
    if domain.is_empty() || domain.len() > 253 {
        return Err("domain length must be between 1 and 253".to_string());
    }

    // Check for valid characters
    let valid_chars = domain
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '*');
    if !valid_chars {
        return Err(format!("invalid characters in domain: {}", domain));
    }

    // Wildcard must be at the start: *.
    if domain.contains('*') && !domain.starts_with("*.") {
        return Err("wildcard must be at the start: *.domain".to_string());
    }

    // No multiple wildcards (e.g., *.*.example.com)
    if domain.starts_with("*.") {
        let remainder = &domain[2..];
        if remainder.is_empty() {
            return Err("wildcard domain must have a suffix after *.".to_string());
        }
        if remainder.contains('*') {
            return Err("multiple wildcards are not allowed".to_string());
        }
    }

    // No consecutive dots
    if domain.contains("..") {
        return Err("domain must not contain consecutive dots".to_string());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_domains() {
        assert!(validate_domain("example.com").is_ok());
        assert!(validate_domain("www.example.com").is_ok());
        assert!(validate_domain("*.example.com").is_ok());
        assert!(validate_domain("a-b.example.com").is_ok());
        assert!(validate_domain("a").is_ok());
    }

    #[test]
    fn test_invalid_domains() {
        assert!(validate_domain("").is_err());
        assert!(validate_domain("exam ple.com").is_err());
        assert!(validate_domain("example..com").is_err());
        assert!(validate_domain("foo.*.com").is_err());
        assert!(validate_domain(&"a".repeat(254)).is_err());
        // Multiple wildcards
        assert!(validate_domain("*.*.example.com").is_err());
        // Bare wildcard with no suffix
        assert!(validate_domain("*.").is_err());
    }
}
