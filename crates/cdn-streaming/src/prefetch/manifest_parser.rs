//! HLS and DASH manifest parsers for extracting segment URLs.

/// Extract segment URLs from an HLS (M3U8) manifest.
///
/// Parses `#EXTINF` lines and collects the subsequent URI lines.
/// Resolves relative URIs against `base_url`.
pub fn extract_hls_segments(manifest: &str, base_url: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut next_is_segment = false;

    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with("#EXTINF:") {
            next_is_segment = true;
            continue;
        }
        if next_is_segment && !line.is_empty() && !line.starts_with('#') {
            let url = resolve_url(line, base_url);
            segments.push(url);
            next_is_segment = false;
        }
    }

    segments
}

/// Extract segment URLs from a DASH MPD manifest.
///
/// Handles `<SegmentTemplate>` with `media` attribute and `$Number$` substitution.
/// Also handles `<SegmentList>` with `<SegmentURL>` elements.
pub fn extract_dash_segments(mpd: &str, base_url: &str) -> Vec<String> {
    let mut segments = Vec::new();

    // Simple XML parsing for SegmentTemplate media patterns
    // Look for media="..." attribute in SegmentTemplate
    if let Some(template) = extract_xml_attr(mpd, "SegmentTemplate", "media") {
        // Look for startNumber and duration/timescale to compute segment count
        let start_number = extract_xml_attr(mpd, "SegmentTemplate", "startNumber")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(1);

        let duration = extract_xml_attr(mpd, "SegmentTemplate", "duration")
            .and_then(|s| s.parse::<u64>().ok());
        let timescale = extract_xml_attr(mpd, "SegmentTemplate", "timescale")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1);

        // Get total duration from MPD
        let total_duration = extract_xml_attr(mpd, "MPD", "mediaPresentationDuration")
            .and_then(|s| parse_iso_duration(&s));

        if let (Some(seg_dur), Some(total)) = (duration, total_duration) {
            let seg_dur_secs = seg_dur as f64 / timescale as f64;
            let segment_count = (total / seg_dur_secs).ceil() as u32;

            for i in 0..segment_count {
                let number = start_number + i;
                let url = template.replace("$Number$", &number.to_string());
                segments.push(resolve_url(&url, base_url));
            }
        } else {
            // Can't determine count, just generate a few
            for i in 0..10 {
                let number = start_number + i;
                let url = template.replace("$Number$", &number.to_string());
                segments.push(resolve_url(&url, base_url));
            }
        }
    }

    // Also check for SegmentList > SegmentURL
    for segment_url in extract_segment_urls(mpd) {
        segments.push(resolve_url(&segment_url, base_url));
    }

    segments
}

/// Resolve a potentially relative URL against a base URL.
fn resolve_url(url: &str, base_url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("//") {
        return url.to_string();
    }

    if url.starts_with('/') {
        // Absolute path — extract scheme+host from base
        if let Some(pos) = base_url.find("://") {
            if let Some(slash_pos) = base_url[pos + 3..].find('/') {
                let origin = &base_url[..pos + 3 + slash_pos];
                return format!("{}{}", origin, url);
            }
        }
        return url.to_string();
    }

    // Relative path — resolve against base directory
    if let Some(last_slash) = base_url.rfind('/') {
        format!("{}/{}", &base_url[..last_slash], url)
    } else {
        url.to_string()
    }
}

/// Extract an XML attribute value from a simple XML string.
/// This is a basic parser — not a full XML parser.
fn extract_xml_attr(xml: &str, element: &str, attr: &str) -> Option<String> {
    let tag_start = format!("<{}", element);
    let pos = xml.find(&tag_start)?;
    let rest = &xml[pos..];
    let tag_end = rest.find('>')?;
    let tag = &rest[..tag_end];

    let attr_pattern = format!("{}=\"", attr);
    let attr_pos = tag.find(&attr_pattern)?;
    let value_start = attr_pos + attr_pattern.len();
    let value_end = tag[value_start..].find('"')?;
    Some(tag[value_start..value_start + value_end].to_string())
}

/// Extract SegmentURL media attributes from a DASH MPD.
fn extract_segment_urls(mpd: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let pattern = "media=\"";
    let mut search_from = 0;

    while let Some(pos) = mpd[search_from..].find("<SegmentURL") {
        let abs_pos = search_from + pos;
        let rest = &mpd[abs_pos..];
        if let Some(media_pos) = rest.find(pattern) {
            let value_start = media_pos + pattern.len();
            if let Some(value_end) = rest[value_start..].find('"') {
                urls.push(rest[value_start..value_start + value_end].to_string());
            }
        }
        search_from = abs_pos + 1;
    }

    urls
}

