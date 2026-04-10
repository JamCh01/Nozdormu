//! HLS M3U8 media playlist generator.
//!
//! Generates VOD playlists with fMP4 segments (EXT-X-MAP for init segment).

use super::mp4_parse::{self, Mp4Metadata, TrackType};

/// Compute the number of segments for the given metadata and target duration.
pub fn compute_segment_count(metadata: &Mp4Metadata, segment_duration: f64) -> u32 {
    if segment_duration <= 0.0 || metadata.duration_secs <= 0.0 {
        return 0;
    }
    (metadata.duration_secs / segment_duration).ceil() as u32
}

/// Compute actual segment durations aligned to keyframes.
///
/// Returns a Vec of (segment_index, actual_duration_secs) pairs.
pub fn compute_segment_durations(metadata: &Mp4Metadata, segment_duration: f64) -> Vec<(u32, f64)> {
    let segment_count = compute_segment_count(metadata, segment_duration);
    if segment_count == 0 {
        return Vec::new();
    }

    // Use the first video track for keyframe alignment, or first track
    let track = metadata
        .tracks
        .iter()
        .find(|t| t.track_type == TrackType::Video)
        .or_else(|| metadata.tracks.first());

    let track = match track {
        Some(t) => t,
        None => return Vec::new(),
    };

    let total_samples = mp4_parse::total_samples(&track.sample_table.stts);
    let mut segments = Vec::new();
    let mut prev_sample: u32 = 0;

    for seg_idx in 0..segment_count {
        let target_end_time = (seg_idx + 1) as f64 * segment_duration * track.timescale as f64;
        let mut end_sample =
            mp4_parse::dts_to_sample(&track.sample_table.stts, target_end_time as u64);

        if end_sample >= total_samples {
            end_sample = total_samples;
        }

        // Align to keyframe for video
        if track.track_type == TrackType::Video && end_sample < total_samples {
            if let Some(ref stss) = track.sample_table.stss {
                let target = end_sample + 1; // 1-based
                match stss.binary_search(&target) {
                    Ok(_) => {}
                    Err(pos) => {
                        if pos < stss.len() {
                            end_sample = stss[pos] - 1;
                        } else {
                            end_sample = total_samples;
                        }
                    }
                }
            }
        }

        if end_sample <= prev_sample && seg_idx < segment_count - 1 {
            continue; // Skip empty segments
        }

        let start_dts = mp4_parse::sample_to_dts(&track.sample_table.stts, prev_sample);
        let end_dts = if end_sample >= total_samples {
            track.duration
        } else {
            mp4_parse::sample_to_dts(&track.sample_table.stts, end_sample)
        };

        let duration_secs = if track.timescale > 0 {
            (end_dts - start_dts) as f64 / track.timescale as f64
        } else {
            segment_duration
        };

        segments.push((seg_idx, duration_secs));
        prev_sample = end_sample;
    }

    segments
}

/// Generate an HLS media playlist (M3U8) for the given MP4 metadata.
///
/// - `base_url`: the original resource URL (e.g., `/video.mp4`)
/// - `query_base`: additional query params to append (e.g., `&quality=high`)
pub fn generate_media_playlist(
    metadata: &Mp4Metadata,
    segment_duration: f64,
    base_url: &str,
    query_base: &str,
) -> String {
    let segments = compute_segment_durations(metadata, segment_duration);
    if segments.is_empty() {
        return String::from("#EXTM3U\n#EXT-X-ENDLIST\n");
    }

    let max_duration = segments
        .iter()
        .map(|(_, d)| *d)
        .fold(0.0f64, f64::max)
        .ceil() as u32;

    let mut playlist = String::new();
    playlist.push_str("#EXTM3U\n");
    playlist.push_str("#EXT-X-VERSION:7\n");
    playlist.push_str(&format!("#EXT-X-TARGETDURATION:{}\n", max_duration));
    playlist.push_str("#EXT-X-MEDIA-SEQUENCE:0\n");
    playlist.push_str("#EXT-X-PLAYLIST-TYPE:VOD\n");

    // Init segment (EXT-X-MAP)
    playlist.push_str(&format!(
        "#EXT-X-MAP:URI=\"{}?format=hls&segment=init{}\"\n",
        base_url, query_base
    ));

    // Media segments
    for (idx, duration) in &segments {
        playlist.push_str(&format!("#EXTINF:{:.6},\n", duration));
        playlist.push_str(&format!(
            "{}?format=hls&segment={}{}\n",
            base_url, idx, query_base
        ));
    }

    playlist.push_str("#EXT-X-ENDLIST\n");
    playlist
}

