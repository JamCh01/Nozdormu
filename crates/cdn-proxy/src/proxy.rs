use crate::balancer::DynamicBalancer;
use crate::context::{CacheStatus, ProxyCtx, ProtocolType};
use arc_swap::ArcSwap;
use async_trait::async_trait;
use bytes::Bytes;
use cdn_config::LiveConfig;
use cdn_middleware::cc::{action::CcActionResult, CcEngine};
use cdn_middleware::redirect;
use cdn_middleware::waf::{CompiledWafSets, WafEngine, WafResult};
use pingora::http::ResponseHeader;
use pingora::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

/// Global monotonic counter for WAF sets cache versioning.
/// Incremented each time a new SiteConfig Arc is seen, preventing ABA reuse.
static WAF_CACHE_VERSION: AtomicU64 = AtomicU64::new(0);

// Thread-local cache for compiled WAF IP sets, keyed by (site_id, version).
// The version is assigned per unique Arc<SiteConfig> pointer, so config reloads
// always get a fresh entry even if the allocator reuses the same address.
thread_local! {
    static WAF_SETS_CACHE: RefCell<HashMap<(usize, u64), Arc<CompiledWafSets>>> = RefCell::new(HashMap::new());
}

fn get_compiled_waf_sets(site: &Arc<cdn_common::SiteConfig>) -> Arc<CompiledWafSets> {
    let ptr = Arc::as_ptr(site) as usize;
    WAF_SETS_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        // Check if any entry with this pointer exists
        if let Some((&key, sets)) = cache.iter().find(|((p, _), _)| *p == ptr) {
            let _ = key;
            return Arc::clone(sets);
        }
        let version = WAF_CACHE_VERSION.fetch_add(1, AtomicOrdering::Relaxed);
        let sets = Arc::new(CompiledWafSets::build(&site.waf));
        // Evict stale entries (keep max 64 per thread)
        if cache.len() > 64 {
            cache.clear();
        }
        cache.insert((ptr, version), Arc::clone(&sets));
        sets
    })
}

pub struct CdnProxy {
    /// Static LB (temporary fallback, used when no site config is available)
    pub lb: Arc<LoadBalancer<RoundRobin>>,
    pub sni: String,
    pub tls: bool,
    pub live_config: Arc<ArcSwap<LiveConfig>>,
    pub waf_engine: Arc<WafEngine>,
    pub cc_engine: Arc<CcEngine>,
    pub balancer: Arc<DynamicBalancer>,
    pub challenge_store: Arc<crate::ssl::challenge::ChallengeStore>,
    pub redis_pool: Arc<crate::utils::redis_pool::RedisPool>,
    pub trusted_proxies: Vec<ipnet::IpNet>,
    pub default_compression: cdn_common::CompressionConfig,
    pub default_image_optimization: cdn_common::ImageOptimizationConfig,
    pub prefetch_worker: Arc<cdn_streaming::prefetch::PrefetchWorker>,
}

#[async_trait]
impl ProxyHttp for CdnProxy {
    type CTX = ProxyCtx;

    fn new_ctx(&self) -> Self::CTX {
        ProxyCtx::default()
    }

    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<bool> {
        let path = session.req_header().uri.path();

        // ── 1. Public endpoints (before routing) ──
        if path == "/health" {
            return self.serve_health(session).await;
        }

        // ── 2. ACME challenge (before routing) ──
        if path.starts_with("/.well-known/acme-challenge/") {
            let host_header = session
                .req_header()
                .headers
                .get("host")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if let Some(key_auth) = self.challenge_store.get_by_path(host_header, path) {
                return self.serve_acme_challenge(session, &key_auth).await;
            }
            return self.serve_not_found(session).await;
        }

        // ── 3. Internal endpoints (IP restricted) ──
        if matches!(path, "/health/detail" | "/status") {
            let remote = session
                .client_addr()
                .and_then(|a| a.as_inet())
                .map(|a| a.ip());
            if !remote.map(|ip| crate::utils::ip::is_private_ip(ip)).unwrap_or(false) {
                return self.serve_forbidden(session).await;
            }
            if path == "/health/detail" {
                return self.serve_health_detail(session).await;
            } else {
                return self.serve_status(session).await;
            }
        }

        // ── 4. Client IP extraction ──
        ctx.client_ip = session
            .client_addr()
            .and_then(|a| a.as_inet())
            .map(|a| a.ip());

        // XFF anti-spoofing
        if let (Some(remote_ip), Some(xff)) = (
            ctx.client_ip,
            session.req_header().headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()),
        ) {
            ctx.client_ip = Some(crate::utils::ip::real_ip_from_xff(xff, remote_ip, &self.trusted_proxies));
        }

        // ── 5. Route matching ──
        let host = session
            .req_header()
            .uri
            .authority()
            .map(|a| a.as_str())
            .or_else(|| {
                session
                    .req_header()
                    .headers
                    .get("host")
                    .and_then(|v| v.to_str().ok())
            })
            .unwrap_or("")
            .to_string();

        let config = self.live_config.load();
        match config.match_site(&host) {
            Some(site) => {
                ctx.site_id = site.site_id.clone();
                ctx.site_config = Some(site);
            }
            None => {
                log::warn!("[Access] site not found: {}", &host);
                return self.serve_not_found(session).await;
            }
        }

