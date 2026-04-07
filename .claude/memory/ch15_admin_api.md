---
name: Chapter 15 - Admin API Design
description: Three-tier access control (public/intranet/localhost), health/metrics/status endpoints, local management port 8080 with Axum
type: reference
---

## 三层访问控制

| 层级 | 端点 | 访问控制 | 实现位置 |
|------|------|---------|---------|
| 公开 | /health | 无限制 | request_filter() 拦截 |
| 内网 | /health/detail, /status | 内网 IP 检查 | request_filter() 拦截 |
| 内网 | /metrics | 内网 IP | Pingora Prometheus Service (:9090) |
| 本地 | /reload, /ssl/*, /site/*, /upstream/*, /cc/* | localhost:8080 | 独立 Axum 服务 |

## 公开 + 内网端点 → request_filter() 最先检查

在路由匹配之前拦截:
1. `/health` → 200 "OK" (零依赖)
2. `/.well-known/acme-challenge/` → ACME 挑战响应
3. `/health/detail`, `/status` → IP 检查后处理

## /health/detail 检查项 (调整)

| 检查 | 原系统 | Nozdormu |
|------|--------|----------|
| Sentinel quorum | ✓ | Redis PING (ConnectionManager 自动处理) |
| Master 可达 | ✓ | Redis PING |
| 共享内存空间 | ✓ | 不需要 (堆内存) |
| etcd 可达 | — | 新增 |

## 本地管理端口 → Axum on 127.0.0.1:8080

```rust
Router::new()
    .route("/reload", post(reload_config))
    .route("/ssl/clear-cache", post(clear_ssl_cache))
    .route("/site/:id", get(get_site_config))
    .route("/upstream/health", get(get_upstream_health))
    .route("/cc/blocked", get(get_cc_blocked))
```

作为 BackgroundService 注册到 Pingora Server。

## 各端点实现

| 端点 | 实现 |
|------|------|
| /reload | etcd 全量重载 → ArcSwap::store() |
| /ssl/clear-cache | cert_cache.invalidate_all() |
| /site/{id} | live_config.load().sites.get(id) → JSON |
| /upstream/health | 遍历 health_checker.status → JSON |
| /cc/blocked | 遍历 cc_state.blocked_index → JSON |

## CC 封禁遍历

moka::Cache 不支持遍历, 需要额外 blocked_index:
```rust
blocked_index: Arc<DashMap<String, HashSet<IpAddr>>>  // site_id → IPs
```
封禁/解封时同步更新, moka eviction listener 清理过期条目。

## 新增依赖

- `axum = { version = "0.7", features = ["tokio"] }` — Admin API
