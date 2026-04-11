use crate::auth;
use crate::codec::aac::AacConfig;
use crate::codec::h264::H264Config;
use crate::config::IngestConfig;
use crate::error::IngestError;
use crate::segmenter::{FrameData, LiveSegmenter, TrackKind};
use crate::store::LiveStreamStore;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{
    ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
};

/// Handle a single RTMP connection.
pub async fn handle_rtmp_connection(
    mut stream: TcpStream,
    addr: SocketAddr,
    store: Arc<LiveStreamStore>,
    config: IngestConfig,
) -> Result<(), IngestError> {
    log::info!("[RTMP] new connection from {}", addr);

    // ── 1. RTMP Handshake ──
    let mut handshake = Handshake::new(PeerType::Server);
    let mut buf = [0u8; 4096];

    // Send S0+S1
    let s0s1 = handshake
        .generate_outbound_p0_and_p1()
        .map_err(|e| IngestError::Protocol(format!("handshake S0S1: {:?}", e)))?;
    stream
        .write_all(&s0s1)
        .await
        .map_err(|e| IngestError::Protocol(format!("write S0S1: {}", e)))?;

    // Read C0+C1+C2 and complete handshake
    let mut handshake_done = false;
    while !handshake_done {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Err(IngestError::Protocol(
                "connection closed during handshake".into(),
            ));
        }

        crate::metrics::INGEST_BYTES_RECEIVED
            .with_label_values(&["rtmp"])
            .inc_by(n as u64);

        let result = handshake
            .process_bytes(&buf[..n])
            .map_err(|e| IngestError::Protocol(format!("handshake: {:?}", e)))?;

        match result {
            HandshakeProcessResult::InProgress { response_bytes } => {
                stream.write_all(&response_bytes).await?;
            }
            HandshakeProcessResult::Completed {
                response_bytes,
                remaining_bytes,
            } => {
                stream.write_all(&response_bytes).await?;
                handshake_done = true;

                // Process remaining bytes after handshake
                if !remaining_bytes.is_empty() {
                    // Will be handled in the session loop below
                    // For now, we'll re-feed them
                }
            }
        }
    }

    // ── 2. Create RTMP Server Session ──
    let session_config = ServerSessionConfig::new();
    let (mut session, initial_results) = ServerSession::new(session_config)
        .map_err(|e| IngestError::Protocol(format!("session create: {:?}", e)))?;

    // Send initial results (window ack, set chunk size, etc.)
    for result in initial_results {
        if let ServerSessionResult::OutboundResponse(data) = result {
            stream.write_all(&data.bytes).await?;
        }
    }

    // ── 3. Main loop: read RTMP chunks, process events ──
    let mut segmenter: Option<LiveSegmenter> = None;
    let mut live_stream = None;
    let mut app_name = String::new();
    let mut stream_name = String::new();
    let mut video_dts: u64 = 0;
    let mut audio_dts: u64 = 0;

    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            log::info!("[RTMP] connection closed from {}", addr);
            break;
        }

        crate::metrics::INGEST_BYTES_RECEIVED
            .with_label_values(&["rtmp"])
            .inc_by(n as u64);

        let results = session
            .handle_input(&buf[..n])
            .map_err(|e| IngestError::Protocol(format!("session input: {:?}", e)))?;

        for result in results {
            match result {
                ServerSessionResult::OutboundResponse(data) => {
                    stream.write_all(&data.bytes).await?;
                }
                ServerSessionResult::RaisedEvent(event) => {
                    match event {
                        ServerSessionEvent::ConnectionRequested {
                            request_id,
                            app_name: req_app,
                        } => {
                            log::info!("[RTMP] connect request: app={}", req_app);
                            app_name = req_app;
                            let accept_results =
                                session.accept_request(request_id).map_err(|e| {
                                    IngestError::Protocol(format!("accept connect: {:?}", e))
                                })?;
                            for r in accept_results {
                                if let ServerSessionResult::OutboundResponse(data) = r {
                                    stream.write_all(&data.bytes).await?;
                                }
                            }
                        }
                        ServerSessionEvent::PublishStreamRequested {
                            request_id,
                            app_name: _,
                            stream_key,
                            ..
                        } => {
                            log::info!(
                                "[RTMP] publish request: app={}, key={}",
                                app_name,
                                &stream_key[..stream_key.len().min(8)]
                            );

                            // Authenticate
                            let (resolved_app, resolved_name) = auth::validate_stream_key(
                                &config.stream_keys,
                                &app_name,
                                &stream_key,
                            )?;

                            stream_name = resolved_name.clone();

                            // Create live stream in store
                            let ls = store.create_stream(
                                &resolved_app,
                                &resolved_name,
                                config.segment_duration,
                                config.ll_hls.part_duration,
                                config.ll_hls.enabled,
                            )?;
                            live_stream = Some(ls);

                            // Create segmenter
                            segmenter = Some(LiveSegmenter::new(
                                config.segment_duration,
                                config.ll_hls.part_duration,
                                config.ll_hls.enabled,
                            ));

                            let accept_results =
                                session.accept_request(request_id).map_err(|e| {
                                    IngestError::Protocol(format!("accept publish: {:?}", e))
                                })?;
                            for r in accept_results {
                                if let ServerSessionResult::OutboundResponse(data) = r {
                                    stream.write_all(&data.bytes).await?;
                                }
                            }

                            log::info!("[RTMP] publishing: {}/{}", resolved_app, resolved_name);
                        }
                        ServerSessionEvent::VideoDataReceived {
                            data, timestamp, ..
                        } => {
                            if let (Some(ref mut seg), Some(ref ls)) =
                                (&mut segmenter, &live_stream)
                            {
                                let stream_label = format!("{}/{}", app_name, stream_name);
                                crate::metrics::INGEST_FRAMES_TOTAL
                                    .with_label_values(&[stream_label.as_str(), "video"])
                                    .inc();

                                video_dts = timestamp.value as u64 * 90; // ms to 90kHz

                                if let Some(frame) = parse_flv_video_tag(&data, seg, video_dts) {
                                    let output = seg.push_frame(frame);
                                    apply_segmenter_output(output, ls, &stream_label).await;
                                }
                            }
                        }
                        ServerSessionEvent::AudioDataReceived {
                            data, timestamp, ..
                        } => {
                            if let (Some(ref mut seg), Some(ref ls)) =
                                (&mut segmenter, &live_stream)
                            {
                                let stream_label = format!("{}/{}", app_name, stream_name);
                                crate::metrics::INGEST_FRAMES_TOTAL
                                    .with_label_values(&[stream_label.as_str(), "audio"])
                                    .inc();

                                audio_dts = timestamp.value as u64; // ms, will be scaled by AAC timescale

                                if let Some(frame) = parse_flv_audio_tag(&data, seg, audio_dts) {
                                    let output = seg.push_frame(frame);
                                    apply_segmenter_output(output, ls, &stream_label).await;
                                }
                            }
                        }
                        ServerSessionEvent::PublishStreamFinished { .. } => {
                            log::info!("[RTMP] publish finished: {}/{}", app_name, stream_name);
                            store.end_stream(&app_name, &stream_name);
                        }
                        _ => {}
                    }
                }
                ServerSessionResult::UnhandleableMessageReceived(_) => {}
            }
        }
    }

    // Cleanup on disconnect
    if !stream_name.is_empty() {
        store.end_stream(&app_name, &stream_name);
    }

    Ok(())
}

