use cdn_common::{ImageFormat, ImageOptimizationConfig};

/// Negotiate the best output image format.
///
/// Priority:
/// 1. Explicit `?fmt=` param (highest)
/// 2. Server-priority formats from config, filtered by client `Accept` header
/// 3. Original format (fallback)
///
/// Returns `(format, was_auto_negotiated)`. When `was_auto_negotiated` is true,
/// the response should include `Vary: Accept`.
pub fn negotiate_format(
    accept: &str,
    explicit_fmt: Option<&ImageFormat>,
    config: &ImageOptimizationConfig,
    original_content_type: &str,
) -> (ImageFormat, bool) {
    // 1. Explicit format takes priority
    if let Some(fmt) = explicit_fmt {
        return (fmt.clone(), false);
    }

    // 2. Auto-negotiate from Accept header using server priority
    let accepted = parse_accept_image(accept);

    for server_fmt in &config.formats {
        let token = server_fmt.accept_token();
        if client_accepts(&accepted, token) {
            return (server_fmt.clone(), true);
        }
    }

    // 3. Fall back to original format
    let original = format_from_content_type(original_content_type).unwrap_or(ImageFormat::Jpeg);
    (original, false)
}

/// Check if a Content-Type is an optimizable image type.
pub fn is_optimizable_image(content_type: &str, config: &ImageOptimizationConfig) -> bool {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_lowercase();

    for pattern in &config.optimizable_types {
        let pattern = pattern.to_lowercase();
        if pattern.ends_with("/*") {
            let prefix = &pattern[..pattern.len() - 1];
            if mime.starts_with(prefix) {
                return true;
            }
        } else if mime == pattern {
            return true;
        }
    }
    false
}

/// Parse the `Accept` header into a list of (mime_type, quality) pairs.
fn parse_accept_image(header: &str) -> Vec<(&str, f32)> {
    header
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            let mut iter = part.splitn(2, ';');
            let token = iter.next()?.trim();
            let quality = iter
                .next()
                .and_then(|params| {
                    params
                        .trim()
                        .strip_prefix("q=")
                        .and_then(|q| q.trim().parse::<f32>().ok())
                })
                .unwrap_or(1.0);
            Some((token, quality))
        })
        .collect()
}

/// Check if the client accepts a given MIME type (quality > 0).
/// Supports exact match and wildcard `image/*` and `*/*`.
fn client_accepts(accepted: &[(&str, f32)], mime: &str) -> bool {
    let mime_lower = mime.to_lowercase();
    let type_prefix = mime_lower.split('/').next().unwrap_or("");

    for &(token, quality) in accepted {
        if quality <= 0.0 {
            continue;
        }
        let token_lower = token.to_lowercase();
        if token_lower == mime_lower {
            return true;
        }
        // Wildcard: image/* matches image/avif
        if token_lower == format!("{}/*", type_prefix) {
            return true;
        }
        // Universal wildcard
        if token_lower == "*/*" {
            return true;
        }
    }
    false
}

