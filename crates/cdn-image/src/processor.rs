use cdn_common::{ImageFormat, ResizeFit};
use fast_image_resize::{images::Image as FirImage, FilterType, ResizeAlg, ResizeOptions, Resizer};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{DynamicImage, GenericImageView, ImageEncoder};

use crate::params::ImageParams;

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("decode error: {0}")]
    Decode(String),
    #[error("encode error: {0}")]
    Encode(String),
    #[error("resize error: {0}")]
    Resize(String),
    #[error("input too large: {size} bytes (max {max})")]
    InputTooLarge { size: u64, max: u64 },
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),
}

/// Crop rectangle for cover/outside fit modes.
struct CropRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

/// Process an image: decode -> resize -> format convert -> encode.
///
/// This function is synchronous and CPU-intensive. It should be called
/// from `response_body_filter` after the full body has been buffered.
pub fn process_image(
    input: &[u8],
    params: &ImageParams,
    output_format: &ImageFormat,
) -> Result<Vec<u8>, ImageError> {
    // 1. Decode
    let img = image::load_from_memory(input)
        .map_err(|e| ImageError::Decode(e.to_string()))?;

    let (orig_w, orig_h) = img.dimensions();

    // 2. Resize if dimensions specified
    let img = if params.effective_width().is_some() || params.effective_height().is_some() {
        let target_w = params.effective_width();
        let target_h = params.effective_height();
        resize_image(img, orig_w, orig_h, target_w, target_h, &params.fit)?
    } else {
        img
    };

    // 3. Encode to output format
    encode_image(&img, output_format, params.quality)
}

/// Resize an image according to the fit mode.
fn resize_image(
    img: DynamicImage,
    orig_w: u32,
    orig_h: u32,
    target_w: Option<u32>,
    target_h: Option<u32>,
    fit: &ResizeFit,
) -> Result<DynamicImage, ImageError> {
    let (resize_w, resize_h, crop) =
        calculate_dimensions(orig_w, orig_h, target_w, target_h, fit);

    // No resize needed if dimensions match
    if resize_w == orig_w && resize_h == orig_h && crop.is_none() {
        return Ok(img);
    }

    // Skip if target is larger and fit mode prevents enlargement
    if matches!(fit, ResizeFit::Inside | ResizeFit::Outside)
        && resize_w >= orig_w
        && resize_h >= orig_h
    {
        return Ok(img);
    }

    // Convert to RGBA8 for fast_image_resize
    let rgba = img.to_rgba8();

    let src_image = FirImage::from_vec_u8(
        orig_w,
        orig_h,
        rgba.into_raw(),
        fast_image_resize::PixelType::U8x4,
    )
    .map_err(|e| ImageError::Resize(e.to_string()))?;

    let mut dst_image = FirImage::new(resize_w, resize_h, fast_image_resize::PixelType::U8x4);

    let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3));
    let mut resizer = Resizer::new();
    resizer
        .resize(&src_image, &mut dst_image, &options)
        .map_err(|e| ImageError::Resize(e.to_string()))?;

    let resized_buf = dst_image.into_vec();
    let resized_img: DynamicImage = DynamicImage::ImageRgba8(
        image::RgbaImage::from_raw(resize_w, resize_h, resized_buf)
            .ok_or_else(|| ImageError::Resize("failed to create resized image".to_string()))?,
    );

    // Apply crop if needed (cover/outside modes)
    if let Some(crop) = crop {
        Ok(resized_img.crop_imm(crop.x, crop.y, crop.width, crop.height))
    } else {
        Ok(resized_img)
    }
}

/// Calculate output dimensions and optional crop rect based on fit mode.
fn calculate_dimensions(
    orig_w: u32,
    orig_h: u32,
    target_w: Option<u32>,
    target_h: Option<u32>,
    fit: &ResizeFit,
) -> (u32, u32, Option<CropRect>) {
    let tw = target_w.unwrap_or(orig_w);
    let th = target_h.unwrap_or(orig_h);

    if tw == 0 || th == 0 || orig_w == 0 || orig_h == 0 {
        return (orig_w.max(1), orig_h.max(1), None);
    }

    let ratio_w = tw as f64 / orig_w as f64;
    let ratio_h = th as f64 / orig_h as f64;

    match fit {
        ResizeFit::Contain | ResizeFit::Inside => {
            let scale = ratio_w.min(ratio_h);
            let scale = match fit {
                ResizeFit::Inside => scale.min(1.0), // never enlarge
                _ => scale,
            };
            let w = (orig_w as f64 * scale).round() as u32;
            let h = (orig_h as f64 * scale).round() as u32;
            (w.max(1), h.max(1), None)
        }
        ResizeFit::Cover | ResizeFit::Outside => {
            let scale = ratio_w.max(ratio_h);
            let scale = match fit {
                ResizeFit::Outside => scale.min(1.0), // never enlarge
                _ => scale,
            };
            let resize_w = (orig_w as f64 * scale).round() as u32;
            let resize_h = (orig_h as f64 * scale).round() as u32;

            // Only crop if both target dimensions were specified
            let crop = if target_w.is_some() && target_h.is_some() && (resize_w > tw || resize_h > th) {
                let crop_x = (resize_w.saturating_sub(tw)) / 2;
                let crop_y = (resize_h.saturating_sub(th)) / 2;
                Some(CropRect {
                    x: crop_x,
                    y: crop_y,
                    width: tw.min(resize_w),
                    height: th.min(resize_h),
                })
            } else {
                None
            };

            (resize_w.max(1), resize_h.max(1), crop)
        }
        ResizeFit::Fill => {
            // Stretch to exact dimensions
            (tw, th, None)
        }
    }
}

