# Nozdormu CDN — 开发计划

## Context

将原 OpenResty CDN 系统（~55 个 Lua 文件，~9000+ 行）迁移到 Rust/Pingora。原系统文档共 15 章，所有架构决策已确认。当前 workspace 已搭建完成，有基础的 ProxyHttp 骨架代码。

**策略**：先完成原系统功能 1:1 迁移，再扩展商业化功能。

---

## Phase 0: 基础设施与依赖 [预计 1-2 天]

> 更新 workspace 依赖，建立所有 crate 的模块骨架。

### 0.1 更新 workspace Cargo.toml — 添加全部新增依赖

```toml
# 新增到 [workspace.dependencies]
etcd-client = "0.14"
redis = { version = "0.27", features = ["tokio-comp", "connection-manager", "cluster-async", "streams", "sentinel"] }
ipnet = "2"
maxminddb = "0.24"
moka = { version = "0.12", features = ["future"] }
crc32fast = "1"
ring = "0.17"
base64 = "0.22"
instant-acme = "0.7"
x509-parser = "0.16"
md-5 = "0.10"
reqwest = { version = "0.12", features = ["rustls-tls"] }
axum = { version = "0.7", features = ["tokio"] }
hickory-resolver = "0.24"
dashmap = "6"
uuid = { version = "1", features = ["v4"] }
```

### 0.2 建立各 crate 模块骨架

```
crates/cdn-common/src/
  lib.rs, error.rs, types.rs (SiteConfig + 所有子结构体)

crates/cdn-config/src/
  lib.rs, node_config.rs (环境变量), live_config.rs (ArcSwap),
  etcd_watcher.rs, schema.rs (验证)

crates/cdn-middleware/src/
  lib.rs, waf/ (mod.rs, ip.rs, geo.rs, asn.rs, country.rs, region.rs),
  cc/ (mod.rs, counter.rs, action.rs, state.rs),
  redirect/ (mod.rs, protocol.rs, url.rs, domain.rs),
  headers/ (mod.rs, request.rs, response.rs, variables.rs)

crates/cdn-cache/src/
  lib.rs, strategy.rs, key.rs, storage.rs, oss.rs

crates/cdn-proxy/src/
  main.rs, proxy.rs, context.rs, protocol.rs,
  balancer.rs, health.rs, dns.rs,
  ssl/ (mod.rs, manager.rs, acme.rs, challenge.rs, storage.rs, renewal.rs),
  logging/ (mod.rs, metrics.rs, queue.rs),
  admin/ (mod.rs, endpoints.rs),
  utils/ (ip.rs, lock.rs, redis_pool.rs)
```

### 0.3 Docker 开发环境更新

- `docker-compose.yml` 添加 etcd 3 节点集群 + Redis Sentinel 集群
- 开发时 `--profile dev` 启动全部基础设施

### 验证

- `cargo check` 全部 crate 编译通过（空骨架）
- `docker compose --profile dev up` 基础设施启动

---

## Phase 1: 核心类型与配置系统 [预计 3-4 天]

> 对应原系统第 2 章。这是所有模块的基础。

### 1.1 cdn-common: 共享类型定义

**文件**: `crates/cdn-common/src/types.rs`

定义所有核心数据结构（从记忆 `site_config_schema.md` 中确认的完整结构）：

- `SiteConfig` — 站点配置（含 domains, origins, load_balancer, protocol, ssl, cache, waf, cc, headers, domain_redirect, target_labels）
- `OriginConfig` — 源站配置
- `LoadBalancerConfig` / `HealthCheckConfig`
- `ProtocolConfig` / `GrpcConfig`
- `SslConfig`
- `CacheConfig` / `CacheRule`
- `WafConfig` / `WafRules`
- `CcConfig` / `CcRule`
- `HeadersConfig` / `HeaderRule`
- `DomainRedirectConfig`
- 所有枚举: `LbAlgorithm`, `OriginProtocol`, `WafMode`, `CcAction`, `CcKeyType`, `HeaderAction`, `GrpcMode`, `SslType`, `HealthCheckType`, `UrlRuleType`