        // Cache request info for response_filter and logging (avoids re-extraction)
        ctx.host = host.clone();
        ctx.uri = session.req_header().uri.to_string();
        // Detect scheme from: 1) URI scheme, 2) downstream TLS digest, 3) fallback "http"
        ctx.scheme = session
            .req_header()
            .uri
            .scheme_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                let is_client_tls = session
                    .digest()
                    .and_then(|d| d.ssl_digest.as_ref())
                    .is_some();
                if is_client_tls { "https" } else { "http" }.to_string()
            });

        // ── 6. WAF check ──
        if let Some(ref site) = ctx.site_config {
            if let Some(client_ip) = ctx.client_ip {
                let waf_sets = get_compiled_waf_sets(site);
                let (waf_result, geo_info) =
                    self.waf_engine.check_with_sets(
                        client_ip, &site.waf, &ctx.site_id,
                        Some(&waf_sets.whitelist), Some(&waf_sets.blacklist),
                    );

                // Cache GeoIP info in context (queried once per request)
                if let Some(geo) = geo_info {
                    ctx.geo_info = Some(crate::context::GeoInfo {
                        country_code: geo.country_code,
                        country_name: geo.country_name,
                        continent_code: geo.continent_code,
                        subdivision_code: geo.subdivision_code,
                        subdivision_name: geo.subdivision_name,
                        city_name: geo.city_name,
                        asn: geo.asn,
                        asn_org: geo.asn_org,
                        latitude: geo.latitude,
                        longitude: geo.longitude,
                    });
                }

                match waf_result {
                    WafResult::Block { reason, .. } => {
                        ctx.waf_blocked = true;
                        ctx.waf_reason = Some(reason);
                        return self.serve_forbidden(session).await;
                    }
                    WafResult::Log { reason, .. } => {
                        ctx.waf_reason = Some(reason);
                        // Continue processing — log-only mode
                    }
                    WafResult::Allow => {}
                }
            }
        }

        // ── 6.5. Body inspection: Content-Length pre-check ──
        if let Some(ref site) = ctx.site_config {
            if site.waf.body_inspection.enabled {
                let method = session.req_header().method.as_str();
                let content_length = session
                    .req_header()
                    .headers
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok());

                let result = cdn_middleware::waf::body::check_content_length(
                    content_length,
                    method,
                    &site.waf.body_inspection,
                );
                match result {
                    cdn_middleware::waf::body::BodyCheckResult::TooLarge { limit, actual } => {
                        ctx.body_rejected = true;
                        crate::logging::metrics::BODY_INSPECTION_TOTAL
                            .with_label_values(&[ctx.site_id.as_str(), "size_rejected"])
                            .inc();
                        return self.serve_payload_too_large(session, limit, actual).await;
                    }
                    cdn_middleware::waf::body::BodyCheckResult::Allow => {
                        if site
                            .waf
                            .body_inspection
                            .inspect_methods
                            .iter()
                            .any(|m| m.eq_ignore_ascii_case(method))
                        {
                            ctx.body_inspection_enabled = true;
                            ctx.body_max_size = site.waf.body_inspection.max_body_size;
                        }
                    }
                    _ => {}
                }
            }
        }

        // ── 7. CC check ──
        if let Some(ref site) = ctx.site_config {
            if let Some(client_ip) = ctx.client_ip {
                let uri = session.req_header().uri.to_string();
                let path = session.req_header().uri.path().to_string();
                let cookie_header = session
                    .req_header()
                    .headers
                    .get("cookie")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());

                let cc_result = self
                    .cc_engine
                    .check(
                        client_ip,
                        &uri,
                        &path,
                        cookie_header.as_deref(),
                        &site.cc,
                        &ctx.site_id,
                    )
                    .await;

                match cc_result {
                    CcActionResult::Block { retry_after, reason } => {
                        ctx.cc_blocked = true;
                        ctx.cc_reason = Some(reason);
                        return self.serve_too_many_requests(session, retry_after).await;
                    }
                    CcActionResult::Challenge { cookie_value, reason } => {
                        ctx.cc_reason = Some(reason);
                        return self.serve_challenge(session, &cookie_value).await;
                    }
                    CcActionResult::Delay { delay_ms, reason } => {
                        ctx.cc_reason = Some(reason);
                        // Return 429 instead of sleeping to prevent task queue saturation under DDoS
                        return self.serve_too_many_requests(session, (delay_ms / 1000).max(1)).await;
                    }
                    CcActionResult::Log { reason } => {
                        ctx.cc_reason = Some(reason);
                        // Continue processing — log-only mode
                    }
                    CcActionResult::Allow => {}
                }
            }
        }

        // ── 7.5. Edge Auth / URL Signing ──
        if let Some(ref site) = ctx.site_config {
            if site.streaming.auth.enabled {
                let path = session.req_header().uri.path();
                let query = session.req_header().uri.query();
                match cdn_streaming::auth::validate_url(
                    &site.streaming.auth,
                    path,
                    query,
                ) {
                    Ok(cleaned_path) => {
                        ctx.auth_cleaned_path = Some(cleaned_path);
                        crate::logging::metrics::STREAMING_AUTH_TOTAL
                            .with_label_values(&[ctx.site_id.as_str(), "accepted"])
                            .inc();
                    }
                    Err(e) => {
                        log::warn!(
                            "[Auth] URL signing validation failed: {}, path={}",
                            e, path
                        );
                        crate::logging::metrics::STREAMING_AUTH_TOTAL
                            .with_label_values(&[ctx.site_id.as_str(), "rejected"])
                            .inc();
                        return self.serve_forbidden(session).await;
                    }
                }
            }
        }

        // ── 8. Redirect check ──
        if let Some(ref site) = ctx.site_config {
            let uri = session.req_header().uri.to_string();
            let path = session.req_header().uri.path();
            let query_string = session.req_header().uri.query();
            let method = session.req_header().method.as_str();

            if let Some(result) = redirect::check_redirect(
                &ctx.scheme,
                &host,
                &uri,
                path,
                query_string,
                method,
                site.domain_redirect.as_ref(),
                &site.protocol.force_https,
                &site.url_redirect_rules,
            ) {
                return self
                    .serve_redirect(session, &result.target_url, result.status_code, result.response_headers, result.cache_control.as_deref())
                    .await;
            }
        }

        // ── 9. Protocol detection ──
        if let Some(ref site) = ctx.site_config {
            ctx.protocol_type = crate::protocol::detect_protocol(session, &site.protocol);

            match &ctx.protocol_type {
                ProtocolType::WebSocket => {
                    if let Err(reason) = crate::protocol::validate_websocket(session) {
                        log::warn!("[Protocol] WebSocket validation failed: {}", reason);
                        return self.serve_bad_request(session, reason).await;
                    }
                    ctx.cache_status = CacheStatus::Bypass;
                }
                ProtocolType::Sse => {
                    ctx.cache_status = CacheStatus::Bypass;
                }
                ProtocolType::Grpc(_) => {
                    // gRPC service whitelist check
                    let path = session.req_header().uri.path();
                    if !crate::protocol::check_grpc_service_whitelist(
                        path,
                        &site.protocol.grpc.services,
                    ) {
                        log::warn!("[Protocol] gRPC service not in whitelist: {}", path);
                        return self.serve_forbidden(session).await;
                    }
                    ctx.cache_status = CacheStatus::Bypass;
                }
                ProtocolType::Http => {}
            }
        }

        // ── 10. Cache lookup ──
        if let Some(ref site) = ctx.site_config {
            if let Some(client_ip) = ctx.client_ip {
                let _ = client_ip; // used in future for vary
            }
            let path = session.req_header().uri.path();
            let method = session.req_header().method.as_str();
            let cache_control = session
                .req_header()
                .headers
                .get("cache-control")
                .and_then(|v| v.to_str().ok());
            let has_auth = session.req_header().headers.get("authorization").is_some();

            let decision = cdn_cache::strategy::check_request_cacheability(
                method,
                path,
                cache_control,
                has_auth,
                &site.cache,
            );

            if decision.cacheable {
                let query_string = session.req_header().uri.query();
                let vary_values: Vec<(String, String)> = site
                    .cache
                    .vary_headers
                    .iter()
                    .filter_map(|h| {
                        session
                            .req_header()
                            .headers
                            .get(h.as_str())
                            .and_then(|v| v.to_str().ok())
                            .map(|v| (h.clone(), v.to_string()))
                    })
                    .collect();

                let cache_key = cdn_cache::key::generate_cache_key(
                    &ctx.site_id,
                    &host,
                    path,
                    query_string,
                    site.cache.sort_query_string,
                    &vary_values,
                );

                ctx.cache_key = Some(cache_key);
                ctx.cache_ttl = Some(decision.ttl);
                // Cache HIT lookup would go here when CacheStorage is wired in
                // For now, all requests are MISS
                ctx.cache_status = CacheStatus::Miss;
            }
        }

        // ── 10.5. Range request handling ──
        if let Some(ref site) = ctx.site_config {
            if site.range.enabled {
                if let Some(range_val) = session
                    .req_header()
                    .headers
                    .get("range")
                    .and_then(|v| v.to_str().ok())
                {
                    if let Some(spec) = crate::range::parse_range_header(range_val) {
                        ctx.range_request = Some(spec);
                        match ctx.cache_status {
                            CacheStatus::Hit => {
                                ctx.range_served_from_cache = true;
                            }
                            _ => {
                                ctx.range_passthrough = true;
                            }
                        }
                    }
                }
            }
        }

        // ── 11. Image optimization: parse query params ──
        if let Some(ref site) = ctx.site_config {
            if site.image_optimization.enabled {
                let query = session.req_header().uri.query();
                if let Some(mut params) =
                    cdn_image::ImageParams::from_query(query, &site.image_optimization)
                {
                    params.clamp(&site.image_optimization);
                    ctx.image_params = Some(params);
                }
            }
        }

        // ── 12. Dynamic packaging detection ──
        if let Some(ref site) = ctx.site_config {
            if site.streaming.dynamic_packaging.enabled && ctx.image_params.is_none() {
                let query = session.req_header().uri.query();
                let accept = session
                    .req_header()
                    .headers
                    .get("accept")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");

                let is_hls_query = query.map(|q| q.contains("format=hls")).unwrap_or(false);
                let is_hls_accept = accept.contains("application/vnd.apple.mpegurl");

                if is_hls_query || is_hls_accept {
                    // Determine what sub-resource is requested
                    let segment_param = query.and_then(|q| {
                        q.split('&').find_map(|p| p.strip_prefix("segment="))
                    });
                    ctx.packaging_request = Some(match segment_param {
                        Some("init") => {
                            cdn_streaming::packaging::PackagingRequest::InitSegment
                        }
                        Some(n) => match n.parse::<u32>() {
                            Ok(idx) => {
                                cdn_streaming::packaging::PackagingRequest::MediaSegment(idx)
                            }
                            Err(_) => cdn_streaming::packaging::PackagingRequest::Manifest,
                        },
                        None => cdn_streaming::packaging::PackagingRequest::Manifest,
                    });

                    // Packaging wins over Range
                    ctx.range_request = None;
                    ctx.range_passthrough = false;
                }
            }
        }

        Ok(false) // Continue to upstream_peer
    }

    async fn request_body_filter(
        &self,
        session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        if !ctx.body_inspection_enabled || ctx.body_rejected {
            return Ok(());
        }

        if let Some(ref data) = body {
            ctx.body_bytes_received += data.len() as u64;

            // Size enforcement (incremental, catches chunked transfers without Content-Length)
            if ctx.body_max_size > 0 && ctx.body_bytes_received > ctx.body_max_size {
                ctx.body_rejected = true;
                *body = None;
                crate::logging::metrics::BODY_INSPECTION_TOTAL
                    .with_label_values(&[ctx.site_id.as_str(), "size_rejected"])
                    .inc();
                let _ = session.respond_error(413).await;
                return Ok(());
            }

            // Buffer first bytes for magic detection (only if not yet checked)
            if !ctx.body_inspection_checked {
                let needed = 8192_usize.saturating_sub(ctx.body_first_chunk.len());
                if needed > 0 {
                    let take = data.len().min(needed);
                    ctx.body_first_chunk.extend_from_slice(&data[..take]);
                }

                // Check when we have enough bytes or end_of_stream
                if ctx.body_first_chunk.len() >= 8192 || end_of_stream {
                    ctx.body_inspection_checked = true;
                    if let Some(ref site) = ctx.site_config {
                        let declared_ct = session
                            .req_header()
                            .headers
                            .get("content-type")
                            .and_then(|v| v.to_str().ok());
                        let result = cdn_middleware::waf::body::check_magic_bytes(
                            &ctx.body_first_chunk,
                            declared_ct,
                            &site.waf.body_inspection,
                        );
                        match result {
                            cdn_middleware::waf::body::BodyCheckResult::ContentTypeBlocked {
                                ..
                            }
                            | cdn_middleware::waf::body::BodyCheckResult::ContentTypeMismatch {
                                ..
                            } => {
                                ctx.body_rejected = true;
                                *body = None;
                                let label = match result {
                                    cdn_middleware::waf::body::BodyCheckResult::ContentTypeMismatch { .. } => "type_mismatch",
                                    _ => "type_rejected",
                                };
                                crate::logging::metrics::BODY_INSPECTION_TOTAL
                                    .with_label_values(&[ctx.site_id.as_str(), label])
                                    .inc();
                                let _ = session.respond_error(403).await;
                                return Ok(());
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        // Dynamic balancer: health filter → backup fallback → LB algorithm → DNS → HttpPeer
        if let Some(ref site) = ctx.site_config {
            let (peer, origin) = self
                .balancer
                .select_peer(site, ctx.client_ip, &ctx.protocol_type)
                .await?;
            ctx.selected_origin = Some(origin);
            return Ok(peer);
        }

        // Fallback: static load balancer (no site config available)
        let upstream = self
            .lb
            .select(b"", 256)
            .ok_or_else(|| pingora::Error::new(ErrorType::ConnectProxyFailure))?;

        let mut peer = if self.tls {
            HttpPeer::new(upstream, true, self.sni.clone())
        } else {
            HttpPeer::new(upstream, false, String::new())
        };

        // Protocol-specific peer options (fallback path)
        match &ctx.protocol_type {
            ProtocolType::Grpc(_) => {
                peer.options.set_http_version(2, 2);
                peer.options.max_h2_streams = 10;
            }
            ProtocolType::WebSocket | ProtocolType::Sse => {
                peer.options.read_timeout = None;
            }
            ProtocolType::Http => {}
        }

        Ok(Box::new(peer))
    }

    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        // Auto headers
        upstream_request
            .insert_header("X-Request-ID", &ctx.request_id)
            .ok();

        if let Some(ip) = ctx.client_ip {
            let xff = upstream_request
                .headers
                .get("X-Forwarded-For")
                .and_then(|v| v.to_str().ok())
                .map(|v| format!("{}, {}", v, ip))
                .unwrap_or_else(|| ip.to_string());
            upstream_request.insert_header("X-Forwarded-For", &xff).ok();
        }

        upstream_request
            .insert_header("X-Forwarded-Proto", &ctx.scheme)
            .ok();

        // Host header: use per-origin host for dynamic routing, static SNI for fallback
        if let Some(ref origin) = ctx.selected_origin {
            let host = origin.sni.as_deref().unwrap_or(&origin.host);
            upstream_request.insert_header("Host", host).ok();
        } else if !self.sni.is_empty() {
            upstream_request.insert_header("Host", &self.sni).ok();
        }

        // Rewrite upstream path if auth cleaned it (strip auth tokens)
        if let Some(ref cleaned) = ctx.auth_cleaned_path {
            let uri_str = if let Some(q) = upstream_request.uri.query() {
                format!("{}?{}", cleaned, q)
            } else {
                cleaned.clone()
            };
            if let Ok(uri) = uri_str.parse() {
                upstream_request.set_uri(uri);
            }
        }

        // Custom request header rules
        if let Some(ref site) = ctx.site_config {
            if !site.headers.request.is_empty() {
                let ip_str = ctx.client_ip.map(|ip| ip.to_string());
                let vars = cdn_middleware::headers::variables::build_request_variables(
                    ip_str.as_deref(),
                    &ctx.request_id,
                    &ctx.host,
                    &ctx.uri,
                    &ctx.scheme,
                    &ctx.site_id,
                );
                let ops = cdn_middleware::headers::request::apply_request_rules(
                    &site.headers.request,
                    &vars,
                );
                for op in &ops {
                    apply_header_op(upstream_request, op);
                }
            }
        }

        // Protocol-specific upstream request headers
        match &ctx.protocol_type {
            ProtocolType::Sse => {
                // Disable compression for SSE (must stream raw)
                upstream_request
                    .insert_header("Accept-Encoding", "identity")
                    .ok();
                upstream_request
                    .insert_header("Cache-Control", "no-cache")
                    .ok();
                // Transparently forward Last-Event-ID
                if let Some(last_id) = session
                    .req_header()
                    .headers
                    .get("last-event-id")
                    .and_then(|v| v.to_str().ok())
                {
                    upstream_request
                        .insert_header("Last-Event-ID", last_id)
                        .ok();
                }
            }
            ProtocolType::Grpc(_) => {
                // gRPC requires TE: trailers for trailer support
                upstream_request.insert_header("TE", "trailers").ok();
            }
            _ => {}
        }

        // ── Range pass-through ──
        if ctx.range_passthrough && ctx.image_params.is_none() {
            if let Some(range_val) = session
                .req_header()
                .headers
                .get("range")
                .and_then(|v| v.to_str().ok())
            {
                upstream_request
                    .insert_header("Range", range_val)
                    .ok();
            }
        } else if ctx.range_request.is_some() && ctx.image_params.is_some() {
            // Range + image optimization conflict: image wins
            ctx.range_request = None;
            ctx.range_passthrough = false;
            ctx.range_served_from_cache = false;
        }

        Ok(())
    }

    async fn response_filter(
        &self,
        session: &mut Session,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        // Sensitive header removal (always, not configurable)
        upstream_response.remove_header("X-Powered-By");
        upstream_response
            .insert_header("Server", "CDN")
            .ok();

        // Auto headers
        upstream_response
            .insert_header("X-Cache-Status", ctx.cache_status.as_str())
            .ok();
        upstream_response
            .insert_header("X-Request-ID", &ctx.request_id)
            .ok();

        // ── Accept-Ranges advertisement ──
        if matches!(ctx.protocol_type, ProtocolType::Http) && ctx.cache_key.is_some() {
            upstream_response
                .insert_header("Accept-Ranges", "bytes")
                .ok();
        }

        // Custom response header rules
        if let Some(ref site) = ctx.site_config {
            if !site.headers.response.is_empty() {
                let ip_str = ctx.client_ip.map(|ip| ip.to_string());
                let vars = cdn_middleware::headers::variables::build_response_variables(
                    ip_str.as_deref(),
                    &ctx.request_id,
                    &ctx.host,
                    &ctx.uri,
                    &ctx.scheme,
                    &ctx.site_id,
                    ctx.cache_status.as_str(),
                );
                let ops = cdn_middleware::headers::response::apply_response_rules(
                    &site.headers.response,
                    &vars,
                );
                for op in &ops {
                    apply_header_op_response(upstream_response, op);
                }
            }
        }

        // Protocol-specific response headers
        match &ctx.protocol_type {
            ProtocolType::Sse => {
                upstream_response
                    .insert_header("X-Accel-Buffering", "no")
                    .ok();
                upstream_response
                    .insert_header("Cache-Control", "no-cache")
                    .ok();
            }
            _ => {}
        }

        // ── Range response handling ──
        if ctx.range_request.is_some() && ctx.image_params.is_none() {
            let status = upstream_response.status.as_u16();

            if ctx.range_passthrough && status == 206 {
                // Origin returned 206: relay as-is, skip compression
                crate::logging::metrics::RANGE_REQUESTS_TOTAL
                    .with_label_values(&[ctx.site_id.as_str(), "passthrough"])
                    .inc();
            } else if ctx.range_passthrough && status == 200 {
                // Origin returned full 200 despite our Range request.
                // Note Content-Length for body-filter slicing.
                if let Some(cl) = upstream_response
                    .headers
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                {
                    ctx.total_content_length = Some(cl);
                }
            }
        }

        // ── Dynamic packaging response setup ──
        if ctx.packaging_request.is_some() && ctx.image_params.is_none() {
            let content_type = upstream_response
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");

            if content_type.contains("video/mp4") || content_type.contains("application/mp4") {
                // Check size limit
                let size_ok = upstream_response
                    .headers
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(|len| {
                        let max = ctx
                            .site_config
                            .as_ref()
                            .map(|s| s.streaming.dynamic_packaging.max_mp4_size)
                            .unwrap_or(2 * 1024 * 1024 * 1024);
                        len <= max
                    })
                    .unwrap_or(true);

                if size_ok {
                    ctx.packaging_buffering = true;
                    match &ctx.packaging_request {
                        Some(cdn_streaming::packaging::PackagingRequest::Manifest) => {
                            upstream_response
                                .insert_header("Content-Type", "application/vnd.apple.mpegurl")
                                .ok();
                        }
                        Some(cdn_streaming::packaging::PackagingRequest::InitSegment)
                        | Some(cdn_streaming::packaging::PackagingRequest::MediaSegment(_)) => {
                            upstream_response
                                .insert_header("Content-Type", "video/mp4")
                                .ok();
                        }
                        None => {}
                    }
                    upstream_response.remove_header("Content-Length");
                } else {
                    ctx.packaging_request = None;
                }
            } else {
                ctx.packaging_request = None;
            }
        }

        // ── Prefetch manifest detection ──
        if let Some(ref site) = ctx.site_config {
            if site.streaming.prefetch.enabled
                && !ctx.packaging_buffering
                && !ctx.image_buffering
            {
                let content_type = upstream_response
                    .headers
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");

                if content_type.contains("application/vnd.apple.mpegurl")
                    || content_type.contains("audio/mpegurl")
                {
                    ctx.prefetch_manifest_type =
                        Some(crate::context::ManifestType::Hls);
                    ctx.prefetch_buffering = true;
                } else if content_type.contains("application/dash+xml") {
                    ctx.prefetch_manifest_type =
                        Some(crate::context::ManifestType::Dash);
                    ctx.prefetch_buffering = true;
                }
            }
        }

        // ── Image optimization setup ──
        // Must run BEFORE compression — image path skips compression entirely.
        if ctx.image_params.is_some() && matches!(ctx.protocol_type, ProtocolType::Http) {
            let content_type = upstream_response
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");

            // Get effective image config: site > global default
            let img_config = ctx
                .site_config
                .as_ref()
                .filter(|s| s.image_optimization.enabled)
                .map(|s| &s.image_optimization)
                .or_else(|| {
                    if self.default_image_optimization.enabled {
                        Some(&self.default_image_optimization)
                    } else {
                        None
                    }
                });

            if let Some(config) = img_config {
                if cdn_image::is_optimizable_image(content_type, config) {
                    // Check input size limit via Content-Length
                    let size_ok = upstream_response
                        .headers
                        .get("content-length")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(|len| len <= config.max_input_size)
                        .unwrap_or(true); // unknown size = try

                    if size_ok {
                        let accept = session
                            .req_header()
                            .headers
                            .get("accept")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("");

                        let explicit_fmt = ctx
                            .image_params
                            .as_ref()
                            .and_then(|p| p.format.as_ref());

                        let (output_format, auto_negotiated) =
                            cdn_image::negotiate_format(accept, explicit_fmt, config, content_type);

                        // Set response headers for the output format
                        upstream_response
                            .insert_header("Content-Type", output_format.content_type())
                            .ok();
                        upstream_response.remove_header("Content-Length");

                        if auto_negotiated {
                            upstream_response
                                .insert_header("Vary", "Accept")
                                .ok();
                            ctx.image_auto_negotiated = true;
                        }

                        ctx.image_output_format = Some(output_format);
                        ctx.image_buffering = true;
                    } else {
                        // Image too large, clear params to pass through
                        ctx.image_params = None;
                    }
                } else {
                    // Not an optimizable image, clear params
                    ctx.image_params = None;
                }
            } else {
                ctx.image_params = None;
            }
        }

        // ── Response compression setup ──
        // Skip for non-HTTP protocols (WebSocket/SSE/gRPC)
        // Skip if image buffering is active (images are already compressed)
        // Skip if Range request is active (byte offsets refer to uncompressed content)
        // Skip if dynamic packaging is active (video segments should not be compressed)
        if !ctx.image_buffering
            && !ctx.packaging_buffering
            && ctx.range_request.is_none()
            && matches!(ctx.protocol_type, ProtocolType::Http)
        {
            // Skip if upstream already compressed
            let already_encoded = upstream_response
                .headers
                .get("content-encoding")
                .is_some();

            if !already_encoded {
                // Get effective compression config: site > global default
                let comp_config = ctx
                    .site_config
                    .as_ref()
                    .filter(|s| s.compression.enabled)
                    .map(|s| &s.compression)
                    .or_else(|| {
                        if self.default_compression.enabled {
                            Some(&self.default_compression)
                        } else {
                            None
                        }
                    });

                if let Some(config) = comp_config {
                    // Check response status (skip 204/304)
                    let status = upstream_response.status.as_u16();
                    let has_body = status != 204 && status != 304;

                    // Check Content-Length against min_size
                    let size_ok = upstream_response
                        .headers
                        .get("content-length")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(|len| len >= config.min_size)
                        .unwrap_or(true); // unknown size = try to compress

                    // Check Content-Type
                    let type_ok = upstream_response
                        .headers
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .map(|ct| crate::compression::is_compressible(ct, config))
                        .unwrap_or(false);

                    if has_body && size_ok && type_ok {
                        // Negotiate with client
                        let accept = session
                            .req_header()
                            .headers
                            .get("accept-encoding")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("");

                        if let Some(algo) =
                            crate::compression::negotiate(accept, config)
                        {
                            upstream_response
                                .insert_header("Content-Encoding", algo.encoding_token())
                                .ok();
                            upstream_response.remove_header("Content-Length");
                            // Add Vary: Accept-Encoding
                            if upstream_response.headers.get("vary").is_none() {
                                upstream_response
                                    .insert_header("Vary", "Accept-Encoding")
                                    .ok();
                            }
                            ctx.compression_algorithm = Some(algo.clone());
                            ctx.compressor = Some(crate::compression::Encoder::new(
                                &algo,
                                config.level,
                            ));
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn response_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<Option<std::time::Duration>>
    where
        Self::CTX: Send + Sync,
    {
        // ── Range slice from origin 200 path ──
        // Origin returned full 200 but client wanted Range. Buffer full body,
        // then slice on end_of_stream.
        if ctx.range_request.is_some()
            && !ctx.image_buffering
            && ctx.total_content_length.is_some()
            && ctx.image_params.is_none()
        {
            if let Some(data) = body.take() {
                ctx.range_body_buffer.extend_from_slice(&data);
                *body = Some(Bytes::new());
            }
            if end_of_stream {
                let full_body = std::mem::take(&mut ctx.range_body_buffer);
                let total = full_body.len() as u64;
                if let Some(ref spec) = ctx.range_request {
                    match crate::range::resolve_range(spec, total) {
                        Ok((start, end)) => {
                            let sliced = crate::range::slice_body(&full_body, start, end);
                            log::debug!(
                                "[Range] sliced origin 200: bytes {}-{}/{} ({} bytes)",
                                start, end, total, sliced.len()
                            );
                            *body = Some(Bytes::from(sliced));
                        }
                        Err(_) => {
                            log::debug!("[Range] not satisfiable, serving full body");
                            *body = Some(Bytes::from(full_body));
                        }
                    }
                } else {
                    *body = Some(Bytes::from(full_body));
                }
            }
            return Ok(None);
        }

        // ── Dynamic packaging path ──
        if ctx.packaging_buffering {
            if let Some(data) = body.take() {
                ctx.packaging_buffer.extend_from_slice(&data);
                *body = Some(Bytes::new());
            }
            if end_of_stream {
                ctx.packaging_buffering = false;
                let mp4_data = std::mem::take(&mut ctx.packaging_buffer);
                let segment_duration = ctx
                    .site_config
                    .as_ref()
                    .map(|s| s.streaming.dynamic_packaging.segment_duration)
                    .unwrap_or(6.0);

                let start = std::time::Instant::now();
                match cdn_streaming::packaging::process_packaging_request(
                    &mp4_data,
                    ctx.packaging_request.as_ref().unwrap(),
                    segment_duration,
                    &ctx.uri,
                    _session.req_header().uri.query(),
                ) {
                    Ok(output) => {
                        let elapsed = start.elapsed().as_secs_f64();
                        let pkg_type = match &ctx.packaging_request {
                            Some(cdn_streaming::packaging::PackagingRequest::Manifest) => {
                                "manifest"
                            }
                            Some(cdn_streaming::packaging::PackagingRequest::InitSegment) => {
                                "init"
                            }
                            Some(cdn_streaming::packaging::PackagingRequest::MediaSegment(_)) => {
                                "segment"
                            }
                            None => "unknown",
                        };
                        crate::logging::metrics::PACKAGING_TOTAL
                            .with_label_values(&[ctx.site_id.as_str(), "success"])
                            .inc();
                        crate::logging::metrics::PACKAGING_DURATION
                            .with_label_values(&[ctx.site_id.as_str(), pkg_type])
                            .observe(elapsed);
                        log::debug!(
                            "[Packaging] {} generated: {} bytes in {:.3}s",
                            pkg_type,
                            output.len(),
                            elapsed
                        );
                        *body = Some(Bytes::from(output));
                    }
                    Err(e) => {
                        log::warn!("[Packaging] transmux failed: {}, passing through", e);
                        crate::logging::metrics::PACKAGING_TOTAL
                            .with_label_values(&[ctx.site_id.as_str(), "error"])
                            .inc();
                        *body = Some(Bytes::from(mp4_data));
                    }
                }
            }
            return Ok(None);
        }

        // ── Prefetch manifest intercept (shadow copy, don't consume body) ──
        if ctx.prefetch_buffering {
            if let Some(ref data) = body {
                ctx.prefetch_body_buffer.extend_from_slice(data);
            }
            if end_of_stream {
                ctx.prefetch_buffering = false;
                let manifest = std::mem::take(&mut ctx.prefetch_body_buffer);
                if let Ok(manifest_str) = std::str::from_utf8(&manifest) {
                    let base_url = format!("{}://{}{}", ctx.scheme, ctx.host, ctx.uri);
                    let segments = match ctx.prefetch_manifest_type {
                        Some(crate::context::ManifestType::Hls) => {
                            cdn_streaming::prefetch::extract_hls_segments(
                                manifest_str,
                                &base_url,
                            )
                        }
                        Some(crate::context::ManifestType::Dash) => {
                            cdn_streaming::prefetch::extract_dash_segments(
                                manifest_str,
                                &base_url,
                            )
                        }
                        None => vec![],
                    };
                    if !segments.is_empty() {
                        if let (Some(ref site), Some(ref origin)) =
                            (&ctx.site_config, &ctx.selected_origin)
                        {
                            self.prefetch_worker.prefetch_segments(
                                ctx.site_id.clone(),
                                Arc::clone(site),
                                origin.clone(),
                                segments,
                                ctx.host.clone(),
                            );
                        }
                    }
                }
                // Body is NOT consumed — passes through to client
            }
        }

        // ── Image optimization path (mutually exclusive with compression) ──
        if ctx.image_buffering {
            // Accumulate chunks into buffer
            if let Some(data) = body.take() {
                ctx.image_buffer.extend_from_slice(&data);
                *body = Some(Bytes::new()); // emit empty for intermediate chunks
            }
            if end_of_stream {
                ctx.image_buffering = false;
                if let (Some(ref params), Some(ref output_format)) =
                    (&ctx.image_params, &ctx.image_output_format)
                {
                    let buffer = std::mem::take(&mut ctx.image_buffer);

                    let max_size = ctx
                        .site_config
                        .as_ref()
                        .map(|s| s.image_optimization.max_input_size)
                        .unwrap_or(50 * 1024 * 1024);

                    if buffer.len() as u64 > max_size {
                        log::warn!(
                            "[Image] input too large: {} bytes, passing through",
                            buffer.len()
                        );
                        *body = Some(Bytes::from(buffer));
                    } else {
                        let start = std::time::Instant::now();
                        match cdn_image::process_image(&buffer, params, output_format) {
                            Ok(processed) => {
                                let elapsed = start.elapsed().as_secs_f64();
                                let input_len = buffer.len();
                                let output_len = processed.len();
                                log::debug!(
                                    "[Image] processed: {} -> {} bytes, format={:?}, {:.3}s",
                                    input_len,
                                    output_len,
                                    output_format,
                                    elapsed,
                                );
                                crate::logging::metrics::IMAGE_OPTIMIZATIONS_TOTAL
                                    .with_label_values(&[
                                        ctx.site_id.as_str(),
                                        output_format.content_type(),
                                        "success",
                                    ])
                                    .inc();
                                crate::logging::metrics::IMAGE_OPTIMIZATION_DURATION
                                    .with_label_values(&[ctx.site_id.as_str()])
                                    .observe(elapsed);
                                if input_len > 0 {
                                    let ratio = output_len as f64 / input_len as f64;
                                    crate::logging::metrics::IMAGE_OPTIMIZATION_SIZE_RATIO
                                        .with_label_values(&[ctx.site_id.as_str()])
                                        .observe(ratio);
                                }
                                *body = Some(Bytes::from(processed));
                            }
                            Err(e) => {
                                log::warn!(
                                    "[Image] processing failed: {}, passing through",
                                    e
                                );
                                crate::logging::metrics::IMAGE_OPTIMIZATIONS_TOTAL
                                    .with_label_values(&[
                                        ctx.site_id.as_str(),
                                        output_format.content_type(),
                                        "error",
                                    ])
                                    .inc();
                                *body = Some(Bytes::from(buffer));
                            }
                        }
                    }
                }
            }
            return Ok(None);
        }

        // ── Compression path ──
        if let Some(ref mut encoder) = ctx.compressor {
            if let Some(data) = body.take() {
                let compressed = encoder.write_chunk(&data);
                if !compressed.is_empty() {
                    *body = Some(Bytes::from(compressed));
                } else {
                    *body = Some(Bytes::new());
                }
            }
            if end_of_stream {
                let encoder = ctx.compressor.take().unwrap();
                let final_bytes = encoder.finish();
                if !final_bytes.is_empty() {
                    match body {
                        Some(existing) if !existing.is_empty() => {
                            let mut combined = existing.to_vec();
                            combined.extend_from_slice(&final_bytes);
                            *body = Some(Bytes::from(combined));
                        }
                        _ => {
                            *body = Some(Bytes::from(final_bytes));
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    async fn logging(
        &self,
        session: &mut Session,
        _e: Option<&pingora::Error>,
        ctx: &mut Self::CTX,
    ) {
        let status = session
            .response_written()
            .map(|r| r.status.as_u16())
            .unwrap_or(0);
        let method = session.req_header().method.as_str();
        let path = session.req_header().uri.path();

        log::info!(
            "{} {} -> {} | site={} cache={} proto={:?}",
            method,
            path,
            status,
            ctx.site_id,
            ctx.cache_status.as_str(),
            ctx.protocol_type,
        );

        // Passive health check
        if let Some(ref origin) = ctx.selected_origin {
            if status >= 500 || status == 0 {
                self.balancer.health.record_failure(&ctx.site_id, &origin.id);
            } else {
                self.balancer.health.record_success(&ctx.site_id, &origin.id);
            }
        }

        // Prometheus metrics
        let response_size = session
            .response_written()
            .and_then(|r| {
                r.headers
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
            })
            .unwrap_or(0);

        crate::logging::metrics::record_request(
            &ctx.site_id,
            method,
            status,
            ctx.cache_status.as_str(),
            response_size,
            0.0, // TODO: track actual duration with Instant in ctx
            ctx.selected_origin.as_ref().map(|o| o.id.as_str()),
        );

        // Async log push
        crate::logging::queue::push_log_entry(crate::logging::queue::LogEntry {
            timestamp: chrono::Utc::now().to_rfc3339(),
            request_id: ctx.request_id.clone(),
            method: method.to_string(),
            host: ctx.host.clone(),
            path: path.to_string(),
            query_string: session.req_header().uri.query().map(|s| s.to_string()),
            scheme: ctx.scheme.clone(),
            protocol: format!("{:?}", ctx.protocol_type),
            client_ip: ctx.client_ip,
            country_code: ctx.geo_info.as_ref().and_then(|g| g.country_code.clone()),
            asn: ctx.geo_info.as_ref().and_then(|g| g.asn),
            status,
            response_size,
            duration_ms: 0.0, // TODO: track actual duration
            site_id: ctx.site_id.clone(),
            cache_status: ctx.cache_status.as_str().to_string(),
            cache_key: ctx.cache_key.clone(),
            origin_id: ctx.selected_origin.as_ref().map(|o| o.id.clone()),
            origin_host: ctx.selected_origin.as_ref().map(|o| o.host.clone()),
            waf_blocked: ctx.waf_blocked,
            waf_reason: ctx.waf_reason.clone(),
            cc_blocked: ctx.cc_blocked,
            cc_reason: ctx.cc_reason.clone(),
            range_request: ctx.range_request.is_some(),
            packaging_request: ctx.packaging_request.is_some(),
            auth_validated: ctx.auth_cleaned_path.is_some(),
            body_rejected: ctx.body_rejected,
            node_id: String::new(), // TODO: inject from NodeConfig
        });
    }

    fn fail_to_connect(
        &self,
        _session: &mut Session,
        _peer: &HttpPeer,
        ctx: &mut Self::CTX,
        e: Box<pingora::Error>,
    ) -> Box<pingora::Error> {
        let max_retries = ctx
            .site_config
            .as_ref()
            .map(|s| s.load_balancer.retries)
            .unwrap_or(2);

        ctx.balancer_tried += 1;

        // Record failure for passive health check
        if let Some(ref origin) = ctx.selected_origin {
            self.balancer.health.record_failure(&ctx.site_id, &origin.id);
        }

        if ctx.balancer_tried < max_retries {
            log::warn!(
                "[Balancer] connect failed, retrying ({}/{}): {}",
                ctx.balancer_tried, max_retries, e
            );
            let mut e = e;
            e.set_retry(true);
            return e;
        }

        log::error!(
            "[Balancer] connect failed, no more retries ({}/{}): {}",
            ctx.balancer_tried, max_retries, e
        );
        e
    }
}

// ── Helper methods ──

impl CdnProxy {
    async fn serve_health(&self, session: &mut Session) -> Result<bool> {
        let mut header = ResponseHeader::build(200, None)?;
        header.insert_header("Content-Type", "text/plain")?;
        session.write_response_header(Box::new(header), false).await?;
        session.write_response_body(Some("OK\n".into()), true).await?;
        Ok(true)
    }

    async fn serve_bad_request(&self, session: &mut Session, reason: &str) -> Result<bool> {
        let body = format!("Bad Request: {}\n", reason);
        let mut header = ResponseHeader::build(400, None)?;
        header.insert_header("Content-Type", "text/plain")?;
        session.write_response_header(Box::new(header), false).await?;
        session.write_response_body(Some(body.into()), true).await?;
        Ok(true)
    }

    async fn serve_acme_challenge(&self, session: &mut Session, key_auth: &str) -> Result<bool> {
        let mut header = ResponseHeader::build(200, None)?;
        header.insert_header("Content-Type", "text/plain")?;
        header.insert_header("Content-Length", &key_auth.len().to_string())?;
        session.write_response_header(Box::new(header), false).await?;
        session.write_response_body(Some(key_auth.to_string().into()), true).await?;
        Ok(true)
    }

    async fn serve_health_detail(&self, session: &mut Session) -> Result<bool> {
        let redis_ok = self.redis_pool.ping().await;
        let detail = serde_json::json!({
            "status": if redis_ok || !self.redis_pool.is_available() { "ok" } else { "degraded" },
            "sites_loaded": self.live_config.load().sites.len(),
            "redis": {
                "available": self.redis_pool.is_available(),
                "connected": redis_ok,
                "description": self.redis_pool.describe(),
            },
        });
        let body = serde_json::to_string_pretty(&detail).unwrap_or_default();
        let mut header = ResponseHeader::build(200, None)?;
        header.insert_header("Content-Type", "application/json")?;
        session.write_response_header(Box::new(header), false).await?;
        session.write_response_body(Some(body.into()), true).await?;
        Ok(true)
    }

    async fn serve_status(&self, session: &mut Session) -> Result<bool> {
        let status = serde_json::json!({
            "node": "nozdormu",
            "version": env!("CARGO_PKG_VERSION"),
            "sites_loaded": self.live_config.load().sites.len(),
        });
        let body = serde_json::to_string_pretty(&status).unwrap_or_default();
        let mut header = ResponseHeader::build(200, None)?;
        header.insert_header("Content-Type", "application/json")?;
        session.write_response_header(Box::new(header), false).await?;
        session.write_response_body(Some(body.into()), true).await?;
        Ok(true)
    }

    async fn serve_not_found(&self, session: &mut Session) -> Result<bool> {
        let mut header = ResponseHeader::build(404, None)?;
        header.insert_header("Content-Type", "text/plain")?;
        session.write_response_header(Box::new(header), false).await?;
        session.write_response_body(Some("Not Found\n".into()), true).await?;
        Ok(true)
    }

    async fn serve_forbidden(&self, session: &mut Session) -> Result<bool> {
        let mut header = ResponseHeader::build(403, None)?;
        header.insert_header("Content-Type", "text/plain")?;
        session.write_response_header(Box::new(header), false).await?;
        session.write_response_body(Some("Forbidden\n".into()), true).await?;
        Ok(true)
    }

    async fn serve_payload_too_large(
        &self,
        session: &mut Session,
        limit: u64,
        actual: Option<u64>,
    ) -> Result<bool> {
        let body = match actual {
            Some(a) => format!(
                "Payload Too Large: {} bytes exceeds limit of {} bytes\n",
                a, limit
            ),
            None => format!("Payload Too Large: limit is {} bytes\n", limit),
        };
        let mut header = ResponseHeader::build(413, None)?;
        header.insert_header("Content-Type", "text/plain")?;
        session
            .write_response_header(Box::new(header), false)
            .await?;
        session
            .write_response_body(Some(body.into()), true)
            .await?;
        Ok(true)
    }

    async fn serve_too_many_requests(
        &self,
        session: &mut Session,
        retry_after: u64,
    ) -> Result<bool> {
        let mut header = ResponseHeader::build(429, None)?;
        header.insert_header("Content-Type", "text/plain")?;
        header.insert_header("Retry-After", &retry_after.to_string())?;
        session.write_response_header(Box::new(header), false).await?;
        session
            .write_response_body(Some("Too Many Requests\n".into()), true)
            .await?;
        Ok(true)
    }

    async fn serve_challenge(
        &self,
        session: &mut Session,
        cookie_value: &str,
    ) -> Result<bool> {
        use cdn_middleware::cc::action::ChallengeManager;
        let html = ChallengeManager::challenge_html(cookie_value);
        let mut header = ResponseHeader::build(503, None)?;
        header.insert_header("Content-Type", "text/html; charset=utf-8")?;
        header.insert_header("Content-Length", &html.len().to_string())?;
        header.insert_header("Cache-Control", "no-store")?;
        session.write_response_header(Box::new(header), false).await?;
        session
            .write_response_body(Some(html.into()), true)
            .await?;
        Ok(true)
    }

    async fn serve_redirect(
        &self,
        session: &mut Session,
        location: &str,
        status_code: u16,
        extra_headers: std::collections::HashMap<String, String>,
        cache_control: Option<&str>,
    ) -> Result<bool> {
        let mut header = ResponseHeader::build(status_code, None)?;
        header.insert_header("Location", location)?;
        header.insert_header("Content-Length", "0")?;
        if let Some(cc) = cache_control {
            header.insert_header("Cache-Control", cc)?;
        }
        for (name, value) in &extra_headers {
            if let (Ok(hn), Ok(hv)) = (
                http::header::HeaderName::from_bytes(name.as_bytes()),
                http::header::HeaderValue::from_str(value),
            ) {
                header.headers.insert(hn, hv);
            }
        }
        session.write_response_header(Box::new(header), false).await?;
        session.write_response_body(Some("".into()), true).await?;
        Ok(true)
    }

    #[allow(dead_code)]
    async fn serve_range_not_satisfiable(
        &self,
        session: &mut Session,
        total_size: u64,
    ) -> Result<bool> {
        let mut header = ResponseHeader::build(416, None)?;
        header.insert_header("Content-Type", "text/plain")?;
        header.insert_header(
            "Content-Range",
            crate::range::content_range_unsatisfied(total_size),
        )?;
        header.insert_header("Accept-Ranges", "bytes")?;
        session.write_response_header(Box::new(header), false).await?;
        session
            .write_response_body(Some("Range Not Satisfiable\n".into()), true)
            .await?;
        Ok(true)
    }

    #[allow(dead_code)]
    async fn serve_partial_content(
        &self,
        session: &mut Session,
        body: Vec<u8>,
        start: u64,
        end: u64,
        total: u64,
        original_headers: &std::collections::HashMap<String, String>,
    ) -> Result<bool> {
        let mut header = ResponseHeader::build(206, None)?;
        header.insert_header(
            "Content-Range",
            crate::range::content_range_header(start, end, total),
        )?;
        header.insert_header("Content-Length", (end - start + 1).to_string())?;
        header.insert_header("Accept-Ranges", "bytes")?;
        // Copy relevant headers from cached meta
        for (name, value) in original_headers {
            if name.eq_ignore_ascii_case("content-type")
                || name.eq_ignore_ascii_case("etag")
                || name.eq_ignore_ascii_case("last-modified")
                || name.eq_ignore_ascii_case("cache-control")
            {
                let n = name.clone();
                let v = value.clone();
                header.insert_header(n, v).ok();
            }
        }
        session.write_response_header(Box::new(header), false).await?;
        session.write_response_body(Some(body.into()), true).await?;
        Ok(true)
    }
}

/// Apply a header operation to a Pingora header (RequestHeader or ResponseHeader).
///
/// IMPORTANT: Must use `insert_header()` / `remove_header()` methods instead of
/// direct `headers.insert()` — Pingora's ResponseHeader maintains a `header_name_map`
/// for original-case serialization that panics if out of sync with the HeaderMap.
macro_rules! impl_apply_header_op {
    ($fn_name:ident, $header_type:ty) => {
        fn $fn_name(
            header: &mut $header_type,
            op: &cdn_middleware::headers::request::HeaderOp,
        ) {
            use cdn_common::HeaderAction;
            let name = op.name.clone();
            match &op.action {
                HeaderAction::Set => {
                    if let Some(ref value) = op.value {
                        let v = value.clone();
                        header.insert_header(name, v).ok();
                    }
                }
                HeaderAction::Add => {
                    if header.headers.get(op.name.as_str()).is_none() {
                        if let Some(ref value) = op.value {
                            let v = value.clone();
                            header.insert_header(name, v).ok();
                        }
                    }
                }
                HeaderAction::Remove => {
                    header.remove_header(op.name.as_str());
                }
                HeaderAction::Append => {
                    if let Some(ref value) = op.value {
                        let new_val = header
                            .headers
                            .get(op.name.as_str())
                            .and_then(|v| v.to_str().ok())
                            .map(|v| format!("{}, {}", v, value))
                            .unwrap_or_else(|| value.clone());
                        header.insert_header(name, new_val).ok();
                    }
                }
            }
        }
    };
}

impl_apply_header_op!(apply_header_op, RequestHeader);
impl_apply_header_op!(apply_header_op_response, ResponseHeader);
