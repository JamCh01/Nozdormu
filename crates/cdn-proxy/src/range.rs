//! HTTP Range request handling (RFC 7233).
//!
//! Supports single byte-range requests for client resume (断点续传).
//! Multi-range requests are rejected (compliant: servers may decline).

/// Parsed Range header specification.
#[derive(Debug, Clone, PartialEq)]
pub enum RangeSpec {
    /// bytes=X-Y (inclusive on both ends)
    Single(u64, u64),
    /// bytes=-N (last N bytes)
    SuffixLength(u64),
    /// bytes=X- (from X to end)
    OpenEnded(u64),
}

/// Range resolution error.
#[derive(Debug, Clone, PartialEq)]
pub enum RangeError {
    NotSatisfiable,
    MultiRangeNotSupported,
}

impl std::fmt::Display for RangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RangeError::NotSatisfiable => write!(f, "Range Not Satisfiable"),
            RangeError::MultiRangeNotSupported => write!(f, "Multi-Range Not Supported"),
        }
    }
}

/// Parse a Range header value into a RangeSpec.
///
/// Returns None for invalid or multi-range requests.
/// Only supports "bytes" unit per RFC 7233.
pub fn parse_range_header(value: &str) -> Option<RangeSpec> {
    let value = value.trim();
    let rest = value.strip_prefix("bytes=")?;

    // Reject multi-range (contains comma)
    if rest.contains(',') {
        return None;
    }

    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }

    // Suffix-length: bytes=-N
    if let Some(suffix) = rest.strip_prefix('-') {
        let n: u64 = suffix.trim().parse().ok()?;
        if n == 0 {
            return None;
        }
        return Some(RangeSpec::SuffixLength(n));
    }

    // Split on '-'
    let (start_str, end_str) = rest.split_once('-')?;
    let start: u64 = start_str.trim().parse().ok()?;

    let end_str = end_str.trim();
    if end_str.is_empty() {
        // Open-ended: bytes=X-
        return Some(RangeSpec::OpenEnded(start));
    }

    let end: u64 = end_str.parse().ok()?;
    if start > end {
        return None;
    }

    Some(RangeSpec::Single(start, end))
}

/// Resolve a RangeSpec to concrete (start, end) inclusive byte positions.
///
/// Returns Err(NotSatisfiable) if the range is invalid for the given total size.
pub fn resolve_range(spec: &RangeSpec, total_size: u64) -> Result<(u64, u64), RangeError> {
    if total_size == 0 {
        return Err(RangeError::NotSatisfiable);
    }

    match spec {
        RangeSpec::Single(start, end) => {
            if *start >= total_size {
                return Err(RangeError::NotSatisfiable);
            }
            // Clamp end to total_size - 1
            let end = (*end).min(total_size - 1);
            Ok((*start, end))
        }
        RangeSpec::SuffixLength(n) => {
            if *n == 0 {
                return Err(RangeError::NotSatisfiable);
            }
            let start = total_size.saturating_sub(*n);
            Ok((start, total_size - 1))
        }
        RangeSpec::OpenEnded(start) => {
            if *start >= total_size {
                return Err(RangeError::NotSatisfiable);
            }
            Ok((*start, total_size - 1))
        }
    }
}

/// Format a Content-Range header value for a successful 206 response.
/// Returns "bytes start-end/total".
pub fn content_range_header(start: u64, end: u64, total: u64) -> String {
    format!("bytes {}-{}/{}", start, end, total)
}

/// Format a Content-Range header value for a 416 response.
/// Returns "bytes */total".
pub fn content_range_unsatisfied(total: u64) -> String {
    format!("bytes */{}", total)
}