所有结构体 `#[derive(Deserialize, Serialize, Clone, Debug)]`，`#[serde(default)]` 填充默认值。

### 1.2 cdn-config: 节点配置

**文件**: `crates/cdn-config/src/node_config.rs`

从环境变量加载静态配置（对应原系统 config.lua 的 60+ 环境变量）：

- `NodeConfig` — 顶层配置
- `NodeInfo` — node_id, labels
- `RedisConfig` — sentinel/standalone 双模式
- `EtcdConfig` — endpoints, prefix, auth
- `CacheOssConfig` — endpoint, bucket, region, credentials
- `SecurityConfig` — waf_default_mode, cc defaults, challenge_secret
- `BalancerConfig` — default_algorithm, retries, dns_nameservers, health_check
- `ProxyTimeoutConfig` — connect/send/read/websocket/sse/grpc
- `SslAcmeConfig` — environment, email, providers, eab_credentials
- `LogConfig` — level, push_to_redis, stream_max_len
- `validate()` — 验证必要配置项

### 1.3 cdn-config: 动态配置 (etcd)

**文件**: `crates/cdn-config/src/live_config.rs`, `etcd_watcher.rs`

- `LiveConfig` — sites HashMap, domain_index, wildcard_index
- `load_all()` — etcd GET with prefix, 按 target_labels 过滤, 构建索引
- `match_site(host)` — 精确匹配 → 通配符匹配 → None
- `EtcdWatcher` — BackgroundService, watch stream, 增量更新 ArcSwap

### 1.4 cdn-config: Schema 验证

**文件**: `crates/cdn-config/src/schema.rs`

- `validate_site_config()` — 域名格式、端口范围、枚举值、必填字段
- `validate_domain()` — 长度 1-253, 字符限制, 通配符格式
- 错误收集: `Vec<ValidationError>` 带路径

### 验证

- 单元测试: SiteConfig 反序列化 + 默认值填充 + 验证
- 集成测试: etcd 连接 → 写入测试站点 → load_all → match_site
- etcd watch: 写入变更 → 验证 ArcSwap 更新

---

## Phase 2: 请求路由与上下文 [预计 1-2 天]

> 对应原系统第 3 章。

### 2.1 ProxyCtx 完整定义

**文件**: `crates/cdn-proxy/src/context.rs`

定义完整的请求上下文结构体（从记忆 `ch3_routing_design.md`）。

### 2.2 路由匹配

**文件**: `crates/cdn-proxy/src/proxy.rs`

在 `request_filter()` 中实现：
1. 管理端点拦截 (/health, /health/detail, /status, ACME challenge)
2. Host 标准化 (端口剥离 + 小写)
3. LiveConfig 查询 (精确 → 通配符)
4. 站点启用检查
5. 设置 ctx.site_config, ctx.site_id

### 验证

- 测试: 精确匹配、通配符匹配、未匹配 404、站点禁用

---

## Phase 3: 安全防护 — WAF [预计 2-3 天]

> 对应原系统第 4 章。

### 3.1 WAF 检查链

**文件**: `crates/cdn-middleware/src/waf/mod.rs`

6 步检查: IP 白名单 → IP 黑名单 → ASN → 国家白名单 → 国家黑名单 → 地区

### 3.2 IP/CIDR 匹配

**文件**: `crates/cdn-middleware/src/waf/ip.rs`

使用 `ipnet` crate。

### 3.3 GeoIP 集成

**文件**: `crates/cdn-middleware/src/waf/geo.rs`

使用 `maxminddb` crate，同时加载 City + Country + ASN 数据库。请求级缓存到 ProxyCtx.geo_info。

### 3.4 Prometheus WAF 统计

`cdn_waf_blocked_total{site_id, block_type}` Counter。

### 验证

- 单元测试: CIDR 匹配、GeoIP 查询、检查链顺序
- 集成测试: 配置 WAF 规则 → 请求 → 验证 403/放行

