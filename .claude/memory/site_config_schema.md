---
name: Site Configuration Schema
description: Complete SiteConfig structure migrated from OpenResty — domains, origins, LB, protocol, SSL, cache, WAF, CC fields with defaults and validation rules
type: reference
---

## SiteConfig 完整结构 (从原系统迁移)

```rust
pub struct SiteConfig {
    // 基础
    pub site_id: String,              // 必填
    pub enabled: bool,                // 默认 true
    pub domains: Vec<String>,         // 必填, ≥1, 支持 *.example.com
    pub target_labels: Vec<String>,   // 节点选择器 (新增, 替代 group/node 隔离)

    // 源站
    pub origins: Vec<OriginConfig>,   // 必填, ≥1

    // 子配置
    pub load_balancer: LoadBalancerConfig,
    pub protocol: ProtocolConfig,
    pub ssl: SslConfig,
    pub cache: CacheConfig,
    pub waf: WafConfig,
    pub cc: CcConfig,
}

pub struct OriginConfig {
    pub id: String,                   // 必填
    pub host: String,                 // 必填
    pub port: u16,                    // 默认 80, 1-65535
    pub weight: u32,                  // 默认 10, 1-100
    pub protocol: OriginProtocol,     // http | https, 默认 http
    pub backup: bool,                 // 默认 false
    pub enabled: bool,                // 默认 true
}

pub struct LoadBalancerConfig {
    pub algorithm: LbAlgorithm,       // round_robin | ip_hash | random, 默认 round_robin
    pub retries: u32,                 // 默认 2, 0-10
    pub health_check: HealthCheckConfig,
}

pub struct HealthCheckConfig {
    pub enabled: bool,                // 默认 true
    pub r#type: HealthCheckType,      // http | tcp, 默认 http
    pub path: String,                 // 默认 "/health"
    pub interval: u64,                // 默认 10, ≥1 (秒)
    pub timeout: u64,                 // 默认 5, ≥1 (秒)
    pub healthy_threshold: u32,       // 默认 2, ≥1
    pub unhealthy_threshold: u32,     // 默认 3, ≥1
}

pub struct ProtocolConfig {
    pub force_https: bool,            // 默认 false
    pub redirect_code: u16,           // 301|302|307|308, 默认 301
    pub http2: bool,                  // 默认 true
    pub websocket: bool,              // 默认 false
    pub sse: bool,                    // 默认 false
    pub grpc: GrpcConfig,
}

pub struct GrpcConfig {
    pub enabled: bool,                // 默认 false
    pub mode: GrpcMode,              // layer4 | layer7, 默认 layer7
}

pub struct SslConfig {
    pub enabled: bool,                // 默认 true
    pub r#type: SslType,             // acme | custom, 默认 acme
    pub acme_email: Option<String>,
}

pub struct CacheConfig {
    pub enabled: bool,                // 默认 true
    pub default_ttl: u64,             // 默认 3600, ≥0 (秒)
    pub max_size: u64,                // 默认 104857600 (100MB)
    pub rules: Vec<CacheRule>,
}

pub struct WafConfig {
    pub enabled: bool,                // 默认 false
    pub mode: WafMode,               // block | log, 默认 block
    pub rules: WafRules,
}

pub struct CcConfig {
    pub enabled: bool,                // 默认 false
    pub rules: Vec<CcRule>,
}
```

## 验证规则

- 域名: 长度 1-253, 支持 `*.` 通配符, 字符 `[a-zA-Z0-9-.]`
- IP: IPv4 格式 + 可选 CIDR (/0-32)
- URL: 必须以 `http://` 或 `https://` 开头
- 端口: 1-65535

## 默认值填充

所有 Option 字段通过 `#[serde(default)]` + `Default` trait 自动填充。
反序列化时自动应用默认值，等同于原系统的 `apply_defaults()`。
