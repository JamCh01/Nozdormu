use bytes::Bytes;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::error::IngestError;

/// Shared store for all active live streams.
///
/// Concurrently accessible from ingest tasks (writers) and HTTP handlers (readers).
pub struct LiveStreamStore {
    streams: DashMap<String, Arc<RwLock<LiveStream>>>,
    max_segments: usize,
    max_streams: usize,
}

/// A live stream with its ring buffer of segments.
pub struct LiveStream {
    pub app: String,
    pub stream_name: String,
    pub started_at: DateTime<Utc>,
    pub last_frame_at: DateTime<Utc>,

    /// fMP4 init segment (ftyp + moov), built once from codec config.
    pub init_segment: Option<Bytes>,

    /// Completed segments in ring buffer.
    pub segments: VecDeque<LiveSegment>,
    /// Media sequence number of the first segment in the deque.
    pub media_sequence: u64,
    /// Next sequence number to assign.
    pub next_sequence: u64,

    /// Completed parts of the in-progress segment (LL-HLS).
    pub current_parts: Vec<LivePart>,
    /// Sequence number of the in-progress segment.
    pub current_part_sequence: u64,
    /// Accumulated duration of the in-progress segment.
    pub current_segment_duration: f64,

    /// Configuration.
    pub segment_duration: f64,
    pub part_duration: f64,
    pub ll_hls_enabled: bool,
    pub max_segments: usize,

    /// Whether the stream has ended (encoder disconnected).
    pub ended: bool,

    /// Video/audio metadata for admin API.
    pub video_width: u32,
    pub video_height: u32,
    pub audio_sample_rate: u32,
    pub audio_channels: u16,

    /// Waiters for blocking playlist reload (LL-HLS).
    pub waiters: Vec<Waiter>,
}

pub struct Waiter {
    target_msn: u64,
    target_part: Option<u32>,
    sender: tokio::sync::oneshot::Sender<()>,
}

/// A completed HLS segment.
pub struct LiveSegment {
    pub sequence: u64,
    pub duration: f64,
    pub data: Bytes,
    pub parts: Vec<LivePart>,
    pub independent: bool,
}

/// A partial segment (LL-HLS).
#[derive(Clone)]
pub struct LivePart {
    pub index: u32,
    pub duration: f64,
    pub data: Bytes,
    pub independent: bool,
}

/// Summary info for admin API.
#[derive(Debug, Serialize)]
pub struct StreamInfo {
    pub app: String,
    pub stream_name: String,
    pub started_at: String,
    pub last_frame_at: String,
    pub duration_secs: f64,
    pub segments_buffered: usize,
    pub media_sequence: u64,
    pub video_resolution: String,
    pub audio_info: String,
    pub ended: bool,
}

impl LiveStreamStore {
    pub fn new(max_segments: usize, max_streams: usize) -> Self {
        Self {
            streams: DashMap::new(),
            max_segments,
            max_streams,
        }
    }

    /// Create a new live stream. Returns error if max streams reached or already exists.
    pub fn create_stream(
        &self,
        app: &str,
        stream_name: &str,
        segment_duration: f64,
        part_duration: f64,
        ll_hls_enabled: bool,
    ) -> Result<Arc<RwLock<LiveStream>>, IngestError> {
        let key = format!("{}/{}", app, stream_name);

        if self.streams.len() >= self.max_streams {
            return Err(IngestError::MaxStreams(self.max_streams));
        }

        let now = Utc::now();
        let stream = Arc::new(RwLock::new(LiveStream {
            app: app.to_string(),
            stream_name: stream_name.to_string(),
            started_at: now,
            last_frame_at: now,
            init_segment: None,
            segments: VecDeque::new(),
            media_sequence: 0,
            next_sequence: 0,
            current_parts: Vec::new(),
            current_part_sequence: 0,
            current_segment_duration: 0.0,
            segment_duration,
            part_duration,
            ll_hls_enabled,
            max_segments: self.max_segments,
            ended: false,
            video_width: 0,
            video_height: 0,
            audio_sample_rate: 0,
            audio_channels: 0,
            waiters: Vec::new(),
        }));

        use dashmap::mapref::entry::Entry;
        match self.streams.entry(key.clone()) {
            Entry::Occupied(_) => Err(IngestError::AlreadyExists(key)),
            Entry::Vacant(e) => {
                e.insert(Arc::clone(&stream));
                crate::metrics::INGEST_ACTIVE_STREAMS.inc();
                Ok(stream)
            }
        }
    }

    /// Mark a stream as ended (encoder disconnected).
    pub fn end_stream(&self, app: &str, stream_name: &str) {
        let key = format!("{}/{}", app, stream_name);
        if let Some(entry) = self.streams.get(&key) {
            let stream = Arc::clone(entry.value());
            tokio::spawn(async move {
                let mut s = stream.write().await;
                s.ended = true;
                s.notify_all_waiters();
            });
        }
    }