---

## Phase 4: 安全防护 — CC [预计 2-3 天]

> 对应原系统第 5 章。

### 4.1 CC 状态管理

**文件**: `crates/cdn-middleware/src/cc/state.rs`

`CcState` — moka 封禁缓存 + 本地计数器。

### 4.2 混合计数器

**文件**: `crates/cdn-middleware/src/cc/counter.rs`

本地 moka + 异步 Redis 同步（每 10 次）。

### 4.3 规则匹配 + Key 生成

**文件**: `crates/cdn-middleware/src/cc/mod.rs`

最长前缀匹配，3 种 key_type (ip/ip_url/ip_path)。

### 4.4 JS 挑战

**文件**: `crates/cdn-middleware/src/cc/action.rs`

HMAC-SHA256 签发/验证，ring crate。

### 验证

- 单元测试: 规则匹配、计数器递增、JS 挑战签发/验证
- 集成测试: 超过阈值 → 429、JS 挑战流程

---

## Phase 5: 跳转引擎 [预计 1-2 天]

> 对应原系统第 6 章。

### 5.1 三级跳转

**文件**: `crates/cdn-middleware/src/redirect/mod.rs`

域名跳转 → 协议跳转 → URL 规则跳转。

### 5.2 协议跳转

**文件**: `crates/cdn-middleware/src/redirect/protocol.rs`

force_https + ACME 排除 + exclude_paths。

### 5.3 URL 规则

**文件**: `crates/cdn-middleware/src/redirect/url.rs`

4 种匹配 (exact/prefix/regex/domain) + 变量替换 + 查询参数保留。

### 验证

- 单元测试: 各匹配类型、变量替换、ACME 排除
- 集成测试: 配置跳转规则 → 请求 → 验证 301/302 + Location 头

---

## Phase 6: 多协议代理 [预计 3-4 天]

> 对应原系统第 7 章。

### 6.1 协议检测

**文件**: `crates/cdn-proxy/src/protocol.rs`

gRPC > WebSocket > SSE > HTTP 优先级检测。

### 6.2 WebSocket 代理

在 `request_filter()` 中验证 RFC 6455 + Origin CORS。Pingora 原生处理 Upgrade。

### 6.3 SSE 代理

`upstream_request_filter()` 设置 Accept-Encoding=identity。`response_filter()` 设置 X-Accel-Buffering=no。

### 6.4 gRPC 代理

`upstream_peer()` 中强制 H2 + max_h2_streams。gRPC-Web 通过 Pingora GrpcWeb 模块。服务白名单在 `request_filter()` 检查。

### 6.5 超时配置

按协议类型在 `upstream_peer()` 中设置不同的 read_timeout。

### 验证

- 集成测试: HTTP 代理、WebSocket 升级、SSE 流、gRPC unary

---

## Phase 7: 负载均衡与健康检查 [预计 2-3 天]

> 对应原系统第 8 章。

### 7.1 upstream_peer() 完整实现

**文件**: `crates/cdn-proxy/src/balancer.rs`

健康源站过滤 → 备用源站 fallback → LB 算法选择 → DNS 解析 → HttpPeer 构建。

### 7.2 LB 算法

- RoundRobin: Pingora 内置
- IpHash: 自定义 DJB2 哈希 + 加权选择
- Random: 自定义加权随机

### 7.3 健康检查

**文件**: `crates/cdn-proxy/src/health.rs`

- `HealthChecker` struct: DashMap 状态 + moka 失败/成功计数
- 主动检查: BackgroundService, 每 10s HTTP 探测
- 被动检查: `logging()` 回调中记录
- 异步同步到 Redis

### 7.4 DNS 缓存

**文件**: `crates/cdn-proxy/src/dns.rs`

`moka::future::Cache<String, IpAddr>` TTL 60s。

### 7.5 重试

`fail_to_connect()` 中设置 `retry=true`。

### 验证

