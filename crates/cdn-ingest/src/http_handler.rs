use bytes::Bytes;
use std::time::Duration;

use crate::manifest;
use crate::store::LiveStreamStore;

/// Response from the live stream HTTP handler.
pub struct LiveResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Bytes,
    pub cache_control: &'static str,
}

/// Attempt to serve a live stream request.
///
/// Returns `Some(LiveResponse)` if the path matches a live stream URL,
/// or `None` if it should be handled by normal proxy logic.
///
/// URL patterns:
/// - `/live/{app}/{stream}.m3u8` — HLS manifest
/// - `/live/{app}/{stream}/init.mp4` — fMP4 init segment
/// - `/live/{app}/{stream}/seg{N}.mp4` — full segment
/// - `/live/{app}/{stream}/seg{N}_part{P}.mp4` — partial segment (LL-HLS)
pub async fn serve_live_request(
    store: &LiveStreamStore,
    path: &str,
    query: Option<&str>,
) -> Option<LiveResponse> {
    let path = path.strip_prefix("/live/")?;

    // Split into components
    let parts: Vec<&str> = path.splitn(3, '/').collect();
    if parts.len() < 2 {
        return None;
    }

    let app = parts[0];

    // Case 1: manifest — /live/{app}/{stream}.m3u8
    if parts.len() == 2 && parts[1].ends_with(".m3u8") {
        let stream_name = parts[1].strip_suffix(".m3u8")?;
        return serve_manifest(store, app, stream_name, query).await;
    }

    // Case 2: segment files — /live/{app}/{stream}/{file}
    if parts.len() == 3 {
        let stream_name = parts[1];
        let file = parts[2];

        if file == "init.mp4" {
            return serve_init_segment(store, app, stream_name).await;
        }

        // seg{N}.mp4
        if let Some(seq) = parse_segment_filename(file) {
            return serve_segment(store, app, stream_name, seq).await;
        }

        // seg{N}_part{P}.mp4
        if let Some((seq, part)) = parse_part_filename(file) {
            return serve_part(store, app, stream_name, seq, part).await;
        }
    }

    None
}

async fn serve_manifest(
    store: &LiveStreamStore,
    app: &str,
    stream_name: &str,
    query: Option<&str>,
) -> Option<LiveResponse> {
    let stream = store.get_stream(app, stream_name)?;

    // Check for blocking playlist reload params
    if let Some(q) = query {
        let (msn, part) = parse_hls_params(q);
        if let Some(target_msn) = msn {
            let _ = manifest::blocking::wait_for_availability(
                &stream,
                target_msn,
                part,
                Duration::from_secs(6),
            )
            .await;
        }
    }

    let s = stream.read().await;
    let base_url = format!("/live/{}/{}", app, stream_name);
    let playlist = manifest::generate_live_manifest(&s, &base_url);

    Some(LiveResponse {
        status: 200,
        content_type: "application/vnd.apple.mpegurl",
        body: Bytes::from(playlist),
        cache_control: "no-cache, no-store",
    })
}

async fn serve_init_segment(
    store: &LiveStreamStore,
    app: &str,
    stream_name: &str,
) -> Option<LiveResponse> {
    let stream = store.get_stream(app, stream_name)?;
    let s = stream.read().await;

    match &s.init_segment {
        Some(data) => Some(LiveResponse {
            status: 200,
            content_type: "video/mp4",
            body: data.clone(),
            cache_control: "max-age=86400",
        }),
        None => Some(LiveResponse {
            status: 404,
            content_type: "text/plain",
            body: Bytes::from_static(b"init segment not ready"),
            cache_control: "no-cache",
        }),
    }
}

async fn serve_segment(
    store: &LiveStreamStore,
    app: &str,
    stream_name: &str,
    sequence: u64,
) -> Option<LiveResponse> {
    let stream = store.get_stream(app, stream_name)?;
    let s = stream.read().await;

    for seg in &s.segments {
        if seg.sequence == sequence {
            return Some(LiveResponse {
                status: 200,
                content_type: "video/mp4",
                body: seg.data.clone(),
                cache_control: "max-age=3600",
            });
        }
    }

    Some(LiveResponse {
        status: 404,
        content_type: "text/plain",
        body: Bytes::from_static(b"segment not found"),
        cache_control: "no-cache",
    })
}

