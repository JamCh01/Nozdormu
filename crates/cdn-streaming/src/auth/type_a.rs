//! Type A URL signing: `/{timestamp}/{md5hash}/{original_path}`
//!
//! HMAC computation: `HMAC-SHA256(key, "{original_path}-{timestamp}")` → hex, first 32 chars.

use super::{constant_time_eq, AuthError};
use cdn_common::StreamingAuthConfig;
use ring::hmac;
use std::time::{SystemTime, UNIX_EPOCH};

/// Compute the HMAC-SHA256 hex signature for Type A.
fn compute_hash(key: &hmac::Key, path: &str, timestamp: u64) -> String {
    let payload = format!("{}-{}", path, timestamp);
    let tag = hmac::sign(key, payload.as_bytes());
    cdn_common::hex_encode(tag.as_ref())
}

/// Validate a Type A signed URL.
///
/// URL format: `/{timestamp}/{hash}/{original_path}`
/// Returns the cleaned original path on success.
pub fn validate(config: &StreamingAuthConfig, request_path: &str) -> Result<String, AuthError> {
    // Strip leading slash and split into segments
    let trimmed = request_path.strip_prefix('/').unwrap_or(request_path);
    let mut parts = trimmed.splitn(3, '/');

    let ts_str = parts.next().ok_or(AuthError::MalformedUrl)?;
    let hash = parts.next().ok_or(AuthError::MalformedUrl)?;
    let rest = parts.next().ok_or(AuthError::MalformedUrl)?;

    if ts_str.is_empty() || hash.is_empty() || rest.is_empty() {
        return Err(AuthError::MalformedUrl);
    }

    // Parse timestamp
    let timestamp: u64 = ts_str.parse().map_err(|_| AuthError::MalformedUrl)?;

    // Check expiry
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if now > timestamp + config.expire_time {
        return Err(AuthError::Expired);
    }

    // Reconstruct original path
    let original_path = format!("/{}", rest);

    // Compute expected hash
    let key = hmac::Key::new(hmac::HMAC_SHA256, config.auth_key.as_bytes());
    let expected = compute_hash(&key, &original_path, timestamp);

    // Constant-time comparison (first 32 chars)
    let expected_short = &expected[..32.min(expected.len())];
    if hash.len() != expected_short.len() {
        return Err(AuthError::InvalidSignature);
    }
    if !constant_time_eq(hash.as_bytes(), expected_short.as_bytes()) {
        return Err(AuthError::InvalidSignature);
    }

    Ok(original_path)
}

/// Generate a Type A signed URL path.
pub fn sign(config: &StreamingAuthConfig, path: &str, timestamp: u64) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, config.auth_key.as_bytes());
    let hash = compute_hash(&key, path, timestamp);
    let hash_short = &hash[..32.min(hash.len())];
    // Strip leading slash from path for URL construction
    let path_no_slash = path.strip_prefix('/').unwrap_or(path);
    format!("/{}/{}/{}", timestamp, hash_short, path_no_slash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cdn_common::AuthType;

    fn make_config(secret: &str, expire: u64) -> StreamingAuthConfig {
        StreamingAuthConfig {
            enabled: true,
            auth_type: AuthType::A,
            auth_key: secret.to_string(),
            expire_time: expire,
        }
    }

    fn current_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn test_sign_and_validate_roundtrip() {
        let config = make_config("my-secret-key", 1800);
        let ts = current_timestamp();
        let signed = sign(&config, "/video/test.mp4", ts);
        let result = validate(&config, &signed);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "/video/test.mp4");
    }

    #[test]
    fn test_expired_token() {
        let config = make_config("my-secret-key", 60);
        // Timestamp from 2 minutes ago
        let ts = current_timestamp() - 120;
        let signed = sign(&config, "/video/test.mp4", ts);
        let result = validate(&config, &signed);
        assert!(matches!(result, Err(AuthError::Expired)));
    }

    #[test]
    fn test_wrong_key() {
        let config = make_config("my-secret-key", 1800);
        let ts = current_timestamp();
        let signed = sign(&config, "/video/test.mp4", ts);

        let wrong_config = make_config("wrong-key", 1800);
        let result = validate(&wrong_config, &signed);
        assert!(matches!(result, Err(AuthError::InvalidSignature)));
    }

    #[test]
    fn test_tampered_hash() {
        let config = make_config("my-secret-key", 1800);
        let ts = current_timestamp();
        let signed = sign(&config, "/video/test.mp4", ts);
        // Tamper with the hash portion
        let tampered = signed.replacen('a', "b", 1);
        if tampered != signed {
            let result = validate(&config, &tampered);
            assert!(result.is_err());
        }
    }

    #[test]
    fn test_malformed_url_no_segments() {
        let config = make_config("key", 1800);
        assert!(matches!(validate(&config, "/"), Err(AuthError::MalformedUrl)));
        assert!(matches!(validate(&config, "/123"), Err(AuthError::MalformedUrl)));
        assert!(matches!(validate(&config, "/123/abc"), Err(AuthError::MalformedUrl)));
    }

    #[test]
    fn test_malformed_url_bad_timestamp() {
        let config = make_config("key", 1800);
        assert!(matches!(
            validate(&config, "/notanumber/abc123/path"),
            Err(AuthError::MalformedUrl)
        ));
    }

    #[test]
    fn test_deep_path() {
        let config = make_config("secret", 1800);
        let ts = current_timestamp();
        let signed = sign(&config, "/a/b/c/d/e.mp4", ts);
        let result = validate(&config, &signed);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "/a/b/c/d/e.mp4");
    }

    #[test]
    fn test_path_with_special_chars() {
        let config = make_config("secret", 1800);
        let ts = current_timestamp();
        let signed = sign(&config, "/video/my file (1).mp4", ts);
        let result = validate(&config, &signed);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "/video/my file (1).mp4");
    }

    #[test]
    fn test_exact_expiry_boundary() {
        let config = make_config("secret", 60);
        // Exactly at the boundary (should still be valid)
        let ts = current_timestamp() - 60;
        let signed = sign(&config, "/test.mp4", ts);
        let result = validate(&config, &signed);
        assert!(result.is_ok());
    }
}
