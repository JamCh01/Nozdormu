use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use crate::error::IngestError;
use crate::store::LiveStream;

/// Wait until a specific media sequence number (and optionally part) is available.
///
/// Used for LL-HLS blocking playlist reload: when a player requests
/// `?_HLS_msn=N&_HLS_part=P`, the server holds the response until
/// segment N / part P is available or the timeout expires.
pub async fn wait_for_availability(
    stream: &Arc<RwLock<LiveStream>>,
    target_msn: u64,
    target_part: Option<u32>,
    timeout: Duration,
) -> Result<(), IngestError> {
    // Check if already available
    {
        let s = stream.read().await;
        if s.is_available(target_msn, target_part) || s.ended {
            return Ok(());
        }
    }

    // Register a waiter
    let rx = {
        let mut s = stream.write().await;
        // Double-check after acquiring write lock
        if s.is_available(target_msn, target_part) || s.ended {
            return Ok(());
        }
        s.add_waiter(target_msn, target_part)
    };

    // Wait with timeout
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => {
            // Sender dropped — stream ended or removed
            Ok(())
        }
        Err(_) => Err(IngestError::Timeout),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{LiveSegment, LiveStreamStore};
    use bytes::Bytes;

    #[tokio::test]
    async fn test_already_available() {
        let store = LiveStreamStore::new(10, 100);
        let stream = store
            .create_stream("live", "test", 6.0, 0.33, true)
            .unwrap();

        {
            let mut s = stream.write().await;
            s.push_segment(LiveSegment {
                sequence: 0,
                duration: 6.0,
                data: Bytes::from(vec![0u8; 100]),
                parts: Vec::new(),
                independent: true,
            });
        }

        let result = wait_for_availability(&stream, 0, None, Duration::from_millis(100)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_wait_and_notify() {
        let store = LiveStreamStore::new(10, 100);
        let stream = store
            .create_stream("live", "test", 6.0, 0.33, true)
            .unwrap();

        let stream_clone = Arc::clone(&stream);
        let handle = tokio::spawn(async move {
            wait_for_availability(&stream_clone, 0, None, Duration::from_secs(5)).await
        });

        // Small delay then push segment
        tokio::time::sleep(Duration::from_millis(50)).await;
        {
            let mut s = stream.write().await;
            s.push_segment(LiveSegment {
                sequence: 0,
                duration: 6.0,
                data: Bytes::from(vec![0u8; 100]),
                parts: Vec::new(),
                independent: true,
            });
        }

        let result = handle.await.unwrap();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_timeout() {
        let store = LiveStreamStore::new(10, 100);
        let stream = store
            .create_stream("live", "test", 6.0, 0.33, true)
            .unwrap();

        let result = wait_for_availability(&stream, 0, None, Duration::from_millis(50)).await;
        assert!(matches!(result, Err(IngestError::Timeout)));
    }
}