async fn serve_part(
    store: &LiveStreamStore,
    app: &str,
    stream_name: &str,
    sequence: u64,
    part_index: u32,
) -> Option<LiveResponse> {
    let stream = store.get_stream(app, stream_name)?;
    let s = stream.read().await;

    // Check completed segments
    for seg in &s.segments {
        if seg.sequence == sequence {
            if let Some(part) = seg.parts.iter().find(|p| p.index == part_index) {
                return Some(LiveResponse {
                    status: 200,
                    content_type: "video/mp4",
                    body: part.data.clone(),
                    cache_control: "max-age=1",
                });
            }
        }
    }

    // Check in-progress parts
    if sequence == s.current_part_sequence {
        if let Some(part) = s.current_parts.iter().find(|p| p.index == part_index) {
            return Some(LiveResponse {
                status: 200,
                content_type: "video/mp4",
                body: part.data.clone(),
                cache_control: "max-age=1",
            });
        }
    }

    Some(LiveResponse {
        status: 404,
        content_type: "text/plain",
        body: Bytes::from_static(b"part not found"),
        cache_control: "no-cache",
    })
}

/// Parse "seg{N}.mp4" → Some(N)
fn parse_segment_filename(file: &str) -> Option<u64> {
    let name = file.strip_prefix("seg")?.strip_suffix(".mp4")?;
    // Make sure it doesn't contain "_part"
    if name.contains('_') {
        return None;
    }
    name.parse().ok()
}

/// Parse "seg{N}_part{P}.mp4" → Some((N, P))
fn parse_part_filename(file: &str) -> Option<(u64, u32)> {
    let name = file.strip_prefix("seg")?.strip_suffix(".mp4")?;
    let (seq_str, part_str) = name.split_once("_part")?;
    let seq: u64 = seq_str.parse().ok()?;
    let part: u32 = part_str.parse().ok()?;
    Some((seq, part))
}

/// Parse `_HLS_msn` and `_HLS_part` from query string.
fn parse_hls_params(query: &str) -> (Option<u64>, Option<u32>) {
    let mut msn = None;
    let mut part = None;

    for param in query.split('&') {
        if let Some(val) = param.strip_prefix("_HLS_msn=") {
            msn = val.parse().ok();
        } else if let Some(val) = param.strip_prefix("_HLS_part=") {
            part = val.parse().ok();
        }
    }

    (msn, part)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_segment_filename() {
        assert_eq!(parse_segment_filename("seg0.mp4"), Some(0));
        assert_eq!(parse_segment_filename("seg123.mp4"), Some(123));
        assert_eq!(parse_segment_filename("seg0_part1.mp4"), None);
        assert_eq!(parse_segment_filename("init.mp4"), None);
        assert_eq!(parse_segment_filename("seg.mp4"), None);
    }

    #[test]
    fn test_parse_part_filename() {
        assert_eq!(parse_part_filename("seg0_part0.mp4"), Some((0, 0)));
        assert_eq!(parse_part_filename("seg5_part3.mp4"), Some((5, 3)));
        assert_eq!(parse_part_filename("seg0.mp4"), None);
        assert_eq!(parse_part_filename("init.mp4"), None);
    }

    #[test]
    fn test_parse_hls_params() {
        let (msn, part) = parse_hls_params("_HLS_msn=5&_HLS_part=2");
        assert_eq!(msn, Some(5));
        assert_eq!(part, Some(2));

        let (msn, part) = parse_hls_params("_HLS_msn=10");
        assert_eq!(msn, Some(10));
        assert_eq!(part, None);

        let (msn, part) = parse_hls_params("foo=bar");
        assert_eq!(msn, None);
        assert_eq!(part, None);
    }

    #[tokio::test]
    async fn test_serve_nonexistent_stream() {
        let store = LiveStreamStore::new(10, 100);
        let result = serve_live_request(&store, "/live/live/test.m3u8", None).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_non_live_path_returns_none() {
        let store = LiveStreamStore::new(10, 100);
        let result = serve_live_request(&store, "/other/path", None).await;
        assert!(result.is_none());
    }
}
