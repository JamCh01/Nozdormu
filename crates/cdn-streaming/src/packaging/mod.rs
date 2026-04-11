pub mod fmp4_gen;
pub mod hls_manifest;
pub mod mp4_parse;

use cdn_common::LlHlsConfig;

/// What type of packaging sub-resource is being requested.
#[derive(Debug, Clone, PartialEq)]
pub enum PackagingRequest {
    /// Generate the HLS m3u8 media playlist
    Manifest,
    /// Generate the LL-HLS m3u8 media playlist (with EXT-X-PART tags)
    LlHlsManifest,
    /// Generate the fMP4 initialization segment
    InitSegment,
    /// Generate a specific fMP4 media segment by index
    MediaSegment(u32),
    /// Generate a specific fMP4 partial segment (part) for LL-HLS
    PartialSegment {
        segment_index: u32,
        part_index: u32,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum PackagingError {
    #[error("MP4 parse error: {0}")]
    Mp4Parse(#[from] mp4_parse::Mp4Error),
    #[error("segment generation error: {0}")]
    SegmentGen(String),
    #[error("segment index {0} out of range (max {1})")]
    SegmentOutOfRange(u32, u32),
    #[error("part index {0} out of range for segment {1} (max parts {2})")]
    PartOutOfRange(u32, u32, u32),
}

/// Process a packaging request: parse MP4, generate the requested HLS sub-resource.
///
/// This is the main entry point called from `response_body_filter`.
/// Pass `ll_hls` config for LL-HLS requests; `None` for standard HLS.
pub fn process_packaging_request(
    mp4_data: &[u8],
    request: &PackagingRequest,
    segment_duration: f64,
    base_url: &str,
    query: Option<&str>,
    ll_hls: Option<&LlHlsConfig>,
) -> Result<Vec<u8>, PackagingError> {
    let metadata = mp4_parse::parse_mp4(mp4_data)?;

    match request {
        PackagingRequest::Manifest => {
            // Standard VOD playlist (unchanged)
            let query_base = filter_query_params(query);
            let playlist = hls_manifest::generate_media_playlist(
                &metadata,
                segment_duration,
                base_url,
                &query_base,
            );
            Ok(playlist.into_bytes())
        }
        PackagingRequest::LlHlsManifest => {
            let query_base = filter_query_params(query);
            let ll_config = ll_hls.ok_or_else(|| {
                PackagingError::SegmentGen(
                    "LL-HLS config required for LlHlsManifest".into(),
                )
            })?;
            let playlist = hls_manifest::generate_ll_hls_playlist(
                &metadata,
                segment_duration,
                ll_config.part_duration,
                base_url,
                &query_base,
            );
            Ok(playlist.into_bytes())
        }
        PackagingRequest::InitSegment => {
            let init = fmp4_gen::generate_init_segment(&metadata);
            Ok(init)
        }
        PackagingRequest::MediaSegment(index) => {
            let segment_count =
                hls_manifest::compute_segment_count(&metadata, segment_duration);
            if *index >= segment_count {
                return Err(PackagingError::SegmentOutOfRange(
                    *index,
                    segment_count,
                ));
            }
            let segment = fmp4_gen::generate_media_segment(
                mp4_data,
                &metadata,
                *index,
                segment_duration,
            )?;
            Ok(segment)
        }
        PackagingRequest::PartialSegment {
            segment_index,
            part_index,
        } => {
            let ll_config = ll_hls.ok_or_else(|| {
                PackagingError::SegmentGen(
                    "LL-HLS config required for PartialSegment".into(),
                )
            })?;
            let segment_count =
                hls_manifest::compute_segment_count(&metadata, segment_duration);
            if *segment_index >= segment_count {
                return Err(PackagingError::SegmentOutOfRange(
                    *segment_index,
                    segment_count,
                ));
            }
            let part_count = hls_manifest::compute_part_count(
                &metadata,
                segment_duration,
                ll_config.part_duration,
                *segment_index,
            );
            if *part_index >= part_count {
                return Err(PackagingError::PartOutOfRange(
                    *part_index,
                    *segment_index,
                    part_count,
                ));
            }
            let part = fmp4_gen::generate_partial_segment(
                mp4_data,
                &metadata,
                *segment_index,
                *part_index,
                segment_duration,
                ll_config.part_duration,
            )?;
            Ok(part)
        }
    }
}

/// Filter query string to remove packaging-related params, keeping the rest.
fn filter_query_params(query: Option<&str>) -> String {
    match query {
        None => String::new(),
        Some(q) => {
            let filtered: Vec<&str> = q
                .split('&')
                .filter(|p| {
                    !p.starts_with("format=")
                        && !p.starts_with("segment=")
                        && !p.starts_with("part=")
                        && !p.starts_with("_HLS_msn=")
                        && !p.starts_with("_HLS_part=")
                })
                .collect();
            if filtered.is_empty() {
                String::new()
            } else {
                format!("&{}", filtered.join("&"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_query_params_none() {
        assert_eq!(filter_query_params(None), "");
    }

    #[test]
    fn test_filter_query_params_only_format() {
        assert_eq!(filter_query_params(Some("format=hls")), "");
    }

    #[test]
    fn test_filter_query_params_mixed() {
        assert_eq!(
            filter_query_params(Some("format=hls&quality=high&segment=0")),
            "&quality=high"
        );
    }

    #[test]
    fn test_filter_query_params_preserved() {
        assert_eq!(filter_query_params(Some("a=1&b=2")), "&a=1&b=2");
    }

    #[test]
    fn test_filter_query_params_strips_ll_hls_params() {
        assert_eq!(
            filter_query_params(Some(
                "format=hls&_HLS_msn=5&_HLS_part=2&quality=high"
            )),
            "&quality=high"
        );
        assert_eq!(
            filter_query_params(Some("format=hls&part=3&segment=0")),
            ""
        );
    }
}
