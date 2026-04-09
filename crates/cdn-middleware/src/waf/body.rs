use cdn_common::BodyInspectionConfig;

/// Result of a body inspection check.
#[derive(Debug, Clone, PartialEq)]
pub enum BodyCheckResult {
    /// Request is allowed to proceed.
    Allow,
    /// Body too large → 413.
    TooLarge { limit: u64, actual: Option<u64> },
    /// Detected content type is explicitly blocked → 403.
    ContentTypeBlocked { detected: String, reason: String },
    /// Declared Content-Type does not match detected magic bytes → 403.
    ContentTypeMismatch { declared: String, detected: String },
}

/// Check Content-Length header against max_body_size.
/// Called in `request_filter` for early rejection before body transfer.
pub fn check_content_length(
    content_length: Option<u64>,
    method: &str,
    config: &BodyInspectionConfig,
) -> BodyCheckResult {
    if !config.enabled {
        return BodyCheckResult::Allow;
    }

    // Only check methods in the inspect list
    if !config
        .inspect_methods
        .iter()
        .any(|m| m.eq_ignore_ascii_case(method))
    {
        return BodyCheckResult::Allow;
    }

    // Size check (0 = unlimited)
    if config.max_body_size > 0 {
        if let Some(cl) = content_length {
            if cl > config.max_body_size {
                return BodyCheckResult::TooLarge {
                    limit: config.max_body_size,
                    actual: Some(cl),
                };
            }
        }
    }

    BodyCheckResult::Allow
}

/// Check magic bytes from the first chunk of request body.
///
/// Uses the `infer` crate to detect actual file type from magic bytes,
/// then validates against allowed/blocked content type lists.
pub fn check_magic_bytes(
    first_bytes: &[u8],
    declared_content_type: Option<&str>,
    config: &BodyInspectionConfig,
) -> BodyCheckResult {
    if !config.enabled {
        return BodyCheckResult::Allow;
    }

    // No content-type rules configured → allow
    if config.allowed_content_types.is_empty() && config.blocked_content_types.is_empty() {
        return BodyCheckResult::Allow;
    }

    // Detect actual MIME type from magic bytes
    let detected_mime = match infer::get(first_bytes) {
        Some(kind) => kind.mime_type().to_string(),
        None => {
            // Cannot detect type from magic bytes
            if !config.allowed_content_types.is_empty() {
                // Fail-closed: if allowlist is set and we can't detect, block
                return BodyCheckResult::ContentTypeBlocked {
                    detected: "unknown".to_string(),
                    reason: "unrecognized file type, allowed list is set".to_string(),
                };
            }
            // No allowlist → allow unknown types (blocklist can't match unknown)
            return BodyCheckResult::Allow;
        }
    };

    // Check against allowed list (if non-empty, must match at least one)
    if !config.allowed_content_types.is_empty() {
        let allowed = config
            .allowed_content_types
            .iter()
            .any(|pattern| mime_matches(pattern, &detected_mime));
        if !allowed {
            return BodyCheckResult::ContentTypeBlocked {
                detected: detected_mime,
                reason: "not in allowed content types".to_string(),
            };
        }
    }

    // Check against blocked list
    if config
        .blocked_content_types
        .iter()
        .any(|pattern| mime_matches(pattern, &detected_mime))
    {
        return BodyCheckResult::ContentTypeBlocked {
            detected: detected_mime,
            reason: "in blocked content types".to_string(),
        };
    }

    // Check declared vs detected mismatch (if declared header exists)
    if let Some(declared) = declared_content_type {
        // Extract base MIME type (strip parameters like charset)
        let declared_base = declared.split(';').next().unwrap_or(declared).trim();
        if !declared_base.is_empty() && !mime_matches(declared_base, &detected_mime) {
            // Only flag mismatch at the type level (e.g., image/* vs application/*)
            let declared_type = declared_base.split('/').next().unwrap_or("");
            let detected_type = detected_mime.split('/').next().unwrap_or("");
            if !declared_type.is_empty()
                && !detected_type.is_empty()
                && !declared_type.eq_ignore_ascii_case(detected_type)
            {
                return BodyCheckResult::ContentTypeMismatch {
                    declared: declared_base.to_string(),
                    detected: detected_mime,
                };
            }
        }
    }

    BodyCheckResult::Allow
}

