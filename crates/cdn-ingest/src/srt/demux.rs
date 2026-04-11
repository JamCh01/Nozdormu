use crate::auth;
use crate::config::IngestConfig;
use crate::error::IngestError;
use crate::segmenter::{FrameData, LiveSegmenter, TrackKind};
use crate::store::LiveStreamStore;
use std::sync::Arc;

use srt_tokio::ConnectionRequest;

/// Handle a single SRT connection.
pub async fn handle_srt_connection(
    request: ConnectionRequest,
    store: Arc<LiveStreamStore>,
    config: IngestConfig,
) -> Result<(), IngestError> {
    // Extract stream ID for authentication
    let stream_id = request
        .stream_id()
        .map(|s| s.to_string())
        .unwrap_or_default();

    log::info!("[SRT] incoming connection, streamid={}", stream_id);

    // Parse app/stream_key from streamid
    let (app, stream_key) = parse_stream_id(&stream_id)?;

    // Authenticate
    let (resolved_app, resolved_name) =
        auth::validate_stream_key(&config.stream_keys, &app, &stream_key)?;

    // Accept the connection
    let mut socket = request
        .accept(None)
        .await
        .map_err(|e| IngestError::Protocol(format!("SRT accept: {}", e)))?;

    log::info!("[SRT] publishing: {}/{}", resolved_app, resolved_name);

    // Create live stream in store
    let live_stream = store.create_stream(
        &resolved_app,
        &resolved_name,
        config.segment_duration,
        config.ll_hls.part_duration,
        config.ll_hls.enabled,
    )?;

    let mut segmenter = LiveSegmenter::new(
        config.segment_duration,
        config.ll_hls.part_duration,
        config.ll_hls.enabled,
    );

    let stream_label = format!("{}/{}", resolved_app, resolved_name);

    // Read SRT packets (MPEG-TS data)
    use futures_util::StreamExt;
    let mut ts_demuxer = TsDemuxer::new();

    while let Some(result) = socket.next().await {
        match result {
            Ok((_instant, data)) => {
                crate::metrics::INGEST_BYTES_RECEIVED
                    .with_label_values(&["srt"])
                    .inc_by(data.len() as u64);

                // Demux TS packets to extract frames
                let frames = ts_demuxer.push_data(&data);

                for frame in frames {
                    match frame.track {
                        TrackKind::Video => {
                            crate::metrics::INGEST_FRAMES_TOTAL
                                .with_label_values(&[stream_label.as_str(), "video"])
                                .inc();
                        }
                        TrackKind::Audio => {
                            crate::metrics::INGEST_FRAMES_TOTAL
                                .with_label_values(&[stream_label.as_str(), "audio"])
                                .inc();
                        }
                    }

                    let output = segmenter.push_frame(frame);
                    crate::rtmp::session::apply_segmenter_output(
                        output,
                        &live_stream,
                        &stream_label,
                    )
                    .await;
                }
            }
            Err(e) => {
                log::warn!("[SRT] receive error: {}", e);
                break;
            }
        }
    }

    log::info!("[SRT] stream ended: {}/{}", resolved_app, resolved_name);
    store.end_stream(&resolved_app, &resolved_name);

    Ok(())
}

/// Parse the SRT streamid to extract app and stream key.
fn parse_stream_id(stream_id: &str) -> Result<(String, String), IngestError> {
    // Try format: "#!::r={app}/{stream_key}"
    if let Some(rest) = stream_id.strip_prefix("#!::r=") {
        if let Some((app, key)) = rest.split_once('/') {
            return Ok((app.to_string(), key.to_string()));
        }
    }

    // Try format: "{app}/{stream_key}"
    if let Some((app, key)) = stream_id.split_once('/') {
        return Ok((app.to_string(), key.to_string()));
    }

    Err(IngestError::Protocol(format!(
        "invalid SRT streamid format: '{}', expected 'app/stream_key'",
        stream_id
    )))
}

/// Simplified MPEG-TS demuxer that extracts H.264 and AAC frames.
pub struct TsDemuxer {
    video_pid: Option<u16>,
    audio_pid: Option<u16>,
    pmt_pid: Option<u16>,
    video_pes_buffer: Vec<u8>,
    audio_pes_buffer: Vec<u8>,
    video_dts: u64,
    audio_dts: u64,
}