    /// Remove a stream entirely (admin kick).
    pub fn remove_stream(&self, app: &str, stream_name: &str) -> bool {
        let key = format!("{}/{}", app, stream_name);
        if self.streams.remove(&key).is_some() {
            crate::metrics::INGEST_ACTIVE_STREAMS.dec();
            true
        } else {
            false
        }
    }

    /// Get a stream for reading.
    pub fn get_stream(&self, app: &str, stream_name: &str) -> Option<Arc<RwLock<LiveStream>>> {
        let key = format!("{}/{}", app, stream_name);
        self.streams.get(&key).map(|e| Arc::clone(e.value()))
    }

    /// List all active streams.
    pub async fn list_streams(&self) -> Vec<StreamInfo> {
        let mut result = Vec::new();
        for entry in self.streams.iter() {
            let stream = entry.value().read().await;
            let now = Utc::now();
            let duration = (now - stream.started_at).num_milliseconds() as f64 / 1000.0;
            result.push(StreamInfo {
                app: stream.app.clone(),
                stream_name: stream.stream_name.clone(),
                started_at: stream.started_at.to_rfc3339(),
                last_frame_at: stream.last_frame_at.to_rfc3339(),
                duration_secs: duration,
                segments_buffered: stream.segments.len(),
                media_sequence: stream.media_sequence,
                video_resolution: format!("{}x{}", stream.video_width, stream.video_height),
                audio_info: format!("{}Hz {}ch", stream.audio_sample_rate, stream.audio_channels),
                ended: stream.ended,
            });
        }
        result
    }

    /// Number of active streams.
    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    /// Remove stale streams (no frames for > timeout_secs).
    pub fn remove_stale_streams(&self, timeout_secs: u64) -> usize {
        let now = Utc::now();
        let mut removed = 0;
        let mut to_remove = Vec::new();

        for entry in self.streams.iter() {
            let key = entry.key().clone();
            // We can't await inside DashMap iteration, so collect keys to check
            to_remove.push(key);
        }

        // This is a simplified sync check — in practice we'd use try_read
        let mut keys_to_remove = Vec::new();
        for key in to_remove {
            if let Some(entry) = self.streams.get(&key) {
                let stream = entry.value();
                if let Ok(s) = stream.try_read() {
                    let elapsed = (now - s.last_frame_at).num_seconds().unsigned_abs();
                    if elapsed > timeout_secs || s.ended {
                        keys_to_remove.push(key);
                    }
                }
            }
        }
        for key in keys_to_remove {
            if self.streams.remove(&key).is_some() {
                crate::metrics::INGEST_ACTIVE_STREAMS.dec();
                removed += 1;
            }
        }
        removed
    }
}

impl LiveStream {
    /// Push a completed segment into the ring buffer.
    pub fn push_segment(&mut self, segment: LiveSegment) {
        // Move current parts into the segment (they belong to the previous in-progress segment)
        self.segments.push_back(segment);

        // Evict oldest if over limit
        while self.segments.len() > self.max_segments {
            self.segments.pop_front();
            self.media_sequence += 1;
        }

        // Reset in-progress state
        self.current_parts.clear();
        self.current_segment_duration = 0.0;
        self.next_sequence += 1;
        self.current_part_sequence = self.next_sequence;

        self.notify_waiters();
    }

    /// Push a completed part for the in-progress segment (LL-HLS).
    pub fn push_part(&mut self, part: LivePart) {
        self.current_parts.push(part);
        self.notify_waiters();
    }

    /// Set the init segment (called once when codec config is ready).
    pub fn set_init_segment(&mut self, data: Bytes) {
        self.init_segment = Some(data);
    }

