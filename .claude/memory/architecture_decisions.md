---
name: Architecture Decisions for Nozdormu CDN
description: Confirmed architectural decisions from migration discussions — storage split, config isolation, key design, crate responsibilities, port layout
type: project
---

## 存储架构 (已确认)

**etcd (配置中心, 强一致)**:
- 站点配置: `/nozdormu/sites/{site_id}`
- TLS 证书: `/nozdormu/certs/{domain}`
- ACME 挑战: `/nozdormu/acme/challenges/{domain}/{token}`
- 节点标签: `/nozdormu/global/node_labels/{node_id}`

**Redis (运行时状态, 高吞吐)**:
- CC 计数器: `nozdormu:cc:counter:{site_id}:{ip}:{rule_id}`
- CC 封禁: `nozdormu:cc:blocked:{ip}`
- CC 挑战 token: `nozdormu:cc:challenge:{token}`
- 健康状态: `nozdormu:health:{site_id}:{origin_id}`
- 缓存元数据: `nozdormu:cache:meta:{site_id}:{cache_key}`
- 请求日志 Stream: `nozdormu:log:requests`
- 分布式锁: `nozdormu:lock:{resource}`

**Why:** 用户确认从纯 Redis 迁移到 etcd + Redis 双层架构。etcd 负责强一致配置，Redis 负责高吞吐运行时状态。

## 配置隔离模式 (已确认)

**全局共享 + 标签选择器**:
- 所有站点配置存储在全局 etcd 路径下
- 每个节点声明自己的标签 (如 `["region:asia", "tier:edge"]`)
- 站点配置包含 `target_labels` 字段
- 节点启动时按标签过滤，只加载匹配的站点
- 不再使用 node_group_id + node_id 隔离

**Why:** 用户选择全局共享+选择器模式，比原系统的节点级隔离更灵活。

## Redis Key 设计 (已确认)

**重新设计，不保持原系统兼容**:
- 前缀统一为 `nozdormu:` (原系统为 `cdn:`)
- 配置类 key 迁移到 etcd，Redis 仅保留运行时状态
- 不需要数据迁移工具（全新设计）

**Why:** 用户确认重新设计 key 结构，配合 etcd + Redis 新架构。

## 缓存架构 (已确认)

**Redis 元数据 + OSS/S3 数据体** — 保持原架构, 不使用 pingora-cache。
- Redis: `nozdormu:cache:meta:{site_id}:{key}` → 元数据 JSON (TTL)
- OSS: `cache/{site_id}/{key前2位}/{key}` → 响应体原始数据
- 理由: pingora-cache 实验性不适合生产, 无本地状态便于扩缩容
- 未来可选: moka 进程内元数据缓存作为 L0 优化

## Admin API (已确认)

**三层访问控制**:
- 公开 (80/443): `/health` — 零依赖探活
- 内网 (80/443): `/health/detail`, `/status` — IP 检查, 在 request_filter() 拦截
- 本地 (127.0.0.1:8080): `/reload`, `/ssl/clear-cache`, `/site/{id}`, `/upstream/health`, `/cc/blocked` — Axum 独立服务
- Prometheus: `:9090` — Pingora 内置 Prometheus Service

**新增依赖**: `axum = "0.7"`

## 监听端口规划

| 端口 | 协议 | 用途 |
|------|------|------|
| 80 | HTTP | HTTP 代理, WS, SSE, h2c |
| 443 | HTTPS | HTTPS 代理, WSS, gRPC (TLS + ALPN H2H1) |
| 8443 | TCP Stream | L4 TCP/TLS 代理 (SNI 路由) |
| 8080 | HTTP | Admin API (localhost only, 待定) |
| 9090 | HTTP | Prometheus 指标端点 |

## Crate 职责 (已确认)

```
cdn-common/     → 错误类型, 共享数据结构 (SiteConfig, Origin, CertEntry...)
cdn-config/     → NodeConfig (环境变量), LiveConfig (etcd watch + ArcSwap), Schema 验证
cdn-cache/      → 缓存策略引擎, Redis 元数据, OSS 数据体读写
cdn-middleware/ → 插件系统: WAF, CC防护, 跳转引擎, 响应头处理, 协议检测
cdn-proxy/      → ProxyHttp 实现, TLS 管理, 负载均衡, 健康检查, 日志, 指标
```

## 初始化流程 (已确认)

```
fn main()
  ├── 加载 NodeConfig (环境变量)
  ├── 验证配置 + 输出启动信息
  ├── Server::new() + bootstrap()
  ├── 连接 etcd → 全量加载站点配置 → ArcSwap<LiveConfig>
  ├── 连接 Redis (ConnectionManager)
  ├── 加载 GeoIP (可选)
  ├── BackgroundService: etcd watch, 健康检查, 指标收集, 证书续期
  ├── ProxyService (HTTP :80, HTTPS :443)
  ├── Prometheus Service (:9090)
  └── server.run_forever()
```

## 配置热更新 (已确认)

- 原系统 Redis Pub/Sub → 替换为 etcd watch stream
- 全量加载 + revision-based 增量更新
- 断线重连从 last_revision 恢复，不丢事件
- 不需要 publish/broadcast 工具函数 — 外部系统直接写 etcd

## 原系统设计约束在 Pingora 中的解决

| 原约束 | Pingora 状态 |
|--------|-------------|
| ngx.ctx 在 ngx.exec 后丢失 | 不存在 — CTX 贯穿请求生命周期 |
| DNS 禁止在 balancer 阶段 | 不存在 — upstream_peer() 是 async fn |
| 环境变量需显式声明 | 不存在 — std::env::var() 直接读取 |
| Worker 0 独占定时任务 | 不存在 — BackgroundService 是全局单例 |

## 共享内存字典 → Rust 替代 (已确认)

| 原字典 | Rust 替代 |
|--------|----------|
| site_cache (100m) | Arc<ArcSwap<LiveConfig>> (etcd watch) |
| config_cache (50m) | 合并到 LiveConfig |
| health_status (10m) | Arc<DashMap<origin_id, HealthState>> |
| cc_counter (100m) | Arc<moka::Cache<key, AtomicU64>> + Redis |
| cc_blocked (50m) | Arc<moka::Cache<ip, BlockInfo>> (TTL) |
| ssl_cache (50m) | Pingora 内置 TLS session cache |
| cert_cache (50m) | Arc<ArcSwap<HashMap<sni, CertKeyPair>>> (etcd) |
| metrics (20m) | prometheus crate Registry |
| locks (1m) | tokio::sync::Mutex / Redis 分布式锁 |
| ipc (10m) | 不需要 — 单进程多线程 |
| balancer_state (10m) | Pingora LoadBalancer 内置 |