/// Match a MIME pattern against a detected MIME type.
/// Supports wildcards: "image/*" matches "image/jpeg", "image/png", etc.
/// Exact match: "application/pdf" matches "application/pdf".
fn mime_matches(pattern: &str, mime: &str) -> bool {
    let pattern = pattern.trim();
    let mime = mime.trim();

    // Full wildcard
    if pattern == "*/*" || pattern == "*" {
        return true;
    }

    // Exact match (case-insensitive)
    if pattern.eq_ignore_ascii_case(mime) {
        return true;
    }

    // Wildcard match: "type/*"
    if let Some(prefix) = pattern.strip_suffix("/*") {
        if let Some(mime_type) = mime.split('/').next() {
            return prefix.eq_ignore_ascii_case(mime_type);
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(
        max_size: u64,
        allowed: Vec<&str>,
        blocked: Vec<&str>,
    ) -> BodyInspectionConfig {
        BodyInspectionConfig {
            enabled: true,
            max_body_size: max_size,
            allowed_content_types: allowed.into_iter().map(|s| s.to_string()).collect(),
            blocked_content_types: blocked.into_iter().map(|s| s.to_string()).collect(),
            inspect_methods: vec!["POST".into(), "PUT".into(), "PATCH".into()],
        }
    }

    // ── Content-Length checks ──

    #[test]
    fn test_content_length_within_limit() {
        let config = make_config(1_000_000, vec![], vec![]);
        let result = check_content_length(Some(500_000), "POST", &config);
        assert_eq!(result, BodyCheckResult::Allow);
    }

    #[test]
    fn test_content_length_exceeds_limit() {
        let config = make_config(1_000_000, vec![], vec![]);
        let result = check_content_length(Some(2_000_000), "POST", &config);
        assert!(matches!(result, BodyCheckResult::TooLarge { limit: 1_000_000, actual: Some(2_000_000) }));
    }

    #[test]
    fn test_content_length_no_header() {
        let config = make_config(1_000_000, vec![], vec![]);
        let result = check_content_length(None, "POST", &config);
        assert_eq!(result, BodyCheckResult::Allow);
    }

    #[test]
    fn test_content_length_unlimited() {
        let config = make_config(0, vec![], vec![]);
        let result = check_content_length(Some(999_999_999), "POST", &config);
        assert_eq!(result, BodyCheckResult::Allow);
    }

    #[test]
    fn test_content_length_get_skipped() {
        let config = make_config(100, vec![], vec![]);
        let result = check_content_length(Some(999_999), "GET", &config);
        assert_eq!(result, BodyCheckResult::Allow);
    }

    #[test]
    fn test_content_length_disabled() {
        let mut config = make_config(100, vec![], vec![]);
        config.enabled = false;
        let result = check_content_length(Some(999_999), "POST", &config);
        assert_eq!(result, BodyCheckResult::Allow);
    }

    #[test]
    fn test_content_length_exact_limit() {
        let config = make_config(1000, vec![], vec![]);
        let result = check_content_length(Some(1000), "POST", &config);
        assert_eq!(result, BodyCheckResult::Allow);
    }

    #[test]
    fn test_content_length_put_method() {
        let config = make_config(1000, vec![], vec![]);
        let result = check_content_length(Some(2000), "PUT", &config);
        assert!(matches!(result, BodyCheckResult::TooLarge { .. }));
    }

    // ── Magic bytes checks ──

    #[test]
    fn test_magic_bytes_jpeg() {
        // JPEG magic bytes: FF D8 FF
        let jpeg_bytes = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46];
        let config = make_config(0, vec!["image/*"], vec![]);
        let result = check_magic_bytes(&jpeg_bytes, Some("image/jpeg"), &config);
        assert_eq!(result, BodyCheckResult::Allow);
    }

    #[test]
    fn test_magic_bytes_jpeg_blocked() {
        let jpeg_bytes = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46];
        let config = make_config(0, vec![], vec!["image/*"]);
        let result = check_magic_bytes(&jpeg_bytes, Some("image/jpeg"), &config);
        assert!(matches!(result, BodyCheckResult::ContentTypeBlocked { .. }));
    }

    #[test]
    fn test_magic_bytes_png_not_in_allowed() {
        // PNG magic bytes: 89 50 4E 47 0D 0A 1A 0A
        let png_bytes = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let config = make_config(0, vec!["application/pdf"], vec![]);
        let result = check_magic_bytes(&png_bytes, Some("image/png"), &config);
        assert!(matches!(result, BodyCheckResult::ContentTypeBlocked { .. }));
    }

    #[test]
    fn test_magic_bytes_pdf_allowed() {
        // PDF magic bytes: %PDF
        let pdf_bytes = b"%PDF-1.4 some content here";
        let config = make_config(0, vec!["application/pdf"], vec![]);
        let result = check_magic_bytes(pdf_bytes, Some("application/pdf"), &config);
        assert_eq!(result, BodyCheckResult::Allow);
    }

    #[test]
    fn test_magic_bytes_unknown_with_allowlist() {
        // Random bytes that don't match any known type
        let unknown = [0x01, 0x02, 0x03, 0x04, 0x05];
        let config = make_config(0, vec!["image/*"], vec![]);
        let result = check_magic_bytes(&unknown, None, &config);
        assert!(matches!(result, BodyCheckResult::ContentTypeBlocked { detected, .. } if detected == "unknown"));
    }

    #[test]
    fn test_magic_bytes_unknown_no_rules() {
        let unknown = [0x01, 0x02, 0x03, 0x04, 0x05];
        let config = make_config(0, vec![], vec![]);
        let result = check_magic_bytes(&unknown, None, &config);
        assert_eq!(result, BodyCheckResult::Allow);
    }

    #[test]
    fn test_magic_bytes_mismatch_declared_vs_detected() {
        // JPEG bytes but declared as application/pdf
        let jpeg_bytes = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46];
        let config = make_config(0, vec!["image/*", "application/*"], vec![]);
        let result = check_magic_bytes(&jpeg_bytes, Some("application/pdf"), &config);
        assert!(matches!(result, BodyCheckResult::ContentTypeMismatch { .. }));
    }

    #[test]
    fn test_magic_bytes_mismatch_same_type_family() {
        // JPEG bytes declared as image/png — same type family (image/*), no mismatch flagged
        let jpeg_bytes = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46];
        let config = make_config(0, vec!["image/*"], vec![]);
        let result = check_magic_bytes(&jpeg_bytes, Some("image/png"), &config);
        assert_eq!(result, BodyCheckResult::Allow);
    }

    #[test]
    fn test_magic_bytes_disabled() {
        let jpeg_bytes = [0xFF, 0xD8, 0xFF, 0xE0];
        let mut config = make_config(0, vec!["application/pdf"], vec![]);
        config.enabled = false;
        let result = check_magic_bytes(&jpeg_bytes, Some("image/jpeg"), &config);
        assert_eq!(result, BodyCheckResult::Allow);
    }

    #[test]
    fn test_magic_bytes_empty_body() {
        let config = make_config(0, vec!["image/*"], vec![]);
        let result = check_magic_bytes(&[], None, &config);
        assert!(matches!(result, BodyCheckResult::ContentTypeBlocked { .. }));
    }

    // ── MIME matching ──

    #[test]
    fn test_mime_matches_exact() {
        assert!(mime_matches("image/jpeg", "image/jpeg"));
        assert!(!mime_matches("image/jpeg", "image/png"));
    }

    #[test]
    fn test_mime_matches_wildcard() {
        assert!(mime_matches("image/*", "image/jpeg"));
        assert!(mime_matches("image/*", "image/png"));
        assert!(!mime_matches("image/*", "application/pdf"));
    }

    #[test]
    fn test_mime_matches_full_wildcard() {
        assert!(mime_matches("*/*", "image/jpeg"));
        assert!(mime_matches("*", "application/pdf"));
    }

    #[test]
    fn test_mime_matches_case_insensitive() {
        assert!(mime_matches("Image/JPEG", "image/jpeg"));
        assert!(mime_matches("IMAGE/*", "image/png"));
    }

    #[test]
    fn test_declared_content_type_with_charset() {
        // "text/plain; charset=utf-8" should extract "text/plain"
        let jpeg_bytes = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46];
        let config = make_config(0, vec!["image/*", "text/*"], vec![]);
        let result = check_magic_bytes(
            &jpeg_bytes,
            Some("text/plain; charset=utf-8"),
            &config,
        );
        // text vs image → mismatch
        assert!(matches!(result, BodyCheckResult::ContentTypeMismatch { .. }));
    }

    // ── GIF detection ──

    #[test]
    fn test_magic_bytes_gif() {
        // GIF89a magic bytes
        let gif_bytes = b"GIF89a\x01\x00\x01\x00\x80\x00\x00";
        let config = make_config(0, vec!["image/*"], vec![]);
        let result = check_magic_bytes(gif_bytes, Some("image/gif"), &config);
        assert_eq!(result, BodyCheckResult::Allow);
    }

    // ── ZIP detection ──

    #[test]
    fn test_magic_bytes_zip_blocked() {
        // ZIP magic bytes: PK\x03\x04
        let zip_bytes = [0x50, 0x4B, 0x03, 0x04, 0x00, 0x00, 0x00, 0x00];
        let config = make_config(0, vec![], vec!["application/zip"]);
        let result = check_magic_bytes(&zip_bytes, Some("application/zip"), &config);
        assert!(matches!(result, BodyCheckResult::ContentTypeBlocked { .. }));
    }
}
