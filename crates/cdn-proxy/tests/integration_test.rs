//! Integration tests for the Nozdormu CDN proxy.
//!
//! These tests exercise the middleware and proxy logic end-to-end
//! without requiring live infrastructure (etcd, Redis, OSS).

mod waf_integration {
    use cdn_common::{WafConfig, WafMode, WafRules};
    use cdn_middleware::waf::{WafEngine, WafResult};
    use ipnet::IpNet;
    use std::str::FromStr;

    #[test]
    fn test_full_waf_chain_allow() {
        let engine = WafEngine::without_geoip();
        let waf = WafConfig {
            enabled: true,
            mode: WafMode::Block,
            rules: WafRules {
                ip_whitelist: vec![IpNet::from_str("10.0.0.0/8").unwrap()],
                ip_blacklist: vec![IpNet::from_str("192.168.0.0/16").unwrap()],
                ..Default::default()
            },
            ..Default::default()
        };

        // Whitelisted IP → allow
        let (result, _) = engine.check("10.0.0.1".parse().unwrap(), &waf, "site1");
        assert!(matches!(result, WafResult::Allow));

        // Non-listed IP → allow (no geo rules)
        let (result, _) = engine.check("8.8.8.8".parse().unwrap(), &waf, "site1");
        assert!(matches!(result, WafResult::Allow));
    }

    #[test]
    fn test_full_waf_chain_block() {
        let engine = WafEngine::without_geoip();
        let waf = WafConfig {
            enabled: true,
            mode: WafMode::Block,
            rules: WafRules {
                ip_blacklist: vec![IpNet::from_str("192.168.0.0/16").unwrap()],
                ..Default::default()
            },
            ..Default::default()
        };

        let (result, _) = engine.check("192.168.1.100".parse().unwrap(), &waf, "site1");
        assert!(result.is_blocked());
    }

    #[test]
    fn test_waf_whitelist_overrides_blacklist() {
        let engine = WafEngine::without_geoip();
        let waf = WafConfig {
            enabled: true,
            mode: WafMode::Block,
            rules: WafRules {
                ip_whitelist: vec![IpNet::from_str("192.168.1.0/24").unwrap()],
                ip_blacklist: vec![IpNet::from_str("192.168.0.0/16").unwrap()],
                ..Default::default()
            },
            ..Default::default()
        };

        // In both whitelist and blacklist → whitelist wins
        let (result, _) = engine.check("192.168.1.50".parse().unwrap(), &waf, "site1");
        assert!(matches!(result, WafResult::Allow));

        // In blacklist only → blocked
        let (result, _) = engine.check("192.168.2.50".parse().unwrap(), &waf, "site1");
        assert!(result.is_blocked());
    }
}

mod cc_integration {
    use cdn_common::{CcAction, CcConfig, CcKeyType, CcRule};
    use cdn_middleware::cc::{action::CcActionResult, CcEngine};

    #[tokio::test]
    async fn test_cc_rate_limit_lifecycle() {
        let engine = CcEngine::new("test_secret", 3, 60, 600, None);
        let cc = CcConfig {
            enabled: true,
            default_rate: 3,
            default_window: 60,
            default_block_duration: 600,
            default_action: CcAction::Block,
            rules: vec![],
        };
        let ip = "10.10.10.1".parse().unwrap();

        // 3 requests allowed
        for i in 0..3 {
            let result = engine.check(ip, "/api", "/api", None, &cc, "site1").await;
            assert!(
                matches!(result, CcActionResult::Allow),
                "request {} should be allowed",
                i
            );
        }

        // 4th request blocked
        let result = engine.check(ip, "/api", "/api", None, &cc, "site1").await;
        assert!(matches!(result, CcActionResult::Block { .. }));

        // Subsequent requests also blocked (IP is banned)
        let result = engine
            .check(ip, "/other", "/other", None, &cc, "site1")
            .await;
        assert!(matches!(result, CcActionResult::Block { .. }));
    }