/// Derive ImageFormat from a Content-Type string.
fn format_from_content_type(ct: &str) -> Option<ImageFormat> {
    let mime = ct.split(';').next().unwrap_or(ct).trim().to_lowercase();

    match mime.as_str() {
        "image/jpeg" | "image/jpg" => Some(ImageFormat::Jpeg),
        "image/png" => Some(ImageFormat::Png),
        "image/webp" => Some(ImageFormat::WebP),
        "image/avif" => Some(ImageFormat::Avif),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ImageOptimizationConfig {
        ImageOptimizationConfig::default()
    }

    #[test]
    fn test_negotiate_explicit_format() {
        let config = default_config();
        let (fmt, auto) = negotiate_format(
            "image/webp,*/*",
            Some(&ImageFormat::Png),
            &config,
            "image/jpeg",
        );
        assert_eq!(fmt, ImageFormat::Png);
        assert!(!auto);
    }

    #[test]
    fn test_negotiate_avif_preferred() {
        let config = default_config(); // formats: [Avif, WebP]
        let (fmt, auto) = negotiate_format(
            "image/avif,image/webp,image/*,*/*;q=0.8",
            None,
            &config,
            "image/jpeg",
        );
        assert_eq!(fmt, ImageFormat::Avif);
        assert!(auto);
    }

    #[test]
    fn test_negotiate_webp_when_no_avif() {
        let config = default_config();
        let (fmt, auto) = negotiate_format("image/webp,image/jpeg", None, &config, "image/jpeg");
        assert_eq!(fmt, ImageFormat::WebP);
        assert!(auto);
    }

    #[test]
    fn test_negotiate_fallback_to_original() {
        let config = default_config();
        let (fmt, auto) = negotiate_format("image/jpeg", None, &config, "image/png");
        // Neither AVIF nor WebP accepted, fall back to original (PNG)
        assert_eq!(fmt, ImageFormat::Png);
        assert!(!auto);
    }

    #[test]
    fn test_negotiate_wildcard_accepts_all() {
        let config = default_config();
        let (fmt, auto) = negotiate_format("*/*", None, &config, "image/jpeg");
        // */* matches everything, so server priority wins (Avif)
        assert_eq!(fmt, ImageFormat::Avif);
        assert!(auto);
    }

    #[test]
    fn test_negotiate_image_wildcard() {
        let config = default_config();
        let (fmt, auto) = negotiate_format("image/*", None, &config, "image/jpeg");
        assert_eq!(fmt, ImageFormat::Avif);
        assert!(auto);
    }

    #[test]
    fn test_negotiate_q0_rejected() {
        let mut config = default_config();
        config.formats = vec![ImageFormat::WebP];
        let (fmt, auto) =
            negotiate_format("image/webp;q=0,image/jpeg", None, &config, "image/jpeg");
        // WebP has q=0, should be rejected
        assert_eq!(fmt, ImageFormat::Jpeg);
        assert!(!auto);
    }

    #[test]
    fn test_is_optimizable_image() {
        let config = default_config();
        assert!(is_optimizable_image("image/jpeg", &config));
        assert!(is_optimizable_image("image/png", &config));
        assert!(is_optimizable_image("image/gif", &config));
        assert!(is_optimizable_image("image/bmp", &config));
        assert!(is_optimizable_image("image/tiff", &config));
        assert!(is_optimizable_image("image/jpeg; charset=utf-8", &config));
        // Not in default list
        assert!(!is_optimizable_image("image/webp", &config));
        assert!(!is_optimizable_image("image/avif", &config));
        assert!(!is_optimizable_image("image/svg+xml", &config));
        assert!(!is_optimizable_image("text/html", &config));
    }

    #[test]
    fn test_is_optimizable_wildcard() {
        let config = ImageOptimizationConfig {
            optimizable_types: vec!["image/*".to_string()],
            ..Default::default()
        };
        assert!(is_optimizable_image("image/jpeg", &config));
        assert!(is_optimizable_image("image/webp", &config));
        assert!(!is_optimizable_image("text/html", &config));
    }

    #[test]
    fn test_parse_accept_image() {
        let accepted = parse_accept_image("image/avif,image/webp;q=0.9,*/*;q=0.1");
        assert_eq!(accepted.len(), 3);
        assert_eq!(accepted[0], ("image/avif", 1.0));
        assert_eq!(accepted[1], ("image/webp", 0.9));
        assert_eq!(accepted[2], ("*/*", 0.1));
    }

    #[test]
    fn test_format_from_content_type() {
        assert_eq!(
            format_from_content_type("image/jpeg"),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(
            format_from_content_type("image/png"),
            Some(ImageFormat::Png)
        );
        assert_eq!(
            format_from_content_type("image/webp"),
            Some(ImageFormat::WebP)
        );
        assert_eq!(
            format_from_content_type("image/avif"),
            Some(ImageFormat::Avif)
        );
        assert_eq!(format_from_content_type("image/gif"), None);
        assert_eq!(format_from_content_type("text/html"), None);
    }

    #[test]
    fn test_empty_accept_header() {
        let config = default_config();
        let (fmt, auto) = negotiate_format("", None, &config, "image/jpeg");
        assert_eq!(fmt, ImageFormat::Jpeg);
        assert!(!auto);
    }
}
