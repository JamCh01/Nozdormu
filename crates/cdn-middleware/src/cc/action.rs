use ring::hmac;
use std::net::IpAddr;
use std::time::{SystemTime, UNIX_EPOCH};

const CHALLENGE_VALIDITY_SECS: u64 = 300; // 5 minutes
const CHALLENGE_COOKIE_NAME: &str = "__cc_challenge";

/// CC action results.
#[derive(Debug, Clone)]
pub enum CcActionResult {
    /// Request is allowed to proceed.
    Allow,
    /// Block the request (429 Too Many Requests).
    Block { retry_after: u64, reason: String },
    /// Serve a JS challenge page (503).
    Challenge {
        cookie_value: String,
        reason: String,
    },
    /// Delay the request (sleep then continue).
    Delay { delay_ms: u64, reason: String },
    /// Log only, continue processing.
    Log { reason: String },
}

/// JS Challenge: HMAC-SHA256 based cookie challenge.
///
/// Sign: `HMAC-SHA256(secret, "{ip}|{timestamp}")` → base64
/// Verify: decode cookie → check 5-min expiry → verify HMAC
pub struct ChallengeManager {
    key: hmac::Key,
}

impl ChallengeManager {
    pub fn new(secret: &str) -> Self {
        let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
        Self { key }
    }

    /// Issue a challenge token for the given IP.
    /// Returns the cookie value to set.
    pub fn issue(&self, ip: IpAddr) -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let payload = format!("{}|{}", ip, timestamp);
        let tag = hmac::sign(&self.key, payload.as_bytes());
        let token = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            tag.as_ref(),
        );
        // Cookie value: base64(hmac)|timestamp
        format!("{}|{}", token, timestamp)
    }

    /// Verify a challenge cookie value for the given IP.
    /// Returns true if valid and not expired.
    pub fn verify(&self, ip: IpAddr, cookie_value: &str) -> bool {
        let parts: Vec<&str> = cookie_value.splitn(2, '|').collect();
        if parts.len() != 2 {
            return false;
        }

        let (token_b64, ts_str) = (parts[0], parts[1]);

        // Parse timestamp
        let timestamp: u64 = match ts_str.parse() {
            Ok(ts) => ts,
            Err(_) => return false,
        };

        // Check expiry
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now.saturating_sub(timestamp) > CHALLENGE_VALIDITY_SECS {
            return false;
        }

        // Decode token
        let token = match base64::Engine::decode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            token_b64,
        ) {
            Ok(t) => t,
            Err(_) => return false,
        };

        // Verify HMAC
        let payload = format!("{}|{}", ip, timestamp);
        hmac::verify(&self.key, payload.as_bytes(), &token).is_ok()
    }

    /// Get the cookie name used for challenges.
    pub fn cookie_name() -> &'static str {
        CHALLENGE_COOKIE_NAME
    }

    /// Generate the HTML page for the JS challenge.
    /// The page auto-submits a form that sets the challenge cookie.
    pub fn challenge_html(cookie_value: &str) -> String {
        // Escape cookie_value for safe embedding in a JS string literal.
        // Prevents XSS if the value ever contains attacker-influenced data.
        let escaped: String = cookie_value
            .chars()
            .flat_map(|c| match c {
                '\\' => vec!['\\', '\\'],
                '"' => vec!['\\', '"'],
                '\'' => vec!['\\', '\''],
                '<' => vec!['\\', 'x', '3', 'c'],
                '>' => vec!['\\', 'x', '3', 'e'],
                '\n' => vec!['\\', 'n'],
                '\r' => vec!['\\', 'r'],
                '/' => vec!['\\', '/'],
                _ => vec![c],
            })
            .collect();

        format!(
            r#"<!DOCTYPE html>
<html>
<head><title>Security Check</title></head>
<body>
<noscript><p>Please enable JavaScript to continue.</p></noscript>
<script>
document.cookie="{}={}; path=/; max-age={}; SameSite=Lax";
location.reload();
</script>
</body>
</html>"#,
            CHALLENGE_COOKIE_NAME, escaped, CHALLENGE_VALIDITY_SECS
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_issue_and_verify() {
        let mgr = ChallengeManager::new("test_secret_key");
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let cookie = mgr.issue(ip);
        assert!(mgr.verify(ip, &cookie));
    }

    #[test]
    fn test_wrong_ip_fails() {
        let mgr = ChallengeManager::new("test_secret_key");
        let ip1: IpAddr = "1.2.3.4".parse().unwrap();
        let ip2: IpAddr = "5.6.7.8".parse().unwrap();
        let cookie = mgr.issue(ip1);
        assert!(!mgr.verify(ip2, &cookie));
    }

    #[test]
    fn test_tampered_cookie_fails() {
        let mgr = ChallengeManager::new("test_secret_key");
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let cookie = mgr.issue(ip);
        let tampered = format!("AAAA{}", &cookie[4..]);
        assert!(!mgr.verify(ip, &tampered));
    }

    #[test]
    fn test_wrong_secret_fails() {
        let mgr1 = ChallengeManager::new("secret1");
        let mgr2 = ChallengeManager::new("secret2");
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let cookie = mgr1.issue(ip);
        assert!(!mgr2.verify(ip, &cookie));
    }

    #[test]
    fn test_malformed_cookie_fails() {
        let mgr = ChallengeManager::new("test_secret_key");
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        assert!(!mgr.verify(ip, ""));
        assert!(!mgr.verify(ip, "garbage"));
        assert!(!mgr.verify(ip, "a|b|c"));
        assert!(!mgr.verify(ip, "token|not_a_number"));
    }

    #[test]
    fn test_expired_cookie_fails() {
        let mgr = ChallengeManager::new("test_secret_key");
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        // Manually craft an expired cookie (timestamp = 0)
        let payload = format!("{}|0", ip);
        let tag = hmac::sign(
            &hmac::Key::new(hmac::HMAC_SHA256, b"test_secret_key"),
            payload.as_bytes(),
        );
        let token = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            tag.as_ref(),
        );
        let cookie = format!("{}|0", token);
        assert!(!mgr.verify(ip, &cookie));
    }

    #[test]
    fn test_challenge_html_contains_cookie() {
        let html = ChallengeManager::challenge_html("test_value");
        assert!(html.contains("__cc_challenge=test_value"));
        assert!(html.contains("location.reload()"));
    }

    #[test]
    fn test_challenge_html_escapes_xss() {
        let malicious = r#"";alert(1);//"#;
        let html = ChallengeManager::challenge_html(malicious);
        // Must NOT contain unescaped script-breaking characters
        assert!(!html.contains(r#"";alert(1);//"#));
        // The escaped version should be present
        assert!(html.contains(r#"\";alert(1);\/\/"#));
        // Verify <script> tags from input are escaped
        let html2 = ChallengeManager::challenge_html("</script><script>alert(1)</script>");
        assert!(!html2.contains("</script><script>"));
        assert!(html2.contains(r"\x3c"));
        assert!(html2.contains(r"\x3e"));
    }

    #[test]
    fn test_ipv6_challenge() {
        let mgr = ChallengeManager::new("test_secret_key");
        let ip: IpAddr = "2001:db8::1".parse().unwrap();
        let cookie = mgr.issue(ip);
        assert!(mgr.verify(ip, &cookie));
    }
}
