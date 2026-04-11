use serde::{Deserialize, Serialize};

/// Live ingest configuration (node-level, in default.yaml under `cdn.ingest`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub rtmp: RtmpListenConfig,

    #[serde(default)]
    pub srt: SrtListenConfig,

    /// Target HLS segment duration in seconds.
    #[serde(default = "default_segment_duration")]
    pub segment_duration: f64,

    /// LL-HLS configuration for live streams.
    #[serde(default)]
    pub ll_hls: LlHlsLiveConfig,

    /// Maximum number of segments to keep per stream (ring buffer size).
    #[serde(default = "default_max_segments")]
    pub max_segments: usize,

    /// Maximum number of concurrent live streams.
    #[serde(default = "default_max_streams")]
    pub max_streams: usize,

    /// Authorized stream keys for push authentication.
    #[serde(default)]
    pub stream_keys: Vec<StreamKeyEntry>,

    /// Seconds without frames before a stream is considered stale and removed.
    #[serde(default = "default_stream_timeout")]
    pub stream_timeout_secs: u64,
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rtmp: RtmpListenConfig::default(),
            srt: SrtListenConfig::default(),
            segment_duration: default_segment_duration(),
            ll_hls: LlHlsLiveConfig::default(),
            max_segments: default_max_segments(),
            max_streams: default_max_streams(),
            stream_keys: Vec::new(),
            stream_timeout_secs: default_stream_timeout(),
        }
    }
}

/// RTMP listener configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RtmpListenConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Listen address for RTMP (default "0.0.0.0:1935").
    #[serde(default = "default_rtmp_listen")]
    pub listen: String,
}

impl Default for RtmpListenConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: default_rtmp_listen(),
        }
    }
}

/// SRT listener configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SrtListenConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Listen address for SRT (default "0.0.0.0:9000").
    #[serde(default = "default_srt_listen")]
    pub listen: String,

    /// SRT latency in milliseconds (default 120).
    #[serde(default = "default_srt_latency")]
    pub latency_ms: u32,
}

impl Default for SrtListenConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: default_srt_listen(),
            latency_ms: default_srt_latency(),
        }
    }
}

/// LL-HLS configuration for live streams.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlHlsLiveConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Target partial segment duration in seconds (default 0.33).
    #[serde(default = "default_part_duration")]
    pub part_duration: f64,
}

impl Default for LlHlsLiveConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            part_duration: default_part_duration(),
        }
    }
}

/// A stream key entry for push authentication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamKeyEntry {
    /// The secret key that the encoder must provide.
    pub key: String,

    /// Application name (e.g., "live").
    pub app: String,

    /// Stream name that this key maps to (e.g., "my_stream").
    pub stream_name: String,

    /// Whether this key is currently active.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_segment_duration() -> f64 {
    6.0
}
fn default_max_segments() -> usize {
    10
}
fn default_max_streams() -> usize {
    100
}
fn default_stream_timeout() -> u64 {
    30
}
fn default_rtmp_listen() -> String {
    "0.0.0.0:1935".to_string()
}
fn default_srt_listen() -> String {
    "0.0.0.0:9000".to_string()
}
fn default_srt_latency() -> u32 {
    120
}
fn default_part_duration() -> f64 {
    0.33
}
fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = IngestConfig::default();
        assert!(!config.enabled);
        assert!(!config.rtmp.enabled);
        assert!(!config.srt.enabled);
        assert!(config.ll_hls.enabled);
        assert_eq!(config.segment_duration, 6.0);
        assert_eq!(config.max_segments, 10);
        assert_eq!(config.max_streams, 100);
    }

    #[test]
    fn test_deserialize_minimal() {
        let json = r#"{"enabled": true}"#;
        let config: IngestConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert!(!config.rtmp.enabled);
        assert_eq!(config.rtmp.listen, "0.0.0.0:1935");
    }

    #[test]
    fn test_deserialize_full() {
        let json = r#"{
            "enabled": true,
            "rtmp": {"enabled": true, "listen": "0.0.0.0:1935"},
            "srt": {"enabled": true, "listen": "0.0.0.0:9000", "latency_ms": 200},
            "segment_duration": 4.0,
            "ll_hls": {"enabled": true, "part_duration": 0.5},
            "max_segments": 20,
            "max_streams": 50,
            "stream_timeout_secs": 60,
            "stream_keys": [
                {"key": "secret", "app": "live", "stream_name": "test", "enabled": true}
            ]
        }"#;
        let config: IngestConfig = serde_json::from_str(json).unwrap();
        assert!(config.rtmp.enabled);
        assert!(config.srt.enabled);
        assert_eq!(config.srt.latency_ms, 200);
        assert_eq!(config.segment_duration, 4.0);
        assert_eq!(config.stream_keys.len(), 1);
        assert_eq!(config.stream_keys[0].key, "secret");
    }
}
