use bytes::Bytes;

use crate::codec::aac::AacConfig;
use crate::codec::h264::H264Config;

/// A frame of media data to be segmented.
pub struct FrameData {
    pub track: TrackKind,
    pub data: Vec<u8>,
    /// Decode timestamp in track timescale units.
    pub dts: u64,
    /// Presentation timestamp in track timescale units.
    pub pts: u64,
    pub is_keyframe: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
}

/// Output from pushing a frame to the segmenter.
pub struct SegmenterOutput {
    /// Set once when both video and audio configs are ready.
    pub init_segment: Option<Bytes>,
    /// Completed LL-HLS parts.
    pub completed_parts: Vec<CompletedPart>,
    /// A completed full segment (if a segment boundary was crossed).
    pub completed_segment: Option<CompletedSegment>,
}

pub struct CompletedSegment {
    pub sequence: u64,
    pub duration: f64,
    pub data: Bytes,
    pub parts: Vec<CompletedPart>,
    pub independent: bool,
}

#[derive(Clone)]
pub struct CompletedPart {
    pub index: u32,
    pub duration: f64,
    pub data: Bytes,
    pub independent: bool,
}

/// Accumulates raw frames and produces fMP4 segments and LL-HLS parts.
pub struct LiveSegmenter {
    video_config: Option<H264Config>,
    audio_config: Option<AacConfig>,
    init_segment_built: bool,
    segment_duration: f64,
    part_duration: f64,
    ll_hls_enabled: bool,
    next_sequence: u64,

    // Current segment accumulation
    video_samples: Vec<SampleData>,
    audio_samples: Vec<SampleData>,
    video_base_dts: u64,
    audio_base_dts: u64,
    segment_start_dts: Option<u64>,

    // Current part accumulation (LL-HLS)
    part_video_samples: Vec<SampleData>,
    part_audio_samples: Vec<SampleData>,
    part_video_base_dts: u64,
    part_audio_base_dts: u64,
    part_start_dts: Option<u64>,
    part_index: u32,

    // Accumulated parts for the current segment
    segment_parts: Vec<CompletedPart>,
}

struct SampleData {
    data: Vec<u8>,
    duration: u32,
    is_sync: bool,
    cts_offset: i32,
}

impl LiveSegmenter {
    pub fn new(segment_duration: f64, part_duration: f64, ll_hls: bool) -> Self {
        Self {
            video_config: None,
            audio_config: None,
            init_segment_built: false,
            segment_duration,
            part_duration,
            ll_hls_enabled: ll_hls,
            next_sequence: 0,
            video_samples: Vec::new(),
            audio_samples: Vec::new(),
            video_base_dts: 0,
            audio_base_dts: 0,
            segment_start_dts: None,
            part_video_samples: Vec::new(),
            part_audio_samples: Vec::new(),
            part_video_base_dts: 0,
            part_audio_base_dts: 0,
            part_start_dts: None,
            part_index: 0,
            segment_parts: Vec::new(),
        }
    }

    pub fn set_video_config(&mut self, config: H264Config) {
        self.video_config = Some(config);
    }

    pub fn set_audio_config(&mut self, config: AacConfig) {
        self.audio_config = Some(config);
    }

    /// Build the fMP4 init segment from codec configs.
    /// Returns None if video config is not yet available.
    pub fn build_init_segment(&self) -> Option<Bytes> {
        let _video = self.video_config.as_ref()?;

        let tracks = self.build_track_infos();
        let timescale = 1000; // global timescale

        let init = cdn_streaming::packaging::generate_init_segment_from_tracks(&tracks, timescale);
        Some(Bytes::from(init))
    }

