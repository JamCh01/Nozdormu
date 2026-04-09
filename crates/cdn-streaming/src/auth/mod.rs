pub mod type_a;
pub mod type_b;
pub mod type_c;

use cdn_common::{AuthType, StreamingAuthConfig};

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("missing auth token")]
    Missing,
    #[error("auth token expired")]
    Expired,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("malformed URL")]
    MalformedUrl,
}

/// Constant-time byte slice comparison to prevent timing attacks.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Validate a request URL against the site's auth config.
///
/// Returns `Ok(cleaned_path)` on success, where `cleaned_path` is the original
/// resource path with auth tokens stripped.
pub fn validate_url(
    config: &StreamingAuthConfig,
    request_path: &str,
    query_string: Option<&str>,
) -> Result<String, AuthError> {
    match config.auth_type {
        AuthType::A => type_a::validate(config, request_path),
        AuthType::B => type_b::validate(config, request_path, query_string),
        AuthType::C => type_c::validate(config, request_path),
    }
}

/// Generate a signed URL (utility for tests and admin API).
pub fn sign_url(
    config: &StreamingAuthConfig,
    path: &str,
    timestamp: u64,
    rand: Option<&str>,
    uid: Option<&str>,
) -> String {
    match config.auth_type {
        AuthType::A => type_a::sign(config, path, timestamp),
        AuthType::B => type_b::sign(config, path, timestamp, rand, uid),
        AuthType::C => type_c::sign(config, path, timestamp),
    }
}
