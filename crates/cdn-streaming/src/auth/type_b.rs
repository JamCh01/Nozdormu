//! Type B URL signing: `?auth_key={timestamp}-{rand}-{uid}-{md5hash}`
//!
//! HMAC computation: `HMAC-SHA256(key, "{path}-{timestamp}-{rand}-{uid}")` → hex, first 32 chars.

use super::{constant_time_eq, AuthError};
use cdn_common::StreamingAuthConfig;
use ring::hmac;
use std::time::{SystemTime, UNIX_EPOCH};

/// Compute the HMAC-SHA256 hex signature for Type B.
fn compute_hash(key: &hmac::Key, path: &str, timestamp: u64, rand: &str, uid: &str) -> String {
    let payload = format!("{}-{}-{}-{}", path, timestamp, rand, uid);
    let tag = hmac::sign(key, payload.as_bytes());
    cdn_common::hex_encode(tag.as_ref())
}

/// Extract the `auth_key` parameter value from a query string.
fn extract_auth_key(query: &str) -> Option<&str> {
    for param in query.split('&') {
        if let Some(value) = param.strip_prefix("auth_key=") {
            return Some(value);
        }
    }
    None
}

/// Validate a Type B signed URL.
///
/// URL format: `/path?auth_key={timestamp}-{rand}-{uid}-{hash}&other_params...`
/// Returns the original path (unchanged) on success.
pub fn validate(
    config: &StreamingAuthConfig,
    request_path: &str,
    query_string: Option<&str>,
) -> Result<String, AuthError> {
    let query = query_string.ok_or(AuthError::Missing)?;
    let auth_key_value = extract_auth_key(query).ok_or(AuthError::Missing)?;

    // Split auth_key value: {timestamp}-{rand}-{uid}-{hash}
    let parts: Vec<&str> = auth_key_value.splitn(4, '-').collect();
    if parts.len() != 4 {
        return Err(AuthError::MalformedUrl);
    }

    let (ts_str, rand, uid, hash) = (parts[0], parts[1], parts[2], parts[3]);

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

    // Compute expected hash
    let key = hmac::Key::new(hmac::HMAC_SHA256, config.auth_key.as_bytes());
    let expected = compute_hash(&key, request_path, timestamp, rand, uid);
    let expected_short = &expected[..32.min(expected.len())];

    // Constant-time comparison
    if hash.len() != expected_short.len() {
        return Err(AuthError::InvalidSignature);
    }
    if !constant_time_eq(hash.as_bytes(), expected_short.as_bytes()) {
        return Err(AuthError::InvalidSignature);
    }

    Ok(request_path.to_string())
}

/// Generate a Type B signed query parameter.
///
/// Returns the full path with auth_key query parameter appended.
pub fn sign(
    config: &StreamingAuthConfig,
    path: &str,
    timestamp: u64,
    rand: Option<&str>,
    uid: Option<&str>,
) -> String {
    let rand = rand.unwrap_or("0");
    let uid = uid.unwrap_or("0");
    let key = hmac::Key::new(hmac::HMAC_SHA256, config.auth_key.as_bytes());
    let hash = compute_hash(&key, path, timestamp, rand, uid);
    let hash_short = &hash[..32.min(hash.len())];
    format!(
        "{}?auth_key={}-{}-{}-{}",
        path, timestamp, rand, uid, hash_short
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cdn_common::AuthType;

    fn make_config(secret: &str, expire: u64) -> StreamingAuthConfig {
        StreamingAuthConfig {
            enabled: true,
            auth_type: AuthType::B,
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
        let signed = sign(
            &config,
            "/video/test.mp4",
            ts,
            Some("abc123"),
            Some("user1"),
        );
        // Extract path and query
        let (path, query) = signed.split_once('?').unwrap();
        let result = validate(&config, path, Some(query));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "/video/test.mp4");
    }

    #[test]
    fn test_default_rand_uid() {
        let config = make_config("secret", 1800);
        let ts = current_timestamp();
        let signed = sign(&config, "/test.mp4", ts, None, None);
        let (path, query) = signed.split_once('?').unwrap();
        let result = validate(&config, path, Some(query));
        assert!(result.is_ok());
    }

    #[test]
    fn test_expired_token() {
        let config = make_config("secret", 60);
        let ts = current_timestamp() - 120;
        let signed = sign(&config, "/test.mp4", ts, None, None);
        let (path, query) = signed.split_once('?').unwrap();
        let result = validate(&config, path, Some(query));
        assert!(matches!(result, Err(AuthError::Expired)));
    }

    #[test]
    fn test_wrong_key() {
        let config = make_config("secret", 1800);
        let ts = current_timestamp();
        let signed = sign(&config, "/test.mp4", ts, None, None);
        let (path, query) = signed.split_once('?').unwrap();

        let wrong_config = make_config("wrong", 1800);
        let result = validate(&wrong_config, path, Some(query));
        assert!(matches!(result, Err(AuthError::InvalidSignature)));
    }

    #[test]
    fn test_missing_query() {
        let config = make_config("secret", 1800);
        let result = validate(&config, "/test.mp4", None);
        assert!(matches!(result, Err(AuthError::Missing)));
    }

    #[test]
    fn test_missing_auth_key_param() {
        let config = make_config("secret", 1800);
        let result = validate(&config, "/test.mp4", Some("foo=bar"));
        assert!(matches!(result, Err(AuthError::Missing)));
    }

    #[test]
    fn test_malformed_auth_key() {
        let config = make_config("secret", 1800);
        // Only 2 parts instead of 4
        let result = validate(&config, "/test.mp4", Some("auth_key=123-abc"));
        assert!(matches!(result, Err(AuthError::MalformedUrl)));
    }

    #[test]
    fn test_with_other_query_params() {
        let config = make_config("secret", 1800);
        let ts = current_timestamp();
        let signed = sign(&config, "/test.mp4", ts, Some("r1"), Some("u1"));
        // Add extra query params
        let (path, query) = signed.split_once('?').unwrap();
        let query_with_extra = format!("foo=bar&{}&baz=qux", query);
        let result = validate(&config, path, Some(&query_with_extra));
        assert!(result.is_ok());
    }

    #[test]
    fn test_tampered_hash() {
        let config = make_config("secret", 1800);
        let ts = current_timestamp();
        let signed = sign(&config, "/test.mp4", ts, None, None);
        let (path, query) = signed.split_once('?').unwrap();
        // Replace last char of hash
        let tampered = format!("{}x", &query[..query.len() - 1]);
        let result = validate(&config, path, Some(&tampered));
        assert!(result.is_err());
    }
}
