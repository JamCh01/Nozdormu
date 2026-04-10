use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

static VAR_PATTERN: OnceLock<Regex> = OnceLock::new();

fn var_regex() -> &'static Regex {
    VAR_PATTERN.get_or_init(|| Regex::new(r"\$\{(\w+)\}").unwrap())
}

/// Variable context for header value substitution.
/// Contains all available variables for ${variable} replacement.
pub struct VariableContext {
    vars: HashMap<String, String>,
}

impl VariableContext {
    pub fn new() -> Self {
        Self {
            vars: HashMap::new(),
        }
    }

    pub fn set(&mut self, key: &str, value: String) {
        self.vars.insert(key.to_string(), value);
    }

    /// Substitute all ${variable} patterns in the input string.
    /// Uses single-pass regex replacement to prevent second-order injection
    /// (a variable's value cannot be re-interpreted as another variable).
    pub fn substitute(&self, input: &str) -> String {
        let re = var_regex();
        re.replace_all(input, |caps: &regex::Captures| {
            let var_name = &caps[1];
            match self.vars.get(var_name) {
                Some(value) => value.clone(),
                None => caps[0].to_string(), // Leave unknown variables as-is
            }
        })
        .into_owned()
    }
}

/// Build a request-phase variable context.
pub fn build_request_variables(
    client_ip: Option<&str>,
    request_id: &str,
    host: &str,
    uri: &str,
    scheme: &str,
    site_id: &str,
) -> VariableContext {
    let mut ctx = VariableContext::new();
    if let Some(ip) = client_ip {
        ctx.set("client_ip", ip.to_string());
    }
    ctx.set("request_id", request_id.to_string());
    ctx.set("host", host.to_string());
    ctx.set("uri", uri.to_string());
    ctx.set("scheme", scheme.to_string());
    ctx.set("site_id", site_id.to_string());
    ctx
}

/// Build a response-phase variable context (extends request variables).
pub fn build_response_variables(
    client_ip: Option<&str>,
    request_id: &str,
    host: &str,
    uri: &str,
    scheme: &str,
    site_id: &str,
    cache_status: &str,
) -> VariableContext {
    let mut ctx = build_request_variables(client_ip, request_id, host, uri, scheme, site_id);
    ctx.set("cache_status", cache_status.to_string());
    ctx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_substitute_single() {
        let mut ctx = VariableContext::new();
        ctx.set("host", "example.com".to_string());
        assert_eq!(ctx.substitute("Host: ${host}"), "Host: example.com");
    }

    #[test]
    fn test_substitute_multiple() {
        let mut ctx = VariableContext::new();
        ctx.set("scheme", "https".to_string());
        ctx.set("host", "example.com".to_string());
        assert_eq!(ctx.substitute("${scheme}://${host}"), "https://example.com");
    }

    #[test]
    fn test_substitute_no_match() {
        let ctx = VariableContext::new();
        assert_eq!(ctx.substitute("no variables here"), "no variables here");
    }

    #[test]
    fn test_substitute_unknown_variable() {
        let ctx = VariableContext::new();
        // Unknown variables are left as-is
        assert_eq!(ctx.substitute("${unknown}"), "${unknown}");
    }

    #[test]
    fn test_substitute_no_second_order_injection() {
        let mut ctx = VariableContext::new();
        // client_ip contains a variable pattern — must NOT be re-interpreted
        ctx.set("client_ip", "${site_id}".to_string());
        ctx.set("site_id", "LEAKED".to_string());
        let result = ctx.substitute("IP: ${client_ip}");
        assert_eq!(result, "IP: ${site_id}"); // NOT "IP: LEAKED"
    }

    #[test]
    fn test_build_request_variables() {
        let ctx = build_request_variables(
            Some("1.2.3.4"),
            "req-123",
            "example.com",
            "/path",
            "https",
            "site1",
        );
        assert_eq!(ctx.substitute("${client_ip}"), "1.2.3.4");
        assert_eq!(ctx.substitute("${request_id}"), "req-123");
        assert_eq!(ctx.substitute("${site_id}"), "site1");
    }

    #[test]
    fn test_build_response_variables() {
        let ctx = build_response_variables(
            Some("1.2.3.4"),
            "req-123",
            "example.com",
            "/path",
            "https",
            "site1",
            "HIT",
        );
        assert_eq!(ctx.substitute("${cache_status}"), "HIT");
    }
}
