pub mod blocking;

use crate::store::LiveStream;

/// Generate a live HLS media playlist from the current stream state.
///
/// Produces a sliding-window playlist (no `#EXT-X-ENDLIST` unless the stream has ended).
/// When LL-HLS is enabled, includes `#EXT-X-PART` tags for partial segments.
pub fn generate_live_manifest(stream: &LiveStream, base_url: &str) -> String {
    let mut playlist = String::with_capacity(4096);

    playlist.push_str("#EXTM3U\n");

    let version = if stream.ll_hls_enabled { 9 } else { 7 };
    playlist.push_str(&format!("#EXT-X-VERSION:{}\n", version));

    // Target duration: ceiling of max segment duration
    let max_duration = stream
        .segments
        .iter()
        .map(|s| s.duration)
        .fold(stream.segment_duration, f64::max);
    playlist.push_str(&format!(
        "#EXT-X-TARGETDURATION:{}\n",
        max_duration.ceil() as u64
    ));

    playlist.push_str(&format!(
        "#EXT-X-MEDIA-SEQUENCE:{}\n",
        stream.media_sequence
    ));

    // LL-HLS server control
    if stream.ll_hls_enabled {
        let part_hold_back = stream.part_duration * 3.0;
        playlist.push_str(&format!(
            "#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,PART-HOLD-BACK={:.6}\n",
            part_hold_back
        ));
        playlist.push_str(&format!(
            "#EXT-X-PART-INF:PART-TARGET={:.6}\n",
            stream.part_duration
        ));
    }

    // Init segment
    if stream.init_segment.is_some() {
        playlist.push_str(&format!("#EXT-X-MAP:URI=\"{}/init.mp4\"\n", base_url));
    }

    // Completed segments
    for seg in &stream.segments {
        // LL-HLS: emit part tags before the segment
        if stream.ll_hls_enabled {
            for part in &seg.parts {
                playlist.push_str(&format!(
                    "#EXT-X-PART:DURATION={:.6},URI=\"{}/seg{}_part{}.mp4\"",
                    part.duration, base_url, seg.sequence, part.index
                ));
                if part.independent {
                    playlist.push_str(",INDEPENDENT=YES");
                }
                playlist.push('\n');
            }
        }

        playlist.push_str(&format!("#EXTINF:{:.6},\n", seg.duration));
        playlist.push_str(&format!("{}/seg{}.mp4\n", base_url, seg.sequence));
    }

    // In-progress segment parts (LL-HLS)
    if stream.ll_hls_enabled && !stream.current_parts.is_empty() {
        for part in &stream.current_parts {
            playlist.push_str(&format!(
                "#EXT-X-PART:DURATION={:.6},URI=\"{}/seg{}_part{}.mp4\"",
                part.duration, base_url, stream.current_part_sequence, part.index
            ));
            if part.independent {
                playlist.push_str(",INDEPENDENT=YES");
            }
            playlist.push('\n');
        }

        // Preload hint for the next expected part
        let next_part_index = stream.current_parts.len() as u32;
        playlist.push_str(&format!(
            "#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"{}/seg{}_part{}.mp4\"\n",
            base_url, stream.current_part_sequence, next_part_index
        ));
    }

    // End list if stream has ended
    if stream.ended {
        playlist.push_str("#EXT-X-ENDLIST\n");
    }

    playlist
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{LivePart, LiveSegment};
    use bytes::Bytes;
    use std::collections::VecDeque;

    fn make_test_stream(ll_hls: bool) -> LiveStream {
        let mut segments = VecDeque::new();
        segments.push_back(LiveSegment {
            sequence: 5,
            duration: 6.0,
            data: Bytes::from(vec![0u8; 100]),
            parts: if ll_hls {
                vec![
                    LivePart {
                        index: 0,
                        duration: 0.33,
                        data: Bytes::from(vec![0u8; 20]),
                        independent: true,
                    },
                    LivePart {
                        index: 1,
                        duration: 0.33,
                        data: Bytes::from(vec![0u8; 20]),
                        independent: false,
                    },
                ]
            } else {
                Vec::new()
            },
            independent: true,
        });
        segments.push_back(LiveSegment {
            sequence: 6,
            duration: 6.0,
            data: Bytes::from(vec![0u8; 100]),
            parts: Vec::new(),
            independent: true,
        });

        LiveStream {
            app: "live".to_string(),
            stream_name: "test".to_string(),
            started_at: chrono::Utc::now(),
            last_frame_at: chrono::Utc::now(),
            init_segment: Some(Bytes::from(vec![0u8; 50])),
            segments,
            media_sequence: 5,
            next_sequence: 7,
            current_parts: Vec::new(),
            current_part_sequence: 7,
            current_segment_duration: 0.0,
            segment_duration: 6.0,
            part_duration: 0.33,
            ll_hls_enabled: ll_hls,
            max_segments: 10,
            ended: false,
            video_width: 1920,
            video_height: 1080,
            audio_sample_rate: 48000,
            audio_channels: 2,
            waiters: Vec::new(),
        }
    }

    #[test]
    fn test_standard_hls_manifest() {
        let stream = make_test_stream(false);
        let manifest = generate_live_manifest(&stream, "/live/live/test");

        assert!(manifest.contains("#EXTM3U"));
        assert!(manifest.contains("#EXT-X-VERSION:7"));
        assert!(manifest.contains("#EXT-X-MEDIA-SEQUENCE:5"));
        assert!(manifest.contains("#EXTINF:6.000000,"));
        assert!(manifest.contains("/live/live/test/seg5.mp4"));
        assert!(manifest.contains("/live/live/test/seg6.mp4"));
        assert!(!manifest.contains("#EXT-X-ENDLIST"));
        assert!(!manifest.contains("#EXT-X-PART"));
    }

    #[test]
    fn test_ll_hls_manifest() {
        let stream = make_test_stream(true);
        let manifest = generate_live_manifest(&stream, "/live/live/test");

        assert!(manifest.contains("#EXT-X-VERSION:9"));
        assert!(manifest.contains("#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES"));
        assert!(manifest.contains("#EXT-X-PART-INF:PART-TARGET=0.330000"));
        assert!(manifest.contains("#EXT-X-PART:DURATION=0.330000"));
        assert!(manifest.contains("INDEPENDENT=YES"));
    }

    #[test]
    fn test_ended_stream_has_endlist() {
        let mut stream = make_test_stream(false);
        stream.ended = true;
        let manifest = generate_live_manifest(&stream, "/live/live/test");
        assert!(manifest.contains("#EXT-X-ENDLIST"));
    }

    #[test]
    fn test_preload_hint() {
        let mut stream = make_test_stream(true);
        stream.current_parts.push(LivePart {
            index: 0,
            duration: 0.33,
            data: Bytes::from(vec![0u8; 20]),
            independent: true,
        });
        let manifest = generate_live_manifest(&stream, "/live/live/test");
        assert!(manifest.contains("#EXT-X-PRELOAD-HINT:TYPE=PART"));
        assert!(manifest.contains("seg7_part1.mp4"));
    }
}
