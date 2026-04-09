use cdn_common::{ImageFormat, ImageOptimizationConfig, ResizeFit};

/// Parsed and validated image transformation parameters from query string.
#[derive(Debug, Clone)]
pub struct ImageParams {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fit: ResizeFit,
    /// Explicit format from `?fmt=`. None means auto-negotiate from Accept header.
    pub format: Option<ImageFormat>,
    /// Output quality (1-100).
    pub quality: u32,
    /// Device pixel ratio multiplier.
    pub dpr: f32,
    /// True when format should be auto-negotiated (no explicit `?fmt=` or `?fmt=auto`).
    pub format_auto: bool,
}

impl ImageParams {
    /// Parse image params from a query string.
    /// Returns `None` if no image-related params are present (zero overhead for non-image requests).
    pub fn from_query(query: Option<&str>, config: &ImageOptimizationConfig) -> Option<Self> {
        let qs = query?;
        if qs.is_empty() {
            return None;
        }

        let mut width: Option<u32> = None;
        let mut height: Option<u32> = None;
        let mut fit = ResizeFit::default();
        let mut format: Option<ImageFormat> = None;
        let mut quality: Option<u32> = None;
        let mut dpr: Option<f32> = None;
        let mut format_auto = true;
        let mut has_image_param = false;

        for part in qs.split('&') {
            let (key, value) = match part.split_once('=') {
                Some((k, v)) => (k, v),
                None => continue,
            };

            match key {
                "w" | "width" => {
                    if let Ok(v) = value.parse::<u32>() {
                        if v > 0 {
                            width = Some(v);
                            has_image_param = true;
                        }
                    }
                }
                "h" | "height" => {
                    if let Ok(v) = value.parse::<u32>() {
                        if v > 0 {
                            height = Some(v);
                            has_image_param = true;
                        }
                    }
                }
                "fit" => {
                    fit = match value.to_lowercase().as_str() {
                        "contain" => ResizeFit::Contain,
                        "cover" => ResizeFit::Cover,
                        "fill" => ResizeFit::Fill,
                        "inside" => ResizeFit::Inside,
                        "outside" => ResizeFit::Outside,
                        _ => ResizeFit::default(),
                    };
                    has_image_param = true;
                }
                "fmt" | "format" => {
                    has_image_param = true;
                    if value == "auto" {
                        format_auto = true;
                    } else if let Some(f) = ImageFormat::from_token(value) {
                        format = Some(f);
                        format_auto = false;
                    }
                }
                "q" | "quality" => {
                    if let Ok(v) = value.parse::<u32>() {
                        quality = Some(v.clamp(1, 100));
                        has_image_param = true;
                    }
                }
                "dpr" => {
                    if let Ok(v) = value.parse::<f32>() {
                        dpr = Some(v.clamp(1.0, 4.0));
                        has_image_param = true;
                    }
                }
                _ => {}
            }
        }

        if !has_image_param {
            return None;
        }

        Some(Self {
            width,
            height,
            fit,
            format,
            quality: quality.unwrap_or(config.default_quality),
            dpr: dpr.unwrap_or(1.0),
            format_auto,
        })
    }

    /// Width after DPR multiplication.
    pub fn effective_width(&self) -> Option<u32> {
        self.width.map(|w| (w as f32 * self.dpr).round() as u32)
    }

    /// Height after DPR multiplication.
    pub fn effective_height(&self) -> Option<u32> {
        self.height.map(|h| (h as f32 * self.dpr).round() as u32)
    }

    /// Clamp dimensions to config limits.
    pub fn clamp(&mut self, config: &ImageOptimizationConfig) {
        if let Some(ref mut w) = self.width {
            *w = (*w).min(config.max_width);
        }
        if let Some(ref mut h) = self.height {
            *h = (*h).min(config.max_height);
        }
    }