    #[tokio::test]
    async fn test_cc_per_path_rules() {
        let engine = CcEngine::new("test_secret", 100, 60, 600, None);
        let cc = CcConfig {
            enabled: true,
            default_rate: 100,
            default_window: 60,
            default_block_duration: 600,
            default_action: CcAction::Block,
            rules: vec![CcRule {
                path: "/api/login".to_string(),
                rate: 2,
                window: 60,
                block_duration: 300,
                action: CcAction::Block,
                key_type: CcKeyType::IpPath,
            }],
        };
        let ip = "10.10.10.2".parse().unwrap();

        // /api/login has rate=2
        let r = engine
            .check(ip, "/api/login", "/api/login", None, &cc, "site1")
            .await;
        assert!(matches!(r, CcActionResult::Allow));
        let r = engine
            .check(ip, "/api/login", "/api/login", None, &cc, "site1")
            .await;
        assert!(matches!(r, CcActionResult::Allow));
        let r = engine
            .check(ip, "/api/login", "/api/login", None, &cc, "site1")
            .await;
        assert!(matches!(r, CcActionResult::Block { .. }));
    }

    #[tokio::test]
    async fn test_cc_challenge_flow() {
        let engine = CcEngine::new("challenge_secret", 1, 60, 600, None);
        let cc = CcConfig {
            enabled: true,
            default_rate: 1,
            default_window: 60,
            default_block_duration: 600,
            default_action: CcAction::Challenge,
            rules: vec![CcRule {
                path: "/".to_string(),
                rate: 1,
                window: 60,
                block_duration: 300,
                action: CcAction::Challenge,
                key_type: CcKeyType::Ip,
            }],
        };
        let ip = "10.10.10.3".parse().unwrap();

        // First request allowed
        let r = engine.check(ip, "/", "/", None, &cc, "site1").await;
        assert!(matches!(r, CcActionResult::Allow));

        // Second request → challenge
        let r = engine.check(ip, "/", "/", None, &cc, "site1").await;
        match r {
            CcActionResult::Challenge { cookie_value, .. } => {
                // Simulate browser setting the cookie and retrying
                let cookie = format!("__cc_challenge={}", cookie_value);
                let r = engine
                    .check(ip, "/", "/", Some(&cookie), &cc, "site1")
                    .await;
                assert!(matches!(r, CcActionResult::Allow));
            }
            other => panic!("expected Challenge, got {:?}", other),
        }
    }
}

mod redirect_integration {
    use cdn_common::{DomainRedirectConfig, ForceHttpsConfig, UrlRedirectRule, UrlRuleType};
    use cdn_middleware::redirect;
    use std::collections::HashMap;

    #[test]
    fn test_domain_redirect_priority() {
        let domain_redirect = DomainRedirectConfig {
            enabled: true,
            target_domain: "new.example.com".to_string(),
            source_domains: vec![],
            status_code: 301,
        };
        let force_https = ForceHttpsConfig {
            enable: true,
            redirect_code: 301,
            ..Default::default()
        };

        // Domain redirect has higher priority than protocol redirect
        let result = redirect::check_redirect(
            "http",
            "old.example.com",
            "/path",
            "/path",
            None,
            "GET",
            Some(&domain_redirect),
            &force_https,
            &[],
        );
        let r = result.unwrap();
        assert!(matches!(r.source, redirect::RedirectSource::Domain));
        assert_eq!(r.target_url, "http://new.example.com/path");
    }

    #[test]
    fn test_protocol_redirect_when_no_domain() {
        let force_https = ForceHttpsConfig {
            enable: true,
            redirect_code: 301,
            ..Default::default()
        };

        let result = redirect::check_redirect(
            "http",
            "example.com",
            "/path",
            "/path",
            None,
            "GET",
            None,
            &force_https,
            &[],
        );
        let r = result.unwrap();
        assert!(matches!(r.source, redirect::RedirectSource::Protocol));
        assert_eq!(r.target_url, "https://example.com/path");
    }

    #[test]
    fn test_url_rule_redirect() {
        let force_https = ForceHttpsConfig::default();
        let rules = vec![UrlRedirectRule {
            r#type: UrlRuleType::Prefix,
            source: Some("/old/".to_string()),
            source_domain: None,
            target: "/new/$1".to_string(),
            status_code: 302,
            enabled: true,
            preserve_query_string: true,
            methods: vec![],
            match_query_string: false,
            regex_options: None,
            cache_control: None,
            response_headers: HashMap::new(),
        }];

        let result = redirect::check_redirect(
            "https",
            "example.com",
            "/old/page?q=1",
            "/old/page",
            Some("q=1"),
            "GET",
            None,
            &force_https,
            &rules,
        );
        let r = result.unwrap();
        assert!(matches!(r.source, redirect::RedirectSource::UrlRule));
        assert_eq!(r.target_url, "/new/page?q=1");
        assert_eq!(r.status_code, 302);
    }

