---
name: Chapter 8 - Load Balancing Design
description: LB algorithms (round_robin/ip_hash/random), health checks (active+passive), DNS resolution, backup fallback, retry mechanism
type: reference
---

## 核心简化 (vs 原系统)

| 原系统 | Pingora |
|--------|---------|
| ngx.ctx 恢复机制 | 不需要 — CTX 贯穿全程 |
| DNS 预解析 (balancer 禁止 cosocket) | 不需要 — upstream_peer() 是 async |
| ngx.shared.balancer_state 原子操作 | Pingora LoadBalancer 内置 |
| balancer.set_current_peer() | 返回 Box<HttpPeer> |
| balancer.set_more_tries() | fail_to_connect() 回调设置 retry |

## 算法映射

| 原算法 | Pingora 实现 |
|--------|-------------|
| 平滑加权轮询 | Pingora 内置 `RoundRobin` (已支持权重) |
| IP 哈希 | Pingora `Consistent` (Ketama) 以 client_ip 为 key, 或自定义 DJB2 |
| 加权随机 | 自定义实现 (简单加权随机) |

未知算法名 fallback 到 round_robin。

## upstream_peer() 流程

1. 获取健康主源站列表
2. 无健康主源站 → fallback 备用源站 (backup=true)
3. 无备用源站 → ConnectProxyFailure 错误
4. 执行 LB 算法选择
5. DNS 解析 (async, 带 moka 缓存 TTL=60s)
6. 构建 HttpPeer (TLS/超时/协议)
7. 记录 ctx.selected_origin

## 健康检查

**主动检查** — BackgroundService, 每 10s:
- HTTP GET {scheme}://{host}:{port}{path}
- 期望 200/204
- Host={origin.sni}, User-Agent=CDN-HealthCheck/1.0

**被动检查** — logging() 回调:
- status >= 500 或 0 或 error → record_failure
- 其他 → record_success

**状态存储**:
```rust
pub struct HealthChecker {
    status: Arc<DashMap<(String, String), bool>>,
    failures: Arc<moka::future::Cache<String, AtomicU32>>,  // 60s 窗口
    successes: Arc<moka::future::Cache<String, AtomicU32>>,
}
```

**阈值**: 连续 3 次失败 → 不健康, 连续 2 次成功 → 恢复
**跨节点**: 异步同步到 Redis `nozdormu:health:{site_id}:{origin_id}`

## 重试 — fail_to_connect()

```rust
fn fail_to_connect(...) -> Box<Error> {
    if ctx.balancer_tried < max_retries {
        e.set_retry(true); // 框架自动重新调用 upstream_peer()
    }
    e
}
```

## DNS 缓存

`moka::future::Cache<String, IpAddr>` TTL=60s
IP 地址直接使用, 域名异步解析
不再需要 content 阶段预解析