    /// Register a waiter for blocking playlist reload.
    pub fn add_waiter(
        &mut self,
        target_msn: u64,
        target_part: Option<u32>,
    ) -> tokio::sync::oneshot::Receiver<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.waiters.push(Waiter {
            target_msn,
            target_part,
            sender: tx,
        });
        rx
    }

    /// Check if a specific MSN/part is available.
    pub fn is_available(&self, target_msn: u64, target_part: Option<u32>) -> bool {
        // Check completed segments
        if let Some(last_seg) = self.segments.back() {
            if target_msn <= last_seg.sequence {
                return true;
            }
        }

        // Check in-progress parts
        if target_msn == self.current_part_sequence {
            match target_part {
                None => !self.current_parts.is_empty(),
                Some(p) => self.current_parts.len() > p as usize,
            }
        } else {
            false
        }
    }

    fn notify_waiters(&mut self) {
        let waiters = std::mem::take(&mut self.waiters);
        let mut remaining = Vec::new();
        for w in waiters {
            if self.is_available(w.target_msn, w.target_part) || self.ended {
                let _ = w.sender.send(());
            } else {
                remaining.push(w);
            }
        }
        self.waiters = remaining;
    }

    fn notify_all_waiters(&mut self) {
        for w in self.waiters.drain(..) {
            let _ = w.sender.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_and_get_stream() {
        let store = LiveStreamStore::new(10, 100);
        let stream = store
            .create_stream("live", "test", 6.0, 0.33, true)
            .unwrap();
        assert_eq!(store.stream_count(), 1);

        let got = store.get_stream("live", "test");
        assert!(got.is_some());

        let s = stream.read().await;
        assert_eq!(s.app, "live");
        assert_eq!(s.stream_name, "test");
        assert!(!s.ended);
    }

    #[tokio::test]
    async fn test_duplicate_stream_rejected() {
        let store = LiveStreamStore::new(10, 100);
        store
            .create_stream("live", "test", 6.0, 0.33, true)
            .unwrap();
        let result = store.create_stream("live", "test", 6.0, 0.33, true);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_max_streams_limit() {
        let store = LiveStreamStore::new(10, 2);
        store.create_stream("live", "s1", 6.0, 0.33, true).unwrap();
        store.create_stream("live", "s2", 6.0, 0.33, true).unwrap();
        let result = store.create_stream("live", "s3", 6.0, 0.33, true);
        assert!(matches!(result, Err(IngestError::MaxStreams(2))));
    }

    #[tokio::test]
    async fn test_ring_buffer_eviction() {
        let store = LiveStreamStore::new(3, 100);
        let stream_arc = store
            .create_stream("live", "test", 6.0, 0.33, true)
            .unwrap();

        {
            let mut stream = stream_arc.write().await;
            for i in 0..5 {
                stream.push_segment(LiveSegment {
                    sequence: i,
                    duration: 6.0,
                    data: Bytes::from(vec![0u8; 100]),
                    parts: Vec::new(),
                    independent: true,
                });
            }

            assert_eq!(stream.segments.len(), 3);
            assert_eq!(stream.media_sequence, 2); // evicted 0, 1
            assert_eq!(stream.segments.front().unwrap().sequence, 2);
            assert_eq!(stream.segments.back().unwrap().sequence, 4);
        }
    }

    #[tokio::test]
    async fn test_remove_stream() {
        let store = LiveStreamStore::new(10, 100);
        store
            .create_stream("live", "test", 6.0, 0.33, true)
            .unwrap();
        assert!(store.remove_stream("live", "test"));
        assert_eq!(store.stream_count(), 0);
        assert!(!store.remove_stream("live", "test"));
    }

    #[tokio::test]
    async fn test_end_stream() {
        let store = LiveStreamStore::new(10, 100);
        let stream_arc = store
            .create_stream("live", "test", 6.0, 0.33, true)
            .unwrap();
        store.end_stream("live", "test");
        // Give the spawned task time to run
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let s = stream_arc.read().await;
        assert!(s.ended);
    }

    #[tokio::test]
    async fn test_waiter_notification() {
        let store = LiveStreamStore::new(10, 100);
        let stream_arc = store
            .create_stream("live", "test", 6.0, 0.33, true)
            .unwrap();

        let rx = {
            let mut stream = stream_arc.write().await;
            stream.add_waiter(0, None)
        };

        // Push a segment — should notify the waiter
        {
            let mut stream = stream_arc.write().await;
            stream.push_segment(LiveSegment {
                sequence: 0,
                duration: 6.0,
                data: Bytes::from(vec![0u8; 100]),
                parts: Vec::new(),
                independent: true,
            });
        }

        let result = tokio::time::timeout(std::time::Duration::from_millis(100), rx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_is_available() {
        let store = LiveStreamStore::new(10, 100);
        let stream_arc = store
            .create_stream("live", "test", 6.0, 0.33, true)
            .unwrap();

        let mut stream = stream_arc.write().await;
        assert!(!stream.is_available(0, None));

        stream.push_segment(LiveSegment {
            sequence: 0,
            duration: 6.0,
            data: Bytes::from(vec![0u8; 100]),
            parts: Vec::new(),
            independent: true,
        });
        assert!(stream.is_available(0, None));
        assert!(!stream.is_available(1, None));

        // Push a part for in-progress segment
        stream.push_part(LivePart {
            index: 0,
            duration: 0.33,
            data: Bytes::from(vec![0u8; 50]),
            independent: true,
        });
        assert!(stream.is_available(1, Some(0)));
        assert!(!stream.is_available(1, Some(1)));
    }
}