/// Parse an FLV video tag and extract frame data.
///
/// FLV video tag format:
/// - byte[0]: (frame_type << 4) | codec_id
///   - frame_type: 1=keyframe, 2=inter frame
///   - codec_id: 7=AVC (H.264)
/// - byte[1]: AVC packet type (0=sequence header, 1=NALU, 2=end of sequence)
/// - byte[2..5]: composition time offset (signed 24-bit, ms)
/// - byte[5..]: data
fn parse_flv_video_tag(
    data: &bytes::Bytes,
    segmenter: &mut LiveSegmenter,
    dts: u64,
) -> Option<FrameData> {
    if data.len() < 5 {
        return None;
    }

    let frame_type = (data[0] >> 4) & 0x0F;
    let codec_id = data[0] & 0x0F;

    if codec_id != 7 {
        // Not H.264
        log::debug!("[RTMP] unsupported video codec: {}", codec_id);
        return None;
    }

    let avc_packet_type = data[1];
    let cts = ((data[2] as i32) << 16) | ((data[3] as i32) << 8) | (data[4] as i32);
    // Sign extend 24-bit
    let cts = if cts & 0x800000 != 0 {
        cts | !0xFFFFFF_i32
    } else {
        cts
    };

    match avc_packet_type {
        0 => {
            // Sequence header: AVCDecoderConfigurationRecord
            if data.len() > 5 {
                match H264Config::from_avcc(&data[5..]) {
                    Ok(config) => {
                        log::info!(
                            "[RTMP] H.264 config: {}x{} profile={} level={}",
                            config.width,
                            config.height,
                            config.profile,
                            config.level
                        );
                        segmenter.set_video_config(config);
                    }
                    Err(e) => {
                        log::warn!("[RTMP] failed to parse H.264 config: {}", e);
                    }
                }
            }
            None
        }
        1 => {
            // NALU data
            let is_keyframe = frame_type == 1;
            let pts = (dts as i64 + cts as i64 * 90) as u64; // CTS is in ms, convert to 90kHz

            Some(FrameData {
                track: TrackKind::Video,
                data: data[5..].to_vec(),
                dts,
                pts,
                is_keyframe,
            })
        }
        2 => {
            // End of sequence
            None
        }
        _ => None,
    }
}