    /// Returns true if any transformation is needed (resize, format change, or quality).
    pub fn needs_processing(&self) -> bool {
        self.width.is_some() || self.height.is_some() || self.format.is_some() || self.format_auto
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ImageOptimizationConfig {
        ImageOptimizationConfig::default()
    }

    #[test]
    fn test_parse_width_height() {
        let config = default_config();
        let params = ImageParams::from_query(Some("w=200&h=150"), &config).unwrap();
        assert_eq!(params.width, Some(200));
        assert_eq!(params.height, Some(150));
        assert_eq!(params.quality, 80); // default
        assert_eq!(params.dpr, 1.0);
        assert!(params.format_auto);
    }

    #[test]
    fn test_parse_all_params() {
        let config = default_config();
        let params = ImageParams::from_query(
            Some("w=400&h=300&fit=cover&fmt=webp&q=75&dpr=2"),
            &config,
        )
        .unwrap();
        assert_eq!(params.width, Some(400));
        assert_eq!(params.height, Some(300));
        assert_eq!(params.fit, ResizeFit::Cover);
        assert_eq!(params.format, Some(ImageFormat::WebP));
        assert_eq!(params.quality, 75);
        assert_eq!(params.dpr, 2.0);
        assert!(!params.format_auto);
    }

    #[test]
    fn test_parse_format_auto() {
        let config = default_config();
        let params = ImageParams::from_query(Some("w=200&fmt=auto"), &config).unwrap();
        assert!(params.format_auto);
        assert!(params.format.is_none());
    }

    #[test]
    fn test_parse_no_image_params() {
        let config = default_config();
        assert!(ImageParams::from_query(Some("page=1&sort=name"), &config).is_none());
    }

    #[test]
    fn test_parse_empty_query() {
        let config = default_config();
        assert!(ImageParams::from_query(Some(""), &config).is_none());
    }

    #[test]
    fn test_parse_none_query() {
        let config = default_config();
        assert!(ImageParams::from_query(None, &config).is_none());
    }

    #[test]
    fn test_quality_clamping() {
        let config = default_config();
        let params = ImageParams::from_query(Some("q=0"), &config).unwrap();
        assert_eq!(params.quality, 1);

        let params = ImageParams::from_query(Some("q=200"), &config).unwrap();
        assert_eq!(params.quality, 100);
    }

    #[test]
    fn test_dpr_clamping() {
        let config = default_config();
        let params = ImageParams::from_query(Some("w=100&dpr=0.5"), &config).unwrap();
        assert_eq!(params.dpr, 1.0);

        let params = ImageParams::from_query(Some("w=100&dpr=10"), &config).unwrap();
        assert_eq!(params.dpr, 4.0);
    }

    #[test]
    fn test_effective_dimensions_with_dpr() {
        let config = default_config();
        let params = ImageParams::from_query(Some("w=200&h=100&dpr=2"), &config).unwrap();
        assert_eq!(params.effective_width(), Some(400));
        assert_eq!(params.effective_height(), Some(200));
    }

    #[test]
    fn test_clamp_to_config() {
        let mut config = default_config();
        config.max_width = 500;
        config.max_height = 300;

        let mut params = ImageParams::from_query(Some("w=1000&h=800"), &config).unwrap();
        params.clamp(&config);
        assert_eq!(params.width, Some(500));
        assert_eq!(params.height, Some(300));
    }

    #[test]
    fn test_format_aliases() {
        let config = default_config();
        let params = ImageParams::from_query(Some("fmt=jpg"), &config).unwrap();
        assert_eq!(params.format, Some(ImageFormat::Jpeg));

        let params = ImageParams::from_query(Some("format=png"), &config).unwrap();
        assert_eq!(params.format, Some(ImageFormat::Png));
    }

    #[test]
    fn test_width_height_aliases() {
        let config = default_config();
        let params = ImageParams::from_query(Some("width=300&height=200"), &config).unwrap();
        assert_eq!(params.width, Some(300));
        assert_eq!(params.height, Some(200));
    }

    #[test]
    fn test_zero_dimensions_ignored() {
        let config = default_config();
        // w=0 should not set width
        let params = ImageParams::from_query(Some("w=0&fmt=webp"), &config).unwrap();
        assert!(params.width.is_none());
    }

    #[test]
    fn test_invalid_format_ignored() {
        let config = default_config();
        let params = ImageParams::from_query(Some("fmt=bmp"), &config);
        // "bmp" is not a valid output format token, but fmt= is still an image param
        assert!(params.is_some());
        let p = params.unwrap();
        assert!(p.format.is_none());
        assert!(p.format_auto); // falls back to auto
    }

    #[test]
    fn test_needs_processing() {
        let config = default_config();
        let params = ImageParams::from_query(Some("w=200"), &config).unwrap();
        assert!(params.needs_processing());

        let params = ImageParams::from_query(Some("fmt=webp"), &config).unwrap();
        assert!(params.needs_processing());
    }
}
