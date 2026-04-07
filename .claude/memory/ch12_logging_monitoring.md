---
name: Chapter 12 - Logging and Monitoring Design
description: Request logging to Redis Streams, Prometheus metrics (counter/gauge/histogram), health check endpoints, status endpoint
type: reference
---

## 请求日志 → logging() 回调

收集 30+ 字段 (时间/请求/客户端/响应/上游/CDN/安全/SSL)
触发链: 收集日志 → Prometheus 指标 → 被动健康检查 → 异步 Redis Streams

## Redis Streams 推送

```rust
tokio::spawn(async move {
    redis::cmd("XADD")
        .arg("nozdormu:log:requests")
        .arg("MAXLEN").arg("~").arg(100000)
        .arg("*").arg("data").arg(&json)
        .query_async(&mut redis).await;
});
```

MAXLEN ~ 100000 近似裁剪, tokio::spawn 异步不阻塞

## Prometheus 指标 → `prometheus` crate

**Counters**: requests_total, bytes_total, cache_hits/misses, waf/cc_blocks, upstream_requests/failures, ssl_handshakes, ws_connections, grpc_requests, redirects
**Gauges**: connections_active/reading/writing/waiting, cache_objects/bytes, upstream_healthy/unhealthy, cert_expiry_seconds, cc_blocked_ips
**Histograms**: request_duration_seconds, upstream_duration_seconds, response_size_bytes

标签: site_id, method, status, protocol, upstream, domain, rule_type 等

## 指标收集时机

- 每个请求 (logging 阶段): 请求计数/字节/缓存/延迟/上游
- 每 15 秒 (BackgroundService): 上游健康/证书过期/CC 封禁
- Pingora Prometheus Service (:9090) 自动暴露

## 健康检查端点

- /health: 200 "OK", 公开, 零依赖
- /health/detail: Redis PING + etcd status, 内网限制, JSON

## 状态端点

- /status: 节点信息 + 连接数 + uptime, 内网限制, JSON
- 不再需要共享内存容量监控 (Pingora 用堆内存)
