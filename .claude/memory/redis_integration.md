---
name: Redis Integration for Nozdormu CDN
description: Redis crate selection, rate limiting Lua scripts, cache invalidation pub/sub, config streams, WAF state, session routing for CDN
type: reference
---

## Rust Redis 客户端

**推荐**: `redis` v0.27 (redis-rs)

```toml
redis = { version = "0.27", features = [
    "tokio-comp",
    "connection-manager",
    "cluster-async",
    "streams",
    "sentinel",
] }
```

- `ConnectionManager`: 自动重连、Clone 共享、生产首选
- `ClusterConnection`: Redis Cluster slot 路由、MOVED/ASK 重定向
- 备选: `fred` crate — 内置连接池、自动 pipeline，但社区较小

## CDN 各场景用途

| 场景 | Redis 功能 | 说明 |
|------|-----------|------|
| 分布式限流 | Lua 脚本 (Token Bucket) | 原子操作，单次 RTT |
| 缓存失效 | Pub/Sub | 实时广播 purge 到所有边缘节点 |
| 配置传播 | Streams + Consumer Group | 持久化，断线可回放 |
| 会话亲和 | SET + TTL | 粘性会话路由 |
| WAF 状态 | INCR + EXPIRE | 跨节点威胁计数 + IP 封禁 |
| Auth 缓存 | SET + TTL + Pub/Sub | Token 缓存 + 全局撤销 |

## 限流算法

### Token Bucket (Lua) — 推荐起步方案
- 低内存 (每 key 2 字段)，支持突发
- 无需安装 Redis 模块
- 原子性: HMGET + HMSET + PEXPIRE 在一个 Lua 脚本内

### Sliding Window (Lua)
- 更精确，但高流量 key 内存较高 (存每个时间戳)
- 使用 ZSET 存储请求时间戳

### redis-cell (CL.THROTTLE)
- 最快 (原生 C)，GCRA 算法
- 需要安装 Redis 模块，非标准 Redis

## 配置传播: Streams vs Pub/Sub

| 方面 | Pub/Sub | Streams |
|------|---------|---------|
| 持久化 | 无 | 有 (append-only log) |
| 断线回放 | 丢失 | 从 last ACK 回放 |
| 延迟 | 亚毫秒 | ~1ms |
| 消费者组 | 无 | 有 |
| **适用** | 缓存失效 (临时) | 配置传播 (持久) |

## 部署建议

**起步**: Redis Sentinel (1主2从+3哨兵)
- 单节点可处理 10万+ ops/sec
- Lua 脚本需要原子访问，Sentinel 更简单
- 后续按需迁移 Cluster

**迁移 Cluster 时机**: 单节点瓶颈 (>100k ops/sec 持续，或数据超 RAM)
