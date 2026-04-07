pub mod ip;
pub mod geo;

// WAF check chain: IP whitelist → IP blacklist → ASN → country whitelist → country blacklist → region
