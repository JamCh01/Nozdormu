use async_trait::async_trait;

#[cfg(feature = "kafka-sink")]
pub mod kafka;
#[cfg(feature = "nats-sink")]
pub mod nats;
#[cfg(feature = "pulsar-sink")]
pub mod pulsar;
#[cfg(feature = "rabbitmq-sink")]
pub mod rabbitmq;
#[cfg(feature = "redis-sink")]
pub mod redis;

/// Trait for log sink backends.
///
/// Each call to `send` targets a specific destination string whose meaning
/// depends on the backend:
/// - Kafka/Pulsar: topic name
/// - NATS: subject
/// - RabbitMQ: routing_key
/// - Redis: stream_key
#[async_trait]
pub trait LogSink: Send + Sync + 'static {
    /// Send a batch of JSON-serialized log entries to a specific destination.
    async fn send(&self, destination: &str, entries: &[String]) -> Result<(), LogSinkError>;

    /// Flush any buffered data. Called on graceful shutdown.
    async fn flush(&self) -> Result<(), LogSinkError>;

    /// Human-readable backend name for log messages (e.g., "redis", "kafka").
    fn name(&self) -> &'static str;
}

/// Errors from log sink operations.
#[derive(Debug, thiserror::Error)]
pub enum LogSinkError {
    #[error("connection failed: {0}")]
    Connection(String),
    #[error("send failed: {0}")]
    Send(String),
    #[error("configuration error: {0}")]
    Config(String),
}
