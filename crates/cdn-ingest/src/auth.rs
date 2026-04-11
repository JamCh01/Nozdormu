use crate::config::StreamKeyEntry;
use crate::error::IngestError;

/// Validate a stream key against the configured key list.
///
/// Returns `(app, stream_name)` on success.
///
/// RTMP URL format: `rtmp://host/{app}/{stream_key}`
/// SRT streamid format: `{app}/{stream_key}`
pub fn validate_stream_key(
    keys: &[StreamKeyEntry],
    app: &str,
    stream_key: &str,
) -> Result<(String, String), IngestError> {
    for entry in keys {
        if entry.enabled && entry.app == app && entry.key == stream_key {
            crate::metrics::INGEST_AUTH_TOTAL
                .with_label_values(&["accepted"])
                .inc();
            return Ok((entry.app.clone(), entry.stream_name.clone()));
        }
    }
    crate::metrics::INGEST_AUTH_TOTAL
        .with_label_values(&["rejected"])
        .inc();
    Err(IngestError::AuthFailed(format!(
        "invalid stream key for app '{}'",
        app
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_keys() -> Vec<StreamKeyEntry> {
        vec![
            StreamKeyEntry {
                key: "secret123".to_string(),
                app: "live".to_string(),
                stream_name: "stream1".to_string(),
                enabled: true,
            },
            StreamKeyEntry {
                key: "disabled_key".to_string(),
                app: "live".to_string(),
                stream_name: "stream2".to_string(),
                enabled: false,
            },
        ]
    }

    #[test]
    fn test_valid_key() {
        let keys = test_keys();
        let result = validate_stream_key(&keys, "live", "secret123");
        assert!(result.is_ok());
        let (app, name) = result.unwrap();
        assert_eq!(app, "live");
        assert_eq!(name, "stream1");
    }

    #[test]
    fn test_invalid_key() {
        let keys = test_keys();
        let result = validate_stream_key(&keys, "live", "wrong_key");
        assert!(matches!(result, Err(IngestError::AuthFailed(_))));
    }

    #[test]
    fn test_disabled_key() {
        let keys = test_keys();
        let result = validate_stream_key(&keys, "live", "disabled_key");
        assert!(matches!(result, Err(IngestError::AuthFailed(_))));
    }

    #[test]
    fn test_wrong_app() {
        let keys = test_keys();
        let result = validate_stream_key(&keys, "other", "secret123");
        assert!(matches!(result, Err(IngestError::AuthFailed(_))));
    }

    #[test]
    fn test_empty_keys() {
        let result = validate_stream_key(&[], "live", "secret123");
        assert!(matches!(result, Err(IngestError::AuthFailed(_))));
    }
}