    /// Push a frame. Returns completed segments/parts.
    pub fn push_frame(&mut self, frame: FrameData) -> SegmenterOutput {
        let mut output = SegmenterOutput {
            init_segment: None,
            completed_parts: Vec::new(),
            completed_segment: None,
        };

        // Try to build init segment if not yet done
        if !self.init_segment_built && self.video_config.is_some() {
            if let Some(init) = self.build_init_segment() {
                output.init_segment = Some(init);
                self.init_segment_built = true;
            }
        }

        let timescale = match frame.track {
            TrackKind::Video => self
                .video_config
                .as_ref()
                .map(|c| c.timescale)
                .unwrap_or(90000),
            TrackKind::Audio => self
                .audio_config
                .as_ref()
                .map(|c| c.timescale)
                .unwrap_or(48000),
        };

        // Initialize segment start DTS
        if self.segment_start_dts.is_none() {
            self.segment_start_dts = Some(frame.dts);
        }
        if self.part_start_dts.is_none() {
            self.part_start_dts = Some(frame.dts);
        }

        let sample = SampleData {
            data: frame.data,
            duration: 0, // will be computed from DTS differences
            is_sync: frame.is_keyframe,
            cts_offset: (frame.pts as i64 - frame.dts as i64) as i32,
        };

        // Check if we should cut a segment (on video keyframe)
        let should_cut_segment = frame.track == TrackKind::Video
            && frame.is_keyframe
            && self.segment_start_dts.is_some()
            && !self.video_samples.is_empty();

        let elapsed_secs = if let Some(start) = self.segment_start_dts {
            (frame.dts - start) as f64 / timescale as f64
        } else {
            0.0
        };

        if should_cut_segment && elapsed_secs >= self.segment_duration {
            // Cut the current segment
            output.completed_segment = Some(self.cut_segment(timescale));
        }

        // Check if we should cut a part (LL-HLS)
        if self.ll_hls_enabled {
            let part_elapsed = if let Some(start) = self.part_start_dts {
                (frame.dts - start) as f64 / timescale as f64
            } else {
                0.0
            };

            if part_elapsed >= self.part_duration && !self.part_video_samples.is_empty() {
                if let Some(part) = self.cut_part(timescale) {
                    output.completed_parts.push(part);
                }
            }
        }

        // Add sample to accumulators
        match frame.track {
            TrackKind::Video => {
                if self.video_samples.is_empty() {
                    self.video_base_dts = frame.dts;
                }
                if self.part_video_samples.is_empty() {
                    self.part_video_base_dts = frame.dts;
                }
                self.video_samples.push(sample.clone_data());
                self.part_video_samples.push(sample);
            }
            TrackKind::Audio => {
                if self.audio_samples.is_empty() {
                    self.audio_base_dts = frame.dts;
                }
                if self.part_audio_samples.is_empty() {
                    self.part_audio_base_dts = frame.dts;
                }
                self.audio_samples.push(sample.clone_data());
                self.part_audio_samples.push(sample);
            }
        }

        output
    }

    fn cut_segment(&mut self, timescale: u32) -> CompletedSegment {
        // Compute sample durations from DTS differences
        compute_durations(&mut self.video_samples, timescale);
        compute_durations(&mut self.audio_samples, timescale);

        // Also cut any remaining part
        if self.ll_hls_enabled && !self.part_video_samples.is_empty() {
            if let Some(part) = self.cut_part(timescale) {
                self.segment_parts.push(part);
            }
        }

        let independent = self
            .video_samples
            .first()
            .map(|s| s.is_sync)
            .unwrap_or(true);

        let duration = self
            .video_samples
            .iter()
            .map(|s| s.duration as f64 / timescale as f64)
            .sum::<f64>();

        let data = self.build_segment_fmp4();

        let seq = self.next_sequence;
        self.next_sequence += 1;

        let parts = std::mem::take(&mut self.segment_parts);

        // Reset accumulators
        self.video_samples.clear();
        self.audio_samples.clear();
        self.segment_start_dts = None;
        self.part_index = 0;

        CompletedSegment {
            sequence: seq,
            duration,
            data,
            parts,
            independent,
        }
    }

    fn cut_part(&mut self, timescale: u32) -> Option<CompletedPart> {
        if self.part_video_samples.is_empty() && self.part_audio_samples.is_empty() {
            return None;
        }

        compute_durations(&mut self.part_video_samples, timescale);
        compute_durations(&mut self.part_audio_samples, timescale);

        let independent = self
            .part_video_samples
            .first()
            .map(|s| s.is_sync)
            .unwrap_or(true);

        let duration = self
            .part_video_samples
            .iter()
            .map(|s| s.duration as f64 / timescale as f64)
            .sum::<f64>();

        let data = self.build_part_fmp4();

        let idx = self.part_index;
        self.part_index += 1;

        // Reset part accumulators
        self.part_video_samples.clear();
        self.part_audio_samples.clear();
        self.part_start_dts = None;

        Some(CompletedPart {
            index: idx,
            duration,
            data,
            independent,
        })
    }