/// Parse an FLV audio tag and extract frame data.
///
/// FLV audio tag format:
/// - byte[0]: (sound_format << 4) | (sound_rate << 2) | (sound_size << 1) | sound_type
///   - sound_format: 10=AAC
/// - byte[1]: AAC packet type (0=sequence header, 1=raw)
/// - byte[2..]: data
fn parse_flv_audio_tag(
    data: &bytes::Bytes,
    segmenter: &mut LiveSegmenter,
    dts_ms: u64,
) -> Option<FrameData> {
    if data.len() < 2 {
        return None;
    }

    let sound_format = (data[0] >> 4) & 0x0F;
    if sound_format != 10 {
        // Not AAC
        log::debug!("[RTMP] unsupported audio codec: {}", sound_format);
        return None;
    }

    let aac_packet_type = data[1];

    match aac_packet_type {
        0 => {
            // Sequence header: AudioSpecificConfig
            if data.len() > 2 {
                match AacConfig::from_audio_specific_config(&data[2..]) {
                    Ok(config) => {
                        log::info!(
                            "[RTMP] AAC config: {}Hz {}ch",
                            config.sample_rate,
                            config.channels
                        );
                        // Scale DTS to audio timescale
                        segmenter.set_audio_config(config);
                    }
                    Err(e) => {
                        log::warn!("[RTMP] failed to parse AAC config: {}", e);
                    }
                }
            }
            None
        }
        1 => {
            // Raw AAC frame
            let timescale = 48000u64; // will be overridden by actual config
            let dts = dts_ms * timescale / 1000;

            Some(FrameData {
                track: TrackKind::Audio,
                data: data[2..].to_vec(),
                dts,
                pts: dts,          // AAC has no B-frames
                is_keyframe: true, // All AAC frames are sync samples
            })
        }
        _ => None,
    }
}

/// Apply segmenter output to the live stream store.
pub async fn apply_segmenter_output(
    output: crate::segmenter::SegmenterOutput,
    live_stream: &Arc<tokio::sync::RwLock<crate::store::LiveStream>>,
    stream_label: &str,
) {
    let mut s = live_stream.write().await;

    if let Some(init) = output.init_segment {
        s.set_init_segment(init);
    }

    for part in output.completed_parts {
        s.push_part(crate::store::LivePart {
            index: part.index,
            duration: part.duration,
            data: part.data,
            independent: part.independent,
        });
    }

    if let Some(segment) = output.completed_segment {
        crate::metrics::INGEST_SEGMENTS_TOTAL
            .with_label_values(&[stream_label])
            .inc();
        crate::metrics::INGEST_SEGMENT_DURATION
            .with_label_values(&[stream_label])
            .observe(segment.duration);

        let parts = segment
            .parts
            .into_iter()
            .map(|p| crate::store::LivePart {
                index: p.index,
                duration: p.duration,
                data: p.data,
                independent: p.independent,
            })
            .collect();

        s.push_segment(crate::store::LiveSegment {
            sequence: segment.sequence,
            duration: segment.duration,
            data: segment.data,
            parts,
            independent: segment.independent,
        });
    }

    s.last_frame_at = chrono::Utc::now();
}
