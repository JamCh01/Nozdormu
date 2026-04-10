use md5::{Digest, Md5};

/// Generate a cache key from request components.
///
/// Format: MD5(site_id:host:path:sorted_args:vary_values)
///
/// - `sort_query_string`: sort query parameters alphabetically to improve hit rate
/// - `vary_headers`: request header values to include in the key (e.g., Accept-Language)
pub fn generate_cache_key(
    site_id: &str,
    host: &str,
    path: &str,
    query_string: Option<&str>,
    sort_query_string: bool,
    vary_headers: &[(String, String)], // (header_name, header_value)
) -> String {
    let mut hasher = Md5::new();

    // Hash: site_id:host:path:
    hasher.update(site_id.as_bytes());
    hasher.update(b":");
    hasher.update(host.as_bytes());
    hasher.update(b":");
    hasher.update(path.as_bytes());
    hasher.update(b":");

    // Hash: sorted_args (or raw query string)
    match query_string {
        Some(qs) if !qs.is_empty() => {
            if sort_query_string {
                let sorted = sort_query(qs);
                hasher.update(sorted.as_bytes());
            } else {
                hasher.update(qs.as_bytes());
            }
        }
        _ => {}
    }

    // Hash: :vary_part
    hasher.update(b":");
    if !vary_headers.is_empty() {
        for (i, (k, v)) in vary_headers.iter().enumerate() {
            if i > 0 {
                hasher.update(b"&");
            }
            // Lowercase header name for case-insensitive matching
            for &b in k.as_bytes() {
                hasher.update([b.to_ascii_lowercase()]);
            }
            hasher.update(b"=");
            hasher.update(v.as_bytes());
        }
    }

    cdn_common::hex_encode(&hasher.finalize())
}

/// Generate the OSS object path from a cache key.
/// Format: cache/{site_id}/{key[0..2]}/{key}
///
/// Validates site_id to prevent path traversal attacks.
pub fn cache_object_path(site_id: &str, key: &str) -> String {
    // Sanitize site_id: reject path separators and traversal sequences
    let safe_site_id = if site_id.contains('/')
        || site_id.contains('\\')
        || site_id.contains("..")
        || site_id.is_empty()
    {
        log::warn!("[Cache] invalid site_id rejected: {:?}", site_id);
        "_invalid_"
    } else {
        site_id
    };
    let prefix = if key.len() >= 2 { &key[..2] } else { key };
    format!("cache/{}/{}/{}", safe_site_id, prefix, key)
}

/// Sort query string parameters alphabetically.
fn sort_query(qs: &str) -> String {
    let mut params: Vec<&str> = qs.split('&').collect();
    params.sort();
    params.join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_key() {
        let key = generate_cache_key("site1", "example.com", "/page", None, false, &[]);
        assert_eq!(key.len(), 32); // MD5 hex = 32 chars
    }

    #[test]
    fn test_deterministic() {
        let k1 = generate_cache_key("s", "h", "/p", Some("a=1"), false, &[]);
        let k2 = generate_cache_key("s", "h", "/p", Some("a=1"), false, &[]);
        assert_eq!(k1, k2);
    }

    #[test]
    fn test_different_inputs_different_keys() {
        let k1 = generate_cache_key("s", "h", "/a", None, false, &[]);
        let k2 = generate_cache_key("s", "h", "/b", None, false, &[]);
        assert_ne!(k1, k2);
    }

    #[test]
    fn test_sort_query_string() {
        let k1 = generate_cache_key("s", "h", "/p", Some("b=2&a=1"), true, &[]);
        let k2 = generate_cache_key("s", "h", "/p", Some("a=1&b=2"), true, &[]);
        assert_eq!(k1, k2); // Same after sorting
    }

    #[test]
    fn test_unsorted_query_different() {
        let k1 = generate_cache_key("s", "h", "/p", Some("b=2&a=1"), false, &[]);
        let k2 = generate_cache_key("s", "h", "/p", Some("a=1&b=2"), false, &[]);
        assert_ne!(k1, k2); // Different without sorting
    }

    #[test]
    fn test_vary_headers_affect_key() {
        let k1 = generate_cache_key("s", "h", "/p", None, false, &[]);
        let k2 = generate_cache_key(
            "s",
            "h",
            "/p",
            None,
            false,
            &[("Accept-Language".to_string(), "en".to_string())],
        );
        assert_ne!(k1, k2);
    }

    #[test]
    fn test_vary_header_name_case_insensitive() {
        let k1 = generate_cache_key(
            "s",
            "h",
            "/p",
            None,
            false,
            &[("Accept-Language".to_string(), "en".to_string())],
        );
        let k2 = generate_cache_key(
            "s",
            "h",
            "/p",
            None,
            false,
            &[("accept-language".to_string(), "en".to_string())],
        );
        assert_eq!(k1, k2); // Same key regardless of header name case
    }

    #[test]
    fn test_cache_object_path() {
        let path = cache_object_path("site1", "abcdef1234567890");
        assert_eq!(path, "cache/site1/ab/abcdef1234567890");
    }

    #[test]
    fn test_empty_query_string() {
        let k1 = generate_cache_key("s", "h", "/p", None, false, &[]);
        let k2 = generate_cache_key("s", "h", "/p", Some(""), false, &[]);
        assert_eq!(k1, k2);
    }

    #[test]
    fn test_cache_object_path_rejects_traversal() {
        // Path traversal in site_id should be sanitized
        let path = cache_object_path("../../../admin", "abcdef1234567890");
        assert!(!path.contains(".."));
        assert!(path.starts_with("cache/_invalid_/"));

        // Slash in site_id
        let path = cache_object_path("site/../../etc", "abcdef1234567890");
        assert!(path.starts_with("cache/_invalid_/"));

        // Backslash
        let path = cache_object_path("site\\..\\admin", "abcdef1234567890");
        assert!(path.starts_with("cache/_invalid_/"));

        // Empty site_id
        let path = cache_object_path("", "abcdef1234567890");
        assert!(path.starts_with("cache/_invalid_/"));

        // Normal site_id still works
        let path = cache_object_path("site-123", "abcdef1234567890");
        assert_eq!(path, "cache/site-123/ab/abcdef1234567890");
    }
}
