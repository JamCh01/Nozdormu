pub mod metrics;
pub mod queue;

// Request logging: collect data → Prometheus metrics → passive health check → Redis Streams.
