use crate::context::{GrpcVariant, ProtocolType};
use cdn_common::ProtocolConfig;
use pingora::prelude::*;

/// Detect the protocol type from request headers.
///
/// Priority: gRPC > WebSocket > SSE > HTTP
/// Each protocol must be enabled in the site's ProtocolConfig.
pub fn detect_protocol(session: &Session, config: &ProtocolConfig) -> ProtocolType {
    let headers = &session.req_header().headers;

    // ── 1. gRPC detection (highest priority) ──
    if config.grpc.enabled {
        if let Some(ct) = headers.get("content-type").and_then(|v| v.to_str().ok()) {
            let ct_lower = ct.to_ascii_lowercase();
            if ct_lower.starts_with("application/grpc-web-text") {
                return ProtocolType::Grpc(GrpcVariant::WebText);
            }
            if ct_lower.starts_with("application/grpc-web") {
                return ProtocolType::Grpc(GrpcVariant::Web);
            }
            if ct_lower.starts_with("application/grpc") {
                return ProtocolType::Grpc(GrpcVariant::Native);
            }
        }
    }

    // ── 2. WebSocket detection ──
    if config.websocket.enable && is_websocket_upgrade(session) {
        return ProtocolType::WebSocket;
    }

    // ── 3. SSE detection ──
    if config.sse.enable {
        if let Some(accept) = headers.get("accept").and_then(|v| v.to_str().ok()) {
            if accept.contains("text/event-stream") {
                return ProtocolType::Sse;
            }
        }
    }

    ProtocolType::Http
}

/// Validate WebSocket upgrade request per RFC 6455.
///
/// Checks:
/// - Method is GET
/// - HTTP version >= 1.1
/// - Upgrade header contains "websocket" (case-insensitive)
/// - Connection header contains "upgrade" (case-insensitive)
/// - Sec-WebSocket-Key is present
/// - Sec-WebSocket-Version is "13"
pub fn validate_websocket(session: &Session) -> Result<(), &'static str> {
    let req = session.req_header();

    if req.method != http::Method::GET {
        return Err("WebSocket requires GET method");
    }

    // Check Upgrade header
    let upgrade = req
        .headers
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !upgrade.eq_ignore_ascii_case("websocket") {
        return Err("missing or invalid Upgrade header");
    }

    // Check Connection header contains "upgrade"
    let connection = req
        .headers
        .get("connection")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let has_upgrade = connection
        .split(',')
        .any(|part| part.trim().eq_ignore_ascii_case("upgrade"));
    if !has_upgrade {
        return Err("Connection header must contain 'upgrade'");
    }

    // Check Sec-WebSocket-Key
    if req.headers.get("sec-websocket-key").is_none() {
        return Err("missing Sec-WebSocket-Key");
    }

    // Check Sec-WebSocket-Version
    let version = req
        .headers
        .get("sec-websocket-version")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if version != "13" {
        return Err("Sec-WebSocket-Version must be 13");
    }

    Ok(())
}

/// Check if the request is a WebSocket upgrade (quick check, not full validation).
fn is_websocket_upgrade(session: &Session) -> bool {
    let headers = &session.req_header().headers;

    let upgrade = headers
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let connection = headers
        .get("connection")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    upgrade.eq_ignore_ascii_case("websocket")
        && connection
            .split(',')
            .any(|part| part.trim().eq_ignore_ascii_case("upgrade"))
}

/// Check if a gRPC request path is in the service whitelist.
/// Path format: /package.Service/Method
/// Returns true if whitelist is empty (all services allowed) or path matches.
pub fn check_grpc_service_whitelist(path: &str, whitelist: &[String]) -> bool {
    if whitelist.is_empty() {
        return true; // No whitelist = all services allowed
    }

    // Extract service name from path: /package.Service/Method → package.Service
    let service = path
        .strip_prefix('/')
        .and_then(|p| p.split('/').next())
        .unwrap_or("");

    whitelist.iter().any(|allowed| allowed == service)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grpc_whitelist_empty_allows_all() {
        assert!(check_grpc_service_whitelist("/my.Service/Method", &[]));
    }

    #[test]
    fn test_grpc_whitelist_match() {
        let whitelist = vec!["my.Service".to_string()];
        assert!(check_grpc_service_whitelist(
            "/my.Service/Method",
            &whitelist
        ));
    }

    #[test]
    fn test_grpc_whitelist_no_match() {
        let whitelist = vec!["other.Service".to_string()];
        assert!(!check_grpc_service_whitelist(
            "/my.Service/Method",
            &whitelist
        ));
    }

    #[test]
    fn test_grpc_whitelist_multiple() {
        let whitelist = vec!["svc.A".to_string(), "svc.B".to_string()];
        assert!(check_grpc_service_whitelist("/svc.A/Do", &whitelist));
        assert!(check_grpc_service_whitelist("/svc.B/Do", &whitelist));
        assert!(!check_grpc_service_whitelist("/svc.C/Do", &whitelist));
    }
}