#[cfg(test)]
mod tests {
    use super::super::mp4_parse::{
        Mp4Metadata, SampleTable, StscEntry, SttsEntry, TrackInfo, TrackType,
    };
    use super::*;

    fn make_test_metadata(duration_secs: f64, num_samples: u32) -> Mp4Metadata {
        let timescale = 90000u32;
        let sample_delta = (timescale as f64 / 30.0) as u32; // 30fps
        let duration = num_samples as u64 * sample_delta as u64;

        // Keyframes every 30 samples (1 second at 30fps)
        let stss: Vec<u32> = (0..num_samples)
            .filter(|i| i % 30 == 0)
            .map(|i| i + 1) // 1-based
            .collect();

        Mp4Metadata {
            duration_secs,
            timescale: 1000,
            tracks: vec![TrackInfo {
                track_id: 1,
                track_type: TrackType::Video,
                codec: "avc1".into(),
                timescale,
                duration,
                sample_table: SampleTable {
                    stts: vec![SttsEntry {
                        sample_count: num_samples,
                        sample_delta,
                    }],
                    stsc: vec![StscEntry {
                        first_chunk: 1,
                        samples_per_chunk: 30,
                        sample_description_index: 1,
                    }],
                    stsz: vec![1000; num_samples as usize],
                    stco: (0..(num_samples / 30))
                        .map(|i| (i * 30000 + 1000) as u64)
                        .collect(),
                    stss: Some(stss),
                    ctts: None,
                },
                width: Some(1920),
                height: Some(1080),
                sample_rate: None,
                channels: None,
                stsd_data: vec![],
            }],
        }
    }

    #[test]
    fn test_compute_segment_count() {
        let meta = make_test_metadata(10.0, 300);
        assert_eq!(compute_segment_count(&meta, 6.0), 2);
        assert_eq!(compute_segment_count(&meta, 5.0), 2);
        assert_eq!(compute_segment_count(&meta, 10.0), 1);
        assert_eq!(compute_segment_count(&meta, 3.0), 4);
    }

    #[test]
    fn test_compute_segment_count_zero() {
        let meta = make_test_metadata(0.0, 0);
        assert_eq!(compute_segment_count(&meta, 6.0), 0);
    }

    #[test]
    fn test_generate_media_playlist() {
        let meta = make_test_metadata(12.0, 360);
        let playlist = generate_media_playlist(&meta, 6.0, "/video.mp4", "");

        assert!(playlist.starts_with("#EXTM3U\n"));
        assert!(playlist.contains("#EXT-X-VERSION:7"));
        assert!(playlist.contains("#EXT-X-PLAYLIST-TYPE:VOD"));
        assert!(playlist.contains("#EXT-X-MAP:URI=\"/video.mp4?format=hls&segment=init\""));
        assert!(playlist.contains("#EXTINF:"));
        assert!(playlist.contains("/video.mp4?format=hls&segment=0"));
        assert!(playlist.ends_with("#EXT-X-ENDLIST\n"));
    }

    #[test]
    fn test_generate_media_playlist_with_query() {
        let meta = make_test_metadata(6.0, 180);
        let playlist = generate_media_playlist(&meta, 6.0, "/video.mp4", "&quality=high");

        assert!(playlist.contains("segment=init&quality=high"));
        assert!(playlist.contains("segment=0&quality=high"));
    }

    #[test]
    fn test_empty_metadata() {
        let meta = Mp4Metadata {
            duration_secs: 0.0,
            timescale: 1000,
            tracks: vec![],
        };
        let playlist = generate_media_playlist(&meta, 6.0, "/v.mp4", "");
        assert_eq!(playlist, "#EXTM3U\n#EXT-X-ENDLIST\n");
    }

    #[test]
    fn test_short_video() {
        let meta = make_test_metadata(2.0, 60);
        let playlist = generate_media_playlist(&meta, 6.0, "/short.mp4", "");
        // Should have 1 segment
        assert!(playlist.contains("segment=0"));
        assert!(!playlist.contains("segment=1"));
    }

    #[test]
    fn test_target_duration_ceiling() {
        let meta = make_test_metadata(10.0, 300);
        let playlist = generate_media_playlist(&meta, 6.0, "/v.mp4", "");
        // TARGETDURATION should be the ceiling of the max segment duration
        assert!(playlist.contains("#EXT-X-TARGETDURATION:"));
    }
}
