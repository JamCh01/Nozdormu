/// Errors that can occur during live ingest operations.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("stream key rejected: {0}")]
    AuthFailed(String),

    #[error("max streams reached ({0})")]
    MaxStreams(usize),

    #[error("codec error: {0}")]
    Codec(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("stream not found: {0}")]
    NotFound(String),

    #[error("timeout waiting for segment")]
    Timeout,

    #[error("stream already exists: {0}")]
    AlreadyExists(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