- 单元测试: LB 算法权重分布、健康状态切换
- 集成测试: 源站宕机 → 健康检查标记 → 流量切换

---

## Phase 8: 内容缓存 [预计 4-5 天]

> 对应原系统第 9 章。最复杂的模块。

### 8.1 缓存策略

**文件**: `crates/cdn-cache/src/strategy.rs`

请求可缓存性判断 + 规则匹配 (path > extension > mimetype > regex > default) + 响应可缓存性 + TTL 调整。

### 8.2 缓存 Key 生成

**文件**: `crates/cdn-cache/src/key.rs`

MD5(site_id:host:uri:sorted_args:vary_values)。

### 8.3 Redis 元数据存储

**文件**: `crates/cdn-cache/src/storage.rs`

读取: Redis GET → 过期检查 → OSS GET。
写入: OSS PUT → Redis SETEX。

### 8.4 OSS/S3 客户端

**文件**: `crates/cdn-cache/src/oss.rs`

AWS Signature V4 签名，PUT/GET/HEAD/DELETE。使用 `reqwest` + `ring` (HMAC-SHA256)。

### 8.5 响应体收集与异步写入

在 `response_body_filter()` 中逐块收集，EOF 时 `tokio::spawn` 异步写入。

### 8.6 缓存命中响应

在 `request_filter()` 中查询缓存，命中时直接写响应 + `Ok(true)` 短路。

### 验证

- 单元测试: 策略判断、Key 生成、TTL 调整
- 集成测试: 缓存 MISS → 写入 → 缓存 HIT、OSS 读写

---

## Phase 9: SSL/TLS 证书管理 [预计 3-4 天]

> 对应原系统第 10 章。

### 9.1 动态证书选择

**文件**: `crates/cdn-proxy/src/ssl/manager.rs`

TlsAcceptCallbacks 实现，SNI → 证书查找 (缓存 → etcd → 通配符 → 自定义 → 默认)。

### 9.2 ACME 客户端

**文件**: `crates/cdn-proxy/src/ssl/acme.rs`

`instant-acme` crate，4 提供商 + EAB + 多提供商轮询。

### 9.3 HTTP-01 挑战

**文件**: `crates/cdn-proxy/src/ssl/challenge.rs`

`request_filter()` 中拦截 `/.well-known/acme-challenge/`。

### 9.4 证书存储

**文件**: `crates/cdn-proxy/src/ssl/storage.rs`

etcd 主存储 + 文件备份。

### 9.5 自动续期

**文件**: `crates/cdn-proxy/src/ssl/renewal.rs`

BackgroundService，两级分布式锁，启动 60s 后 + 每天。

### 验证

- 单元测试: 证书 PEM 解析、通配符匹配
- 集成测试: ACME staging 环境证书申请（Let's Encrypt staging）

---

## Phase 10: 请求/响应头处理 [预计 1 天]

> 对应原系统第 11 章。

### 10.1 请求头

**文件**: `crates/cdn-middleware/src/headers/request.rs`

`upstream_request_filter()` 中执行: 自动头 (XFF/Proto/RequestID) + 自定义规则 (set/add/remove/append) + 变量替换。

### 10.2 响应头

**文件**: `crates/cdn-middleware/src/headers/response.rs`

`response_filter()` 中执行: 敏感头移除 + 自定义规则 + 自动头 (X-Cache-Status/X-Request-ID)。

### 验证

- 单元测试: 变量替换、四种操作
- 集成测试: 配置头规则 → 请求 → 验证请求/响应头

---

## Phase 11: 日志与监控 [预计 2-3 天]

> 对应原系统第 12 章。

### 11.1 Prometheus 指标定义

**文件**: `crates/cdn-proxy/src/logging/metrics.rs`

全部 Counter/Gauge/Histogram 指标注册。

### 11.2 请求日志收集

**文件**: `crates/cdn-proxy/src/logging/mod.rs`

`logging()` 回调: 收集日志 → 记录指标 → 被动健康检查 → 异步 Redis Streams。

### 11.3 Redis Streams 推送