impl Default for TsDemuxer {
    fn default() -> Self {
        Self::new()
    }
}

impl TsDemuxer {
    pub fn new() -> Self {
        Self {
            video_pid: None,
            audio_pid: None,
            pmt_pid: None,
            video_pes_buffer: Vec::new(),
            audio_pes_buffer: Vec::new(),
            video_dts: 0,
            audio_dts: 0,
        }
    }

    /// Push TS data and return extracted frames.
    pub fn push_data(&mut self, data: &[u8]) -> Vec<FrameData> {
        let mut frames = Vec::new();

        let mut offset = 0;
        while offset + 188 <= data.len() {
            if data[offset] != 0x47 {
                offset += 1;
                continue;
            }

            let packet = &data[offset..offset + 188];
            offset += 188;

            let pid = ((packet[1] as u16 & 0x1F) << 8) | packet[2] as u16;
            let payload_start = (packet[1] & 0x40) != 0;
            let has_adaptation = (packet[3] & 0x20) != 0;
            let has_payload = (packet[3] & 0x10) != 0;

            if !has_payload {
                continue;
            }

            let mut payload_offset = 4;
            if has_adaptation {
                let adaptation_len = packet[4] as usize;
                payload_offset = 5 + adaptation_len;
            }

            if payload_offset >= 188 {
                continue;
            }

            let payload = &packet[payload_offset..188];

            if pid == 0 && payload_start {
                self.parse_pat(payload);
                continue;
            }

            if Some(pid) == self.pmt_pid && payload_start {
                self.parse_pmt(payload);
                continue;
            }

            if Some(pid) == self.video_pid {
                if payload_start {
                    if !self.video_pes_buffer.is_empty() {
                        if let Some(frame) = self.extract_video_frame() {
                            frames.push(frame);
                        }
                    }
                    self.video_pes_buffer.clear();
                }
                self.video_pes_buffer.extend_from_slice(payload);
            }

            if Some(pid) == self.audio_pid {
                if payload_start {
                    if !self.audio_pes_buffer.is_empty() {
                        if let Some(frame) = self.extract_audio_frame() {
                            frames.push(frame);
                        }
                    }
                    self.audio_pes_buffer.clear();
                }
                self.audio_pes_buffer.extend_from_slice(payload);
            }
        }

        frames
    }

    fn parse_pat(&mut self, payload: &[u8]) {
        if payload.is_empty() {
            return;
        }
        let pointer = payload[0] as usize;
        if 1 + pointer >= payload.len() {
            return;
        }
        let data = &payload[1 + pointer..];

        if data.len() < 8 || data[0] != 0x00 {
            return;
        }

        let section_length = ((data[1] as usize & 0x0F) << 8) | data[2] as usize;
        let entries_start = 8;
        let entries_end = (3 + section_length).min(data.len()).saturating_sub(4);

        let mut i = entries_start;
        while i + 4 <= entries_end {
            let program_num = ((data[i] as u16) << 8) | data[i + 1] as u16;
            let pid = ((data[i + 2] as u16 & 0x1F) << 8) | data[i + 3] as u16;
            if program_num != 0 {
                self.pmt_pid = Some(pid);
            }
            i += 4;
        }
    }

    fn parse_pmt(&mut self, payload: &[u8]) {
        if payload.is_empty() {
            return;
        }
        let pointer = payload[0] as usize;
        if 1 + pointer >= payload.len() {
            return;
        }
        let data = &payload[1 + pointer..];

        if data.len() < 12 {
            return;
        }

        let section_length = ((data[1] as usize & 0x0F) << 8) | data[2] as usize;
        let program_info_length = ((data[10] as usize & 0x0F) << 8) | data[11] as usize;

        let mut i = 12 + program_info_length;
        let end = (3 + section_length).min(data.len()).saturating_sub(4);

        while i + 5 <= end {
            let stream_type = data[i];
            let pid = ((data[i + 1] as u16 & 0x1F) << 8) | data[i + 2] as u16;
            let es_info_length = ((data[i + 3] as usize & 0x0F) << 8) | data[i + 4] as usize;

            match stream_type {
                0x1B => {
                    self.video_pid = Some(pid);
                }
                0x0F => {
                    self.audio_pid = Some(pid);
                }
                _ => {}
            }

            i += 5 + es_info_length;
        }
    }

