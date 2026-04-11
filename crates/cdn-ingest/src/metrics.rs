use once_cell::sync::Lazy;
use prometheus::{
    register_histogram_vec, register_int_counter_vec, register_int_gauge, HistogramVec,
    IntCounterVec, IntGauge,
};

/// Total ingest connections by protocol (rtmp, srt).
pub static INGEST_CONNECTIONS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "cdn_ingest_connections_total",
        "Total ingest connections",
        &["protocol"]
    )
    .unwrap()
});

/// Total frames received by stream and track type (video, audio).
pub static INGEST_FRAMES_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "cdn_ingest_frames_total",
        "Total frames received",
        &["stream", "track"]
    )
    .unwrap()
});

/// Total segments produced by stream.
pub static INGEST_SEGMENTS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "cdn_ingest_segments_total",
        "Total segments produced",
        &["stream"]
    )
    .unwrap()
});

/// Auth results (accepted, rejected).
pub static INGEST_AUTH_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "cdn_ingest_auth_total",
        "Stream key auth results",
        &["result"]
    )
    .unwrap()
});

/// Total bytes received by protocol.
pub static INGEST_BYTES_RECEIVED: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "cdn_ingest_bytes_received_total",
        "Total bytes received from encoders",
        &["protocol"]
    )
    .unwrap()
});

/// Number of currently active live streams.
pub static INGEST_ACTIVE_STREAMS: Lazy<IntGauge> = Lazy::new(|| {
    register_int_gauge!("cdn_ingest_active_streams", "Number of active live streams").unwrap()
});

/// Number of currently active ingest connections by protocol.
pub static INGEST_ACTIVE_CONNECTIONS: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "cdn_ingest_active_connections",
        "Active ingest connections",
        &["protocol"]
    )
    .unwrap()
});

/// Histogram of segment durations.
pub static INGEST_SEGMENT_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "cdn_ingest_segment_duration_seconds",
        "Duration of produced segments",
        &["stream"],
        vec![0.5, 1.0, 2.0, 4.0, 6.0, 8.0, 10.0]
    )
    .unwrap()
});