**文件**: `crates/cdn-proxy/src/logging/queue.rs`

`tokio::spawn` 异步 XADD，MAXLEN ~ 100000。

### 11.4 定时指标收集

BackgroundService 每 15s: 上游健康数、证书过期、CC 封禁数。

### 验证

- 集成测试: 请求 → 验证 Prometheus /metrics 输出
- 集成测试: 请求 → 验证 Redis Stream 中有日志

---

## Phase 12: 工具库与管理接口 [预计 2-3 天]

> 对应原系统第 13、14、15 章。

### 12.1 Redis 连接管理

**文件**: `crates/cdn-proxy/src/utils/redis_pool.rs`

ConnectionManager 封装，Sentinel/Standalone 双模式。

### 12.2 IP 工具

**文件**: `crates/cdn-proxy/src/utils/ip.rs`

XFF 防伪造 (从右向左遍历可信代理)。

### 12.3 分布式锁

**文件**: `crates/cdn-proxy/src/utils/lock.rs`

Redis SETNX + Lua 脚本原子释放。

### 12.4 Admin API

**文件**: `crates/cdn-proxy/src/admin/mod.rs`, `endpoints.rs`

Axum on 127.0.0.1:8080:
- POST /reload
- POST /ssl/clear-cache
- GET /site/{id}
- GET /upstream/health
- GET /cc/blocked

### 12.5 公开/内网端点

在 `request_filter()` 中拦截:
- /health (公开)
- /health/detail, /status (内网 IP 检查)

### 验证

- 集成测试: Admin API 各端点
- 集成测试: 分布式锁获取/释放/过期

---

## Phase 13: 集成测试与 Docker [预计 2-3 天]

### 13.1 端到端测试

- 完整请求流程: 客户端 → CDN → 源站 → 响应
- 缓存流程: MISS → 写入 → HIT
- WAF 拦截 → 403
- CC 封禁 → 429
- 跳转 → 301/302
- WebSocket 升级
- gRPC 代理

### 13.2 Docker 生产构建

- 更新 Dockerfile 多阶段构建
- docker-compose.yml 生产 profile
- 健康检查配置

### 13.3 配置文件

- `config/default.yaml` 更新为完整配置
- `.env.example` 所有环境变量示例

---

## 总时间估算

| Phase | 内容 | 预计天数 |
|-------|------|---------|
| 0 | 基础设施与依赖 | 1-2 |
| 1 | 核心类型与配置系统 | 3-4 |
| 2 | 请求路由与上下文 | 1-2 |
| 3 | WAF | 2-3 |
| 4 | CC 防护 | 2-3 |
| 5 | 跳转引擎 | 1-2 |
| 6 | 多协议代理 | 3-4 |
| 7 | 负载均衡与健康检查 | 2-3 |
| 8 | 内容缓存 | 4-5 |
| 9 | SSL/TLS 证书管理 | 3-4 |
| 10 | 请求/响应头处理 | 1 |
| 11 | 日志与监控 | 2-3 |
| 12 | 工具库与管理接口 | 2-3 |
| 13 | 集成测试与 Docker | 2-3 |

---

## 依赖关系

```
Phase 0 (基础设施)
  └── Phase 1 (类型与配置) ← 所有后续 Phase 依赖
        ├── Phase 2 (路由) ← Phase 3-6 依赖
        │     ├── Phase 3 (WAF)
        │     ├── Phase 4 (CC)
        │     ├── Phase 5 (跳转)
        │     └── Phase 6 (多协议)
        ├── Phase 7 (负载均衡) ← Phase 6 部分依赖
        ├── Phase 8 (缓存)
        ├── Phase 9 (SSL/TLS)
        ├── Phase 10 (头处理)
        ├── Phase 11 (日志监控)
        └── Phase 12 (工具库/Admin)
              └── Phase 13 (集成测试)
```

Phase 3-5 (WAF/CC/跳转) 可并行开发。
Phase 7-11 在 Phase 2 完成后可并行开发。
