use once_cell::sync::Lazy;
use prometheus::{
    register_histogram_vec, register_int_counter_vec,
    HistogramVec, IntCounterVec, IntGauge,
};

// ── Counters ──

pub static HTTP_REQUESTS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "cdn_http_requests_total",
        "Total HTTP requests",
        &["site_id", "method", "status", "cache_status"]
    )
    .unwrap()
});

pub static HTTP_BYTES_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "cdn_http_bytes_total",
        "Total response bytes",
        &["site_id", "direction"] // direction: "in" or "out"
    )
    .unwrap()
});

pub static UPSTREAM_REQUESTS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "cdn_upstream_requests_total",
        "Total upstream requests",
        &["site_id", "origin_id", "status"]
    )
    .unwrap()
});

pub static UPSTREAM_FAILURES_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "cdn_upstream_failures_total",
        "Total upstream connection failures",
        &["site_id", "origin_id"]
    )
    .unwrap()
});

pub static REDIRECT_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "cdn_redirect_total",
        "Total redirects served",
        &["site_id", "type"] // type: domain, protocol, url_rule
    )
    .unwrap()
});

// ── Gauges ──

pub static CONNECTIONS_ACTIVE: Lazy<IntGauge> = Lazy::new(|| {
    prometheus::register_int_gauge!(
        "cdn_connections_active",
        "Currently active connections"
    )
    .unwrap()
});

// ── Histograms ──

pub static REQUEST_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "cdn_request_duration_seconds",
        "Request processing duration",
        &["site_id"],
        vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]
    )
    .unwrap()
});

pub static UPSTREAM_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "cdn_upstream_duration_seconds",
        "Upstream response duration",
        &["site_id", "origin_id"],
        vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]
    )
    .unwrap()
});

pub static RESPONSE_SIZE: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "cdn_response_size_bytes",
        "Response body size",
        &["site_id"],
        vec![100.0, 1_000.0, 10_000.0, 100_000.0, 1_000_000.0, 10_000_000.0]
    )
    .unwrap()
});

// ── Health Check ──

pub static HEALTH_CHECK_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "cdn_health_check_total",
        "Total active health check probes",
        &["site_id", "origin_id", "result"]
    )
    .unwrap()
});

pub static HEALTH_CHECK_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "cdn_health_check_duration_seconds",
        "Active health check probe duration",
        &["site_id", "origin_id"],
        vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0]
    )
    .unwrap()
});

// ── Cache Purge ──

pub static CACHE_PURGE_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "cdn_cache_purge_total",
        "Total cache purge operations",
        &["site_id", "purge_type", "result"]
    )
    .unwrap()
});

pub static CACHE_PURGE_KEYS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "cdn_cache_purge_keys_total",
        "Total cache keys purged",
        &["site_id", "purge_type"]
    )
    .unwrap()
});

pub static CACHE_PURGE_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "cdn_cache_purge_duration_seconds",
        "Cache purge operation duration",
        &["site_id", "purge_type"],
        vec![0.001, 0.01, 0.1, 0.5, 1.0, 5.0, 10.0, 30.0, 60.0, 300.0]
    )
    .unwrap()
});

// ── Image Optimization ──

pub static IMAGE_OPTIMIZATIONS_TOTAL: Lazy<IntCounterVec> = Lazy::new(|| {
    register_int_counter_vec!(
        "cdn_image_optimizations_total",
        "Total image optimization operations",
        &["site_id", "output_format", "result"]
    )
    .unwrap()
});

pub static IMAGE_OPTIMIZATION_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "cdn_image_optimization_duration_seconds",
        "Image optimization processing duration",
        &["site_id"],
        vec![0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0]
    )
    .unwrap()
});

pub static IMAGE_OPTIMIZATION_SIZE_RATIO: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "cdn_image_optimization_size_ratio",
        "Image output/input size ratio",
        &["site_id"],
        vec![0.1, 0.2, 0.3, 0.5, 0.7, 1.0, 1.5, 2.0]
    )
    .unwrap()
});

/// Record metrics for a completed request.
pub fn record_request(
    site_id: &str,
    method: &str,
    status: u16,
    cache_status: &str,
    response_size: u64,
    duration_secs: f64,
    origin_id: Option<&str>,
) {
    let status_class = match status {
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "other",
    };

    HTTP_REQUESTS_TOTAL
        .with_label_values(&[site_id, method, status_class, cache_status])
        .inc();

    HTTP_BYTES_TOTAL
        .with_label_values(&[site_id, "out"])
        .inc_by(response_size);

    REQUEST_DURATION
        .with_label_values(&[site_id])
        .observe(duration_secs);

    RESPONSE_SIZE
        .with_label_values(&[site_id])
        .observe(response_size as f64);

    if let Some(oid) = origin_id {
        UPSTREAM_REQUESTS_TOTAL
            .with_label_values(&[site_id, oid, status_class])
            .inc();

        UPSTREAM_DURATION
            .with_label_values(&[site_id, oid])
            .observe(duration_secs);
    }
}