    #[test]
    fn test_no_redirect() {
        let force_https = ForceHttpsConfig::default();
        let result = redirect::check_redirect(
            "https",
            "example.com",
            "/path",
            "/path",
            None,
            "GET",
            None,
            &force_https,
            &[],
        );
        assert!(result.is_none());
    }
}

mod cache_integration {
    use cdn_cache::key::generate_cache_key;
    use cdn_cache::strategy::{
        adjust_ttl, check_request_cacheability, check_response_cacheability,
    };
    use cdn_common::CacheConfig;

    #[test]
    fn test_full_cache_flow() {
        let config = CacheConfig::default();

        // Step 1: Check request cacheability
        let decision = check_request_cacheability("GET", "/page.html", None, false, &config);
        assert!(decision.cacheable);
        assert_eq!(decision.ttl, 3600);

        // Step 2: Generate cache key
        let key = generate_cache_key("site1", "example.com", "/page.html", Some("v=1"), true, &[]);
        assert_eq!(key.len(), 32);

        // Step 3: Check response cacheability
        let resp =
            check_response_cacheability(200, Some("max-age=600"), false, None, Some(1024), &config);
        assert!(resp.cacheable);

        // Step 4: Adjust TTL
        let final_ttl = adjust_ttl(decision.ttl, Some("max-age=600"), None);
        assert_eq!(final_ttl, 600); // min(3600, 600)
    }

    #[test]
    fn test_cache_bypass_post() {
        let config = CacheConfig::default();
        let decision = check_request_cacheability("POST", "/api", None, false, &config);
        assert!(!decision.cacheable);
    }

    #[test]
    fn test_cache_bypass_no_store() {
        let config = CacheConfig::default();
        let decision = check_request_cacheability("GET", "/", Some("no-store"), false, &config);
        assert!(!decision.cacheable);
    }

    #[test]
    fn test_cache_key_deterministic_with_sort() {
        let k1 = generate_cache_key("s", "h", "/p", Some("b=2&a=1"), true, &[]);
        let k2 = generate_cache_key("s", "h", "/p", Some("a=1&b=2"), true, &[]);
        assert_eq!(k1, k2);
    }

    #[test]
    fn test_s_maxage_overrides_max_age() {
        let ttl = adjust_ttl(3600, Some("max-age=600, s-maxage=120"), None);
        assert_eq!(ttl, 120);
    }
}

mod health_integration {
    use cdn_proxy::health::HealthChecker;

    #[test]
    fn test_health_lifecycle() {
        let hc = HealthChecker::new(3, 2);

        // Initially healthy
        assert!(hc.is_healthy("site1", "origin1"));

        // 3 failures → unhealthy
        hc.record_failure("site1", "origin1");
        hc.record_failure("site1", "origin1");
        assert!(hc.is_healthy("site1", "origin1")); // still healthy at 2
        hc.record_failure("site1", "origin1");
        assert!(!hc.is_healthy("site1", "origin1")); // now unhealthy

        // 2 successes → recover
        hc.record_success("site1", "origin1");
        assert!(!hc.is_healthy("site1", "origin1")); // 1 success
        hc.record_success("site1", "origin1");
        assert!(hc.is_healthy("site1", "origin1")); // recovered
    }

    #[test]
    fn test_health_independent_sites() {
        let hc = HealthChecker::new(1, 1);
        hc.record_failure("site1", "o1");
        assert!(!hc.is_healthy("site1", "o1"));
        assert!(hc.is_healthy("site2", "o1")); // different site
    }
}

mod ip_utils_integration {
    use cdn_proxy::utils::ip::{is_private_ip, real_ip_from_xff};

    #[test]
    fn test_xff_spoofing_prevention() {
        let remote: std::net::IpAddr = "203.0.113.1".parse().unwrap();

        // Attacker sends: X-Forwarded-For: 1.1.1.1, 5.6.7.8
        // Trusted proxy adds remote_addr
        // We traverse right-to-left, skip trusted, return first untrusted
        let real = real_ip_from_xff("1.1.1.1, 5.6.7.8, 10.0.0.1", remote, &[]);
        assert_eq!(real, "5.6.7.8".parse::<std::net::IpAddr>().unwrap());
    }

    #[test]
    fn test_private_ip_detection() {
        assert!(is_private_ip("127.0.0.1".parse().unwrap()));
        assert!(is_private_ip("10.0.0.1".parse().unwrap()));
        assert!(is_private_ip("192.168.1.1".parse().unwrap()));
        assert!(is_private_ip("172.16.0.1".parse().unwrap()));
        assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_private_ip("1.2.3.4".parse().unwrap()));
    }
}