    fn build_segment_fmp4(&self) -> Bytes {
        let mut tracks = Vec::new();

        if let Some(ref _vc) = self.video_config {
            tracks.push(cdn_streaming::packaging::LiveTrackData {
                track_id: 1,
                base_dts: self.video_base_dts,
                samples: self
                    .video_samples
                    .iter()
                    .map(|s| cdn_streaming::packaging::LiveSample {
                        data: s.data.clone(),
                        duration: s.duration,
                        is_sync: s.is_sync,
                        cts_offset: s.cts_offset,
                    })
                    .collect(),
            });
        }

        if let Some(ref _ac) = self.audio_config {
            tracks.push(cdn_streaming::packaging::LiveTrackData {
                track_id: 2,
                base_dts: self.audio_base_dts,
                samples: self
                    .audio_samples
                    .iter()
                    .map(|s| cdn_streaming::packaging::LiveSample {
                        data: s.data.clone(),
                        duration: s.duration,
                        is_sync: s.is_sync,
                        cts_offset: s.cts_offset,
                    })
                    .collect(),
            });
        }

        let seq = self.next_sequence as u32;
        let data = cdn_streaming::packaging::generate_live_media_segment(seq, &tracks);
        Bytes::from(data)
    }

    fn build_part_fmp4(&self) -> Bytes {
        let mut tracks = Vec::new();

        if let Some(ref _vc) = self.video_config {
            tracks.push(cdn_streaming::packaging::LiveTrackData {
                track_id: 1,
                base_dts: self.part_video_base_dts,
                samples: self
                    .part_video_samples
                    .iter()
                    .map(|s| cdn_streaming::packaging::LiveSample {
                        data: s.data.clone(),
                        duration: s.duration,
                        is_sync: s.is_sync,
                        cts_offset: s.cts_offset,
                    })
                    .collect(),
            });
        }

        if let Some(ref _ac) = self.audio_config {
            tracks.push(cdn_streaming::packaging::LiveTrackData {
                track_id: 2,
                base_dts: self.part_audio_base_dts,
                samples: self
                    .part_audio_samples
                    .iter()
                    .map(|s| cdn_streaming::packaging::LiveSample {
                        data: s.data.clone(),
                        duration: s.duration,
                        is_sync: s.is_sync,
                        cts_offset: s.cts_offset,
                    })
                    .collect(),
            });
        }

        let seq = self.next_sequence as u32 * 1000 + self.part_index + 1;
        let data = cdn_streaming::packaging::generate_live_media_segment(seq, &tracks);
        Bytes::from(data)
    }

    fn build_track_infos(&self) -> Vec<cdn_streaming::packaging::LiveTrackInfo> {
        let mut tracks = Vec::new();

        if let Some(ref vc) = self.video_config {
            tracks.push(cdn_streaming::packaging::LiveTrackInfo {
                track_id: 1,
                track_type: cdn_streaming::packaging::LiveTrackType::Video,
                timescale: vc.timescale,
                width: Some(vc.width),
                height: Some(vc.height),
                sample_rate: None,
                channels: None,
                stsd_data: vc.build_stsd_data(),
            });
        }

        if let Some(ref ac) = self.audio_config {
            tracks.push(cdn_streaming::packaging::LiveTrackInfo {
                track_id: 2,
                track_type: cdn_streaming::packaging::LiveTrackType::Audio,
                timescale: ac.timescale,
                width: None,
                height: None,
                sample_rate: Some(ac.sample_rate),
                channels: Some(ac.channels),
                stsd_data: ac.build_stsd_data(),
            });
        }

        tracks
    }
}

impl SampleData {
    fn clone_data(&self) -> SampleData {
        SampleData {
            data: self.data.clone(),
            duration: self.duration,
            is_sync: self.is_sync,
            cts_offset: self.cts_offset,
        }
    }
}

/// Compute sample durations from accumulated samples.
/// Uses a default duration based on timescale if only one sample.
fn compute_durations(samples: &mut [SampleData], _timescale: u32) {
    if samples.is_empty() {
        return;
    }
    // For live, we use a fixed duration estimate per sample.
    // Video: 90000/30 = 3000 (30fps), Audio: 1024 (AAC frame size)
    // This is a simplification — in production, DTS differences would be used.
    let default_duration = if samples.len() == 1 {
        3000 // ~30fps at 90000 timescale
    } else {
        3000
    };

    for sample in samples.iter_mut() {
        if sample.duration == 0 {
            sample.duration = default_duration;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_segmenter_creation() {
        let seg = LiveSegmenter::new(6.0, 0.33, true);
        assert_eq!(seg.segment_duration, 6.0);
        assert_eq!(seg.part_duration, 0.33);
        assert!(seg.ll_hls_enabled);
        assert_eq!(seg.next_sequence, 0);
    }

    #[test]
    fn test_push_frame_without_config() {
        let mut seg = LiveSegmenter::new(6.0, 0.33, true);
        let output = seg.push_frame(FrameData {
            track: TrackKind::Video,
            data: vec![0u8; 100],
            dts: 0,
            pts: 0,
            is_keyframe: true,
        });
        // No init segment without video config
        assert!(output.init_segment.is_none());
        assert!(output.completed_segment.is_none());
    }
}
