---
name: Complete Dependency Summary
description: All confirmed Rust crate dependencies for Nozdormu CDN, organized by purpose, collected from all chapter analyses
type: reference
---

## 已有依赖 (workspace Cargo.toml)

```toml
pingora = { version = "0.8", features = ["lb", "cache", "openssl", "proxy", "prometheus"] }
pingora-limits = "0.8"
pingora-timeout = "0.8"
async-trait = "0.1"
tokio = { version = "1", features = ["rt-multi-thread", "signal", "macros"] }
serde = { version = "1.0", features = ["derive"] }
serde_yaml = "0.9"
serde_json = "1.0"
log = "0.4"
env_logger = "0.11"
clap = { version = "4.5", features = ["derive"] }
prometheus = "0.14"
once_cell = "1"
arc-swap = "1"
bytes = "1"
http = "1"
regex = "1"
chrono = "0.4"
thiserror = "2"
anyhow = "1"
```

## 新增依赖 (从各章节分析中确认)

### 配置与存储
```toml
etcd-client = "0.14"          # Ch2: etcd v3 配置中心
redis = { version = "0.27", features = ["tokio-comp", "connection-manager", "cluster-async", "streams", "sentinel"] }  # Ch2/5/9/12/13
```

### 安全防护
```toml
ipnet = "2"                    # Ch4: IP/CIDR 匹配 (WAF)
maxminddb = "0.24"             # Ch4: GeoIP2 数据库查询
moka = { version = "0.12", features = ["future"] }  # Ch5: TTL 缓存 (CC封禁/计数器/DNS)
crc32fast = "1"                # Ch5: URL/路径哈希 (CC key)
ring = "0.17"                  # Ch5/10: HMAC-SHA256 (JS挑战/ACME)
base64 = "0.22"                # Ch5/10: 编解码 (JS挑战/ACME)
```

### SSL/TLS
```toml
instant-acme = "0.7"           # Ch10: ACME v2 客户端
x509-parser = "0.16"           # Ch10: 证书过期时间解析
```

### 缓存
```toml
md-5 = "0.10"                  # Ch9: 缓存 key MD5 哈希
reqwest = { version = "0.12", features = ["rustls-tls"] }  # Ch9: OSS HTTP 客户端
```

### 管理接口
```toml
axum = { version = "0.7", features = ["tokio"] }  # Ch15: Admin API
```

### DNS
```toml
hickory-resolver = "0.24"      # Ch8: 异步 DNS 解析 (或用 Pingora 内置)
```

## 按 crate 分配

| crate | 使用的依赖 |
|-------|-----------|
| cdn-common | thiserror, anyhow, serde, log, http, bytes, pingora, ipnet |
| cdn-config | serde, serde_yaml, serde_json, etcd-client, log, anyhow, arc-swap |
| cdn-cache | redis, reqwest, md-5, serde_json, log, tokio |
| cdn-middleware | ipnet, maxminddb, moka, crc32fast, ring, base64, regex, serde, log |
| cdn-proxy | pingora, async-trait, redis, moka, prometheus, axum, instant-acme, x509-parser, hickory-resolver, once_cell, arc-swap, tokio, log, env_logger, clap |
