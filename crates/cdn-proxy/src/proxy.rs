use crate::context::{CacheStatus, ProxyCtx, ProtocolType};
use arc_swap::ArcSwap;
use async_trait::async_trait;
use cdn_config::LiveConfig;
use pingora::http::ResponseHeader;
use pingora::prelude::*;
use std::sync::Arc;

pub struct CdnProxy {
    pub lb: Arc<LoadBalancer<RoundRobin>>,
    pub sni: String,
    pub tls: bool,
    pub live_config: Arc<ArcSwap<LiveConfig>>,
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
            // TODO: Phase 9 — serve ACME challenge from etcd
            return self.serve_not_found(session).await;
        }

        // ── 3. Internal endpoints (IP restricted) ──
        if matches!(path, "/health/detail" | "/status") {
            let remote = session
                .client_addr()
                .and_then(|a| a.as_inet())
                .map(|a| a.ip());
            if !remote.map(|ip| is_private_ip(ip)).unwrap_or(false) {
                return self.serve_forbidden(session).await;
            }
            // TODO: Phase 12 — serve /health/detail and /status
            return self.serve_not_found(session).await;
        }

        // ── 4. Client IP extraction ──
        ctx.client_ip = session
            .client_addr()
            .and_then(|a| a.as_inet())
            .map(|a| a.ip());
        // TODO: Phase 12 — XFF anti-spoofing (utils/ip.rs)

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
            .unwrap_or("");

        let config = self.live_config.load();
        match config.match_site(host) {
            Some(site) => {
                ctx.site_id = site.site_id.clone();
                ctx.site_config = Some(site);
            }
            None => {
                log::warn!("[Access] site not found: {}", host);
                return self.serve_not_found(session).await;
            }
        }

        // ── 6. WAF check ──
        // TODO: Phase 3

        // ── 7. CC check ──
        // TODO: Phase 4

        // ── 8. Redirect check ──
        // TODO: Phase 5

        // ── 9. Protocol detection ──
        // TODO: Phase 6

        // ── 10. Cache lookup ──
        // TODO: Phase 8

        Ok(false) // Continue to upstream_peer
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        // TODO: Phase 7 — full LB implementation
        // Temporary: use static load balancer from config
        let upstream = self
            .lb
            .select(b"", 256)
            .ok_or_else(|| pingora::Error::new(ErrorType::ConnectProxyFailure))?;

        let peer = if self.tls {
            HttpPeer::new(upstream, true, self.sni.clone())
        } else {
            HttpPeer::new(upstream, false, String::new())
        };

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

        let scheme = session
            .req_header()
            .uri
            .scheme_str()
            .unwrap_or("http");
        upstream_request
            .insert_header("X-Forwarded-Proto", scheme)
            .ok();

        // Host header
        if !self.sni.is_empty() {
            upstream_request.insert_header("Host", &self.sni).ok();
        }

        // TODO: Phase 10 — custom header rules + variable substitution

        Ok(())
    }

    async fn response_filter(
        &self,
        _session: &mut Session,
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

        // TODO: Phase 10 — custom response header rules

        Ok(())
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

        // TODO: Phase 7 — passive health check
        // TODO: Phase 11 — Prometheus metrics + Redis Streams log
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
}

/// Check if an IP is a private/internal address.
fn is_private_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
        }
    }
}