/// Parse an ISO 8601 duration string (e.g., "PT1H30M15.5S") to seconds.
fn parse_iso_duration(s: &str) -> Option<f64> {
    let s = s.strip_prefix("PT")?;
    let mut total = 0.0;
    let mut num_str = String::new();

    for ch in s.chars() {
        match ch {
            'H' => {
                total += num_str.parse::<f64>().ok()? * 3600.0;
                num_str.clear();
            }
            'M' => {
                total += num_str.parse::<f64>().ok()? * 60.0;
                num_str.clear();
            }
            'S' => {
                total += num_str.parse::<f64>().ok()?;
                num_str.clear();
            }
            _ => num_str.push(ch),
        }
    }

    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_hls_segments_basic() {
        let manifest = r#"#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:10
#EXTINF:9.009,
segment0.ts
#EXTINF:9.009,
segment1.ts
#EXTINF:3.003,
segment2.ts
#EXT-X-ENDLIST"#;

        let segments = extract_hls_segments(manifest, "http://cdn.example.com/live/");
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0], "http://cdn.example.com/live/segment0.ts");
        assert_eq!(segments[1], "http://cdn.example.com/live/segment1.ts");
        assert_eq!(segments[2], "http://cdn.example.com/live/segment2.ts");
    }

    #[test]
    fn test_extract_hls_segments_absolute_urls() {
        let manifest = r#"#EXTM3U
#EXTINF:10.0,
http://origin.example.com/seg0.ts
#EXTINF:10.0,
http://origin.example.com/seg1.ts
#EXT-X-ENDLIST"#;

        let segments = extract_hls_segments(manifest, "http://cdn.example.com/");
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0], "http://origin.example.com/seg0.ts");
    }

    #[test]
    fn test_extract_hls_segments_empty() {
        let manifest = "#EXTM3U\n#EXT-X-ENDLIST";
        let segments = extract_hls_segments(manifest, "http://cdn.example.com/");
        assert!(segments.is_empty());
    }

    #[test]
    fn test_extract_dash_segments_template() {
        let mpd = r#"<?xml version="1.0"?>
<MPD mediaPresentationDuration="PT30S">
  <Period>
    <AdaptationSet>
      <SegmentTemplate media="segment_$Number$.m4s" startNumber="1" duration="6000" timescale="1000"/>
    </AdaptationSet>
  </Period>
</MPD>"#;

        let segments = extract_dash_segments(mpd, "http://cdn.example.com/video/");
        assert_eq!(segments.len(), 5); // 30s / 6s = 5 segments
        assert_eq!(segments[0], "http://cdn.example.com/video/segment_1.m4s");
        assert_eq!(segments[4], "http://cdn.example.com/video/segment_5.m4s");
    }

    #[test]
    fn test_extract_dash_segment_urls() {
        let mpd = r#"<MPD>
  <Period>
    <AdaptationSet>
      <SegmentList>
        <SegmentURL media="seg1.m4s"/>
        <SegmentURL media="seg2.m4s"/>
      </SegmentList>
    </AdaptationSet>
  </Period>
</MPD>"#;

        let segments = extract_dash_segments(mpd, "http://cdn.example.com/");
        assert!(segments.iter().any(|s| s.contains("seg1.m4s")));
        assert!(segments.iter().any(|s| s.contains("seg2.m4s")));
    }

    #[test]
    fn test_resolve_url_absolute() {
        assert_eq!(
            resolve_url("http://other.com/seg.ts", "http://cdn.com/"),
            "http://other.com/seg.ts"
        );
    }

    #[test]
    fn test_resolve_url_absolute_path() {
        assert_eq!(
            resolve_url("/video/seg.ts", "http://cdn.com/live/index.m3u8"),
            "http://cdn.com/video/seg.ts"
        );
    }

    #[test]
    fn test_resolve_url_relative() {
        assert_eq!(
            resolve_url("seg.ts", "http://cdn.com/live/index.m3u8"),
            "http://cdn.com/live/seg.ts"
        );
    }

    #[test]
    fn test_parse_iso_duration() {
        assert_eq!(parse_iso_duration("PT30S"), Some(30.0));
        assert_eq!(parse_iso_duration("PT1M30S"), Some(90.0));
        assert_eq!(parse_iso_duration("PT1H30M15.5S"), Some(5415.5));
        assert_eq!(parse_iso_duration("PT0S"), Some(0.0));
    }

    #[test]
    fn test_parse_iso_duration_invalid() {
        assert_eq!(parse_iso_duration("30S"), None); // Missing PT prefix
    }
}