    fn extract_video_frame(&mut self) -> Option<FrameData> {
        let pes = &self.video_pes_buffer;
        if pes.len() < 9 || pes[0] != 0 || pes[1] != 0 || pes[2] != 1 {
            return None;
        }

        let pts_dts_flags = (pes[7] >> 6) & 0x03;
        let pes_header_data_length = pes[8] as usize;
        let data_start = 9 + pes_header_data_length;

        if data_start >= pes.len() {
            return None;
        }

        let (pts, dts) = if pts_dts_flags >= 2 && pes.len() >= 14 {
            let pts = parse_timestamp(&pes[9..14]);
            let dts = if pts_dts_flags == 3 && pes.len() >= 19 {
                parse_timestamp(&pes[14..19])
            } else {
                pts
            };
            (pts, dts)
        } else {
            (self.video_dts, self.video_dts)
        };

        self.video_dts = dts;
        let nalu_data = &pes[data_start..];
        let is_keyframe = contains_idr_nalu(nalu_data);

        Some(FrameData {
            track: TrackKind::Video,
            data: nalu_data.to_vec(),
            dts,
            pts,
            is_keyframe,
        })
    }

    fn extract_audio_frame(&mut self) -> Option<FrameData> {
        let pes = &self.audio_pes_buffer;
        if pes.len() < 9 || pes[0] != 0 || pes[1] != 0 || pes[2] != 1 {
            return None;
        }

        let pts_dts_flags = (pes[7] >> 6) & 0x03;
        let pes_header_data_length = pes[8] as usize;
        let data_start = 9 + pes_header_data_length;

        if data_start >= pes.len() {
            return None;
        }

        let dts = if pts_dts_flags >= 2 && pes.len() >= 14 {
            parse_timestamp(&pes[9..14])
        } else {
            self.audio_dts
        };

        self.audio_dts = dts;

        Some(FrameData {
            track: TrackKind::Audio,
            data: pes[data_start..].to_vec(),
            dts,
            pts: dts,
            is_keyframe: true,
        })
    }
}

fn parse_timestamp(data: &[u8]) -> u64 {
    let b0 = data[0] as u64;
    let b1 = data[1] as u64;
    let b2 = data[2] as u64;
    let b3 = data[3] as u64;
    let b4 = data[4] as u64;

    ((b0 >> 1) & 0x07) << 30 | (b1 << 22) | ((b2 >> 1) << 15) | (b3 << 7) | (b4 >> 1)
}

fn contains_idr_nalu(data: &[u8]) -> bool {
    let mut i = 0;
    while i < data.len() {
        if i + 3 < data.len() && data[i] == 0 && data[i + 1] == 0 {
            let nal_start = if data[i + 2] == 1 {
                i + 3
            } else if i + 4 < data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                i + 4
            } else {
                i += 1;
                continue;
            };

            if nal_start < data.len() {
                let nal_type = data[nal_start] & 0x1F;
                if nal_type == 5 {
                    return true;
                }
            }
            i = nal_start;
        } else {
            i += 1;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_stream_id_simple() {
        let (app, key) = parse_stream_id("live/secret123").unwrap();
        assert_eq!(app, "live");
        assert_eq!(key, "secret123");
    }

    #[test]
    fn test_parse_stream_id_srt_format() {
        let (app, key) = parse_stream_id("#!::r=live/secret123").unwrap();
        assert_eq!(app, "live");
        assert_eq!(key, "secret123");
    }

    #[test]
    fn test_parse_stream_id_invalid() {
        let result = parse_stream_id("noseparator");
        assert!(result.is_err());
    }

    #[test]
    fn test_contains_idr_nalu() {
        let data = [0x00, 0x00, 0x00, 0x01, 0x65, 0x88];
        assert!(contains_idr_nalu(&data));

        let data = [0x00, 0x00, 0x00, 0x01, 0x41, 0x88];
        assert!(!contains_idr_nalu(&data));
    }
}