/// Encode a DynamicImage to the specified format with quality.
fn encode_image(
    img: &DynamicImage,
    format: &ImageFormat,
    quality: u32,
) -> Result<Vec<u8>, ImageError> {
    let mut buf = Vec::new();

    match format {
        ImageFormat::Jpeg => {
            let rgba = img.to_rgb8();
            let encoder = JpegEncoder::new_with_quality(&mut buf, quality as u8);
            encoder
                .write_image(
                    rgba.as_raw(),
                    rgba.width(),
                    rgba.height(),
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(|e| ImageError::Encode(e.to_string()))?;
        }
        ImageFormat::Png => {
            let rgba = img.to_rgba8();
            let encoder = PngEncoder::new(&mut buf);
            encoder
                .write_image(
                    rgba.as_raw(),
                    rgba.width(),
                    rgba.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|e| ImageError::Encode(e.to_string()))?;
        }
        ImageFormat::WebP => {
            let rgba = img.to_rgba8();
            // image 0.25 WebP encoder only supports lossless
            let encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut buf);
            encoder
                .write_image(
                    rgba.as_raw(),
                    rgba.width(),
                    rgba.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|e| ImageError::Encode(e.to_string()))?;
        }
        ImageFormat::Avif => {
            let rgba = img.to_rgba8();
            let encoder = image::codecs::avif::AvifEncoder::new_with_speed_quality(
                &mut buf,
                6,     // speed (1=slow/best, 10=fast/worst)
                quality as u8,
            );
            encoder
                .write_image(
                    rgba.as_raw(),
                    rgba.width(),
                    rgba.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|e| ImageError::Encode(e.to_string()))?;
        }
    }

    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Create a small test JPEG image in memory.
    fn create_test_jpeg(width: u32, height: u32) -> Vec<u8> {
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        }));
        let mut buf = Vec::new();
        let encoder = JpegEncoder::new_with_quality(&mut buf, 90);
        encoder
            .write_image(
                img.to_rgb8().as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
        buf
    }

    /// Create a small test PNG image in memory.
    fn create_test_png(width: u32, height: u32) -> Vec<u8> {
        let img = DynamicImage::ImageRgba8(image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        }));
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    fn make_params(w: Option<u32>, h: Option<u32>, fit: ResizeFit, quality: u32) -> ImageParams {
        ImageParams {
            width: w,
            height: h,
            fit,
            format: None,
            quality,
            dpr: 1.0,
            format_auto: true,
        }
    }

    #[test]
    fn test_jpeg_to_jpeg_resize() {
        let input = create_test_jpeg(100, 80);
        let params = make_params(Some(50), Some(40), ResizeFit::Contain, 80);
        let output = process_image(&input, &params, &ImageFormat::Jpeg).unwrap();
        assert!(!output.is_empty());
        // Verify it's a valid JPEG
        let decoded = image::load_from_memory(&output).unwrap();
        assert_eq!(decoded.width(), 50);
        assert_eq!(decoded.height(), 40);
    }

    #[test]
    fn test_jpeg_to_png() {
        let input = create_test_jpeg(60, 40);
        let params = make_params(None, None, ResizeFit::Contain, 80);
        let output = process_image(&input, &params, &ImageFormat::Png).unwrap();
        assert!(!output.is_empty());
        let decoded = image::load_from_memory(&output).unwrap();
        assert_eq!(decoded.width(), 60);
        assert_eq!(decoded.height(), 40);
    }

    #[test]
    fn test_jpeg_to_webp() {
        let input = create_test_jpeg(60, 40);
        let params = make_params(Some(30), None, ResizeFit::Contain, 75);
        let output = process_image(&input, &params, &ImageFormat::WebP).unwrap();
        assert!(!output.is_empty());
    }

    #[test]
    fn test_png_to_jpeg() {
        let input = create_test_png(80, 60);
        let params = make_params(Some(40), Some(30), ResizeFit::Fill, 90);
        let output = process_image(&input, &params, &ImageFormat::Jpeg).unwrap();
        assert!(!output.is_empty());
        let decoded = image::load_from_memory(&output).unwrap();
        assert_eq!(decoded.width(), 40);
        assert_eq!(decoded.height(), 30);
    }

    #[test]
    fn test_contain_preserves_aspect_ratio() {
        let input = create_test_jpeg(200, 100);
        let params = make_params(Some(100), Some(100), ResizeFit::Contain, 80);
        let output = process_image(&input, &params, &ImageFormat::Jpeg).unwrap();
        let decoded = image::load_from_memory(&output).unwrap();
        // 200x100 into 100x100 contain → 100x50
        assert_eq!(decoded.width(), 100);
        assert_eq!(decoded.height(), 50);
    }

    #[test]
    fn test_cover_crops_to_target() {
        let input = create_test_jpeg(200, 100);
        let params = make_params(Some(100), Some(100), ResizeFit::Cover, 80);
        let output = process_image(&input, &params, &ImageFormat::Jpeg).unwrap();
        let decoded = image::load_from_memory(&output).unwrap();
        // 200x100 into 100x100 cover → scale to 200x100 (ratio_h=1.0 wins), crop center to 100x100
        assert_eq!(decoded.width(), 100);
        assert_eq!(decoded.height(), 100);
    }

    #[test]
    fn test_fill_stretches() {
        let input = create_test_jpeg(200, 100);
        let params = make_params(Some(50), Some(80), ResizeFit::Fill, 80);
        let output = process_image(&input, &params, &ImageFormat::Jpeg).unwrap();
        let decoded = image::load_from_memory(&output).unwrap();
        assert_eq!(decoded.width(), 50);
        assert_eq!(decoded.height(), 80);
    }

    #[test]
    fn test_inside_never_enlarges() {
        let input = create_test_jpeg(50, 40);
        let params = make_params(Some(200), Some(200), ResizeFit::Inside, 80);
        let output = process_image(&input, &params, &ImageFormat::Jpeg).unwrap();
        let decoded = image::load_from_memory(&output).unwrap();
        // Should not enlarge
        assert_eq!(decoded.width(), 50);
        assert_eq!(decoded.height(), 40);
    }

    #[test]
    fn test_width_only_resize() {
        let input = create_test_jpeg(200, 100);
        let params = make_params(Some(100), None, ResizeFit::Contain, 80);
        let output = process_image(&input, &params, &ImageFormat::Jpeg).unwrap();
        let decoded = image::load_from_memory(&output).unwrap();
        // Width 100, height proportional: 100 * (100/200) = 50
        assert_eq!(decoded.width(), 100);
        assert_eq!(decoded.height(), 50);
    }

    #[test]
    fn test_corrupt_input() {
        let params = make_params(Some(50), Some(50), ResizeFit::Contain, 80);
        let result = process_image(b"not an image", &params, &ImageFormat::Jpeg);
        assert!(result.is_err());
        match result.unwrap_err() {
            ImageError::Decode(_) => {}
            other => panic!("expected Decode error, got: {}", other),
        }
    }

    #[test]
    fn test_no_resize_format_only() {
        let input = create_test_jpeg(100, 80);
        let params = make_params(None, None, ResizeFit::Contain, 80);
        let output = process_image(&input, &params, &ImageFormat::Jpeg).unwrap();
        assert!(!output.is_empty());
        let decoded = image::load_from_memory(&output).unwrap();
        assert_eq!(decoded.width(), 100);
        assert_eq!(decoded.height(), 80);
    }

    #[test]
    fn test_calculate_dimensions_contain() {
        let (w, h, crop) = calculate_dimensions(200, 100, Some(100), Some(100), &ResizeFit::Contain);
        assert_eq!(w, 100);
        assert_eq!(h, 50);
        assert!(crop.is_none());
    }

    #[test]
    fn test_calculate_dimensions_cover() {
        let (w, h, crop) = calculate_dimensions(200, 100, Some(100), Some(100), &ResizeFit::Cover);
        assert_eq!(w, 200);
        assert_eq!(h, 100);
        assert!(crop.is_some());
        let c = crop.unwrap();
        assert_eq!(c.width, 100);
        assert_eq!(c.height, 100);
    }

    #[test]
    fn test_calculate_dimensions_fill() {
        let (w, h, crop) = calculate_dimensions(200, 100, Some(50), Some(80), &ResizeFit::Fill);
        assert_eq!(w, 50);
        assert_eq!(h, 80);
        assert!(crop.is_none());
    }

    #[test]
    fn test_dpr_applied() {
        let input = create_test_jpeg(400, 200);
        let params = ImageParams {
            width: Some(100),
            height: Some(50),
            fit: ResizeFit::Contain,
            format: None,
            quality: 80,
            dpr: 2.0,
            format_auto: true,
        };
        // effective: 200x100
        let output = process_image(&input, &params, &ImageFormat::Jpeg).unwrap();
        let decoded = image::load_from_memory(&output).unwrap();
        assert_eq!(decoded.width(), 200);
        assert_eq!(decoded.height(), 100);
    }
}