/// Check an If-Range precondition.
///
/// If-Range can be an ETag (strong comparison) or an HTTP-date.
/// Returns true if the condition matches (serve partial), false (serve full).
pub fn check_if_range(
    if_range: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> bool {
    let if_range = if_range.trim();
    if if_range.is_empty() {
        return true;
    }

    // ETag: starts with '"' or 'W/'
    if if_range.starts_with('"') || if_range.starts_with("W/") {
        // If-Range requires strong comparison — weak ETags never match
        if if_range.starts_with("W/") {
            return false;
        }
        // Strong comparison: exact match
        if let Some(etag) = etag {
            let etag = etag.trim();
            // Server ETag must also be strong
            if etag.starts_with("W/") {
                return false;
            }
            return etag == if_range;
        }
        return false;
    }

    // HTTP-date comparison
    if let Some(lm) = last_modified {
        return lm.trim() == if_range;
    }

    false
}

/// Extract a byte slice from a body buffer.
/// start and end are inclusive byte positions.
pub fn slice_body(body: &[u8], start: u64, end: u64) -> Vec<u8> {
    let start = start as usize;
    let end = (end as usize).min(body.len().saturating_sub(1));
    if start > end || start >= body.len() {
        return Vec::new();
    }
    body[start..=end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_range_header ──

    #[test]
    fn test_parse_single_range() {
        assert_eq!(
            parse_range_header("bytes=0-499"),
            Some(RangeSpec::Single(0, 499))
        );
        assert_eq!(
            parse_range_header("bytes=100-200"),
            Some(RangeSpec::Single(100, 200))
        );
    }

    #[test]
    fn test_parse_single_byte() {
        assert_eq!(
            parse_range_header("bytes=0-0"),
            Some(RangeSpec::Single(0, 0))
        );
    }

    #[test]
    fn test_parse_suffix() {
        assert_eq!(
            parse_range_header("bytes=-500"),
            Some(RangeSpec::SuffixLength(500))
        );
        assert_eq!(
            parse_range_header("bytes=-1"),
            Some(RangeSpec::SuffixLength(1))
        );
    }

    #[test]
    fn test_parse_open_ended() {
        assert_eq!(
            parse_range_header("bytes=500-"),
            Some(RangeSpec::OpenEnded(500))
        );
        assert_eq!(
            parse_range_header("bytes=0-"),
            Some(RangeSpec::OpenEnded(0))
        );
    }

    #[test]
    fn test_parse_invalid() {
        assert_eq!(parse_range_header(""), None);
        assert_eq!(parse_range_header("bytes="), None);
        assert_eq!(parse_range_header("bytes=abc"), None);
        assert_eq!(parse_range_header("items=0-10"), None);
        assert_eq!(parse_range_header("bytes=500-100"), None); // start > end
        assert_eq!(parse_range_header("bytes=-0"), None); // zero suffix
    }

    #[test]
    fn test_parse_multirange_rejected() {
        assert_eq!(parse_range_header("bytes=0-499, 500-999"), None);
    }

    #[test]
    fn test_parse_whitespace() {
        assert_eq!(
            parse_range_header("  bytes=0-499  "),
            Some(RangeSpec::Single(0, 499))
        );
    }

    // ── resolve_range ──

    #[test]
    fn test_resolve_single() {
        assert_eq!(resolve_range(&RangeSpec::Single(0, 499), 1000), Ok((0, 499)));
        // End clamped to total - 1
        assert_eq!(
            resolve_range(&RangeSpec::Single(0, 9999), 1000),
            Ok((0, 999))
        );
    }

    #[test]
    fn test_resolve_suffix() {
        assert_eq!(
            resolve_range(&RangeSpec::SuffixLength(500), 1000),
            Ok((500, 999))
        );
        // Suffix larger than file → entire file
        assert_eq!(
            resolve_range(&RangeSpec::SuffixLength(2000), 1000),
            Ok((0, 999))
        );
    }

    #[test]
    fn test_resolve_open_ended() {
        assert_eq!(
            resolve_range(&RangeSpec::OpenEnded(500), 1000),
            Ok((500, 999))
        );
    }

    #[test]
    fn test_resolve_not_satisfiable() {
        assert_eq!(
            resolve_range(&RangeSpec::Single(1000, 1500), 1000),
            Err(RangeError::NotSatisfiable)
        );
        assert_eq!(
            resolve_range(&RangeSpec::OpenEnded(1000), 1000),
            Err(RangeError::NotSatisfiable)
        );
    }

    #[test]
    fn test_resolve_zero_size_file() {
        assert_eq!(
            resolve_range(&RangeSpec::Single(0, 0), 0),
            Err(RangeError::NotSatisfiable)
        );
        assert_eq!(
            resolve_range(&RangeSpec::SuffixLength(100), 0),
            Err(RangeError::NotSatisfiable)
        );
    }

    // ── content_range_header ──

    #[test]
    fn test_content_range_header() {
        assert_eq!(content_range_header(0, 499, 1000), "bytes 0-499/1000");
        assert_eq!(content_range_header(500, 999, 1000), "bytes 500-999/1000");
    }

    #[test]
    fn test_content_range_unsatisfied() {
        assert_eq!(content_range_unsatisfied(1000), "bytes */1000");
    }

    // ── check_if_range ──

    #[test]
    fn test_if_range_etag_match() {
        assert!(check_if_range(
            "\"abc123\"",
            Some("\"abc123\""),
            None
        ));
    }

    #[test]
    fn test_if_range_etag_mismatch() {
        assert!(!check_if_range(
            "\"abc123\"",
            Some("\"xyz789\""),
            None
        ));
    }

    #[test]
    fn test_if_range_weak_etag_rejected() {
        // Weak ETags never match for If-Range (strong comparison required)
        assert!(!check_if_range(
            "W/\"abc123\"",
            Some("W/\"abc123\""),
            None
        ));
    }

    #[test]
    fn test_if_range_date_match() {
        let date = "Tue, 15 Nov 2024 08:12:31 GMT";
        assert!(check_if_range(date, None, Some(date)));
    }

    #[test]
    fn test_if_range_date_mismatch() {
        assert!(!check_if_range(
            "Tue, 15 Nov 2024 08:12:31 GMT",
            None,
            Some("Wed, 16 Nov 2024 08:12:31 GMT")
        ));
    }

    #[test]
    fn test_if_range_no_validators() {
        // No ETag or Last-Modified on server → condition fails
        assert!(!check_if_range("\"abc123\"", None, None));
    }

    #[test]
    fn test_if_range_empty() {
        // Empty If-Range → always serve partial
        assert!(check_if_range("", Some("\"abc\""), None));
    }

    // ── slice_body ──

    #[test]
    fn test_slice_body() {
        let body = b"Hello, World!";
        assert_eq!(slice_body(body, 0, 4), b"Hello");
        assert_eq!(slice_body(body, 7, 11), b"World");
        assert_eq!(slice_body(body, 0, 0), b"H");
    }

    #[test]
    fn test_slice_body_full() {
        let body = b"Hello";
        assert_eq!(slice_body(body, 0, 4), b"Hello");
    }

    #[test]
    fn test_slice_body_out_of_bounds() {
        let body = b"Hello";
        assert_eq!(slice_body(body, 10, 20), Vec::<u8>::new());
    }

    #[test]
    fn test_slice_body_end_clamped() {
        let body = b"Hello";
        // end beyond body length → clamped
        assert_eq!(slice_body(body, 0, 100), b"Hello");
    }
}
