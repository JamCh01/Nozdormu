# Nozdormu CDN

[![CI](https://github.com/JamCh01/Nozdormu/actions/workflows/ci.yml/badge.svg)](https://github.com/JamCh01/Nozdormu/actions/workflows/ci.yml)

基于 [Pingora](https://github.com/cloudflare/pingora) 构建的高性能 CDN 反向代理，旨在替代基于 OpenResty/Lua 的 CDN 系统。

## 功能特性

- **动态配置** -- 站点配置和集群共享设置存储在 etcd 中，通过 ArcSwap 热加载，零停机；启动参数通过 CLI 标志传入，环境变量可覆盖 etcd 配置实现单节点控制
- **WAF 防火墙** -- IP/CIDR 黑白名单（前缀树 O(log n)），GeoIP 国家/地区/ASN 过滤，国家白名单 fail-closed 模式，请求体检查（POST Body 大小限制 + 基于魔数字节的内容类型校验，使用 `infer` 库检测 200+ 文件类型）
- **CC 防护（频率限制）** -- 本地+Redis 混合计数器，JS 挑战（HMAC-SHA256），按路径规则最长前缀匹配
- **缓存** -- 双后端架构：Redis 元数据 + S3/OSS 对象存储，基于规则的 TTL（路径/扩展名/正则），Cache-Control 合规，缓存清除 API（精确 URL + 全站清除 + 按 Tag 清除），直接哈希 Key 生成（零中间分配），批量清除并发 Redis 管道；Stale-While-Revalidate（过期但在 SWR 窗口内立即返回 stale 响应，后台异步回源刷新）；Request Coalescing（同一 cache key 的并发 miss 请求排队，仅第一个回源）；Cache Tags（从 `Surrogate-Key`/`Cache-Tag` 响应头解析，Redis SET 反向索引，支持按 tag 批量清除）；缓存预热 API（`POST /_admin/cache/warm` 批量回源写入缓存）
- **图片优化** -- 实时裁剪/缩放（5 种 fit 模式），格式转换（JPEG/PNG/WebP/AVIF），质量调整，通过 `Accept` 头自动协商格式，DPR 自适应（DPR 感知尺寸限制）；纯 Rust 实现（`image` + `fast_image_resize`）
- **Range 请求** -- 客户端断点续传，`Accept-Ranges: bytes` 通告，`If-Range` 条件请求，Range 透传回源，OSS Range GET 内存高效的缓存分片服务；按站点启用/禁用
- **视频流优化** -- 三大功能：
  - **边缘鉴权（URL 签名）** -- Type A/B/C 三种 URL 签名模式（HMAC-SHA256），防盗链，可配置过期时间
  - **动态转封装** -- 源站存储 MP4，边缘实时转封装为 HLS（fMP4 分片 + m3u8 播放列表），支持 `?format=hls` 触发
  - **智能预取** -- 解析 HLS/DASH 清单文件，异步预取后续分片到缓存，原子去重，响应体大小限制（256MB），提升命中率
- **多协议支持** -- HTTP、WebSocket、SSE、gRPC（原生 + gRPC-Web），按协议独立超时和头部处理
- **负载均衡** -- 加权轮询、IP 哈希、随机；主动健康检查（HTTP/TCP 探测）+ 被动健康追踪，自动故障转移到备用源站
- **SSL/TLS** -- 多提供商 ACME（Let's Encrypt、ZeroSSL、Buypass、Google），完整 ACME v2 协议流程（HTTP-01 挑战、CSR 生成、证书下载），账户凭证 Redis 持久化，自动续期（后台服务 + Redis 分布式锁），EAB 支持；下游 TLS 监听器（动态证书选择，SNI 精确/通配符/默认回退），TLS 1.3 0-RTT Early Data（非幂等方法自动拒绝 425，上游 `Early-Data: 1` 头部 RFC 8470）
- **重定向** -- 三层引擎：域名重定向、协议强制（HTTP/HTTPS）、URL 规则（精确/前缀/正则/域名）
- **头部操作** -- 请求/响应头规则，支持变量替换（`${client_ip}`、`${host}`、`${cache_status}` 等）
- **可观测性** -- Prometheus 指标（请求/上游/健康检查/缓存清除/图片优化/流媒体/0-RTT/ACME 签发与续期/SWR 回源/请求合并/缓存预热 计数器，耗时直方图），Redis Streams 请求日志（有界通道 + 批量写入，背压保护），请求 ID 追踪，请求耗时追踪（毫秒级 Instant 计时）
- **压缩** -- gzip、Brotli、Zstandard，`Accept-Encoding` 协商；按站点配置+全局默认；WebSocket/SSE/gRPC 和不可压缩类型自动跳过；编码器错误传播（非静默吞没）
- **管理 API** -- 挂载于代理端口 `/_admin/` 路径，可对公网暴露；Bearer Token 认证（etcd `global/security` 配置），常量时间比较；配置重载、健康状态及手动覆盖、CC 状态检查、缓存清除（精确 URL + 全站后台任务 + 按 Tag 清除）、缓存预热（批量 URL 回源写入缓存）；内置 OpenAPI 3.1 规范（`/_admin/openapi.json`）和 Swagger UI（`/_admin/swagger`）

## 架构

```
crates/
  cdn-common        共享类型、错误处理、RedisOps trait
  cdn-config        节点配置、GlobalConfig（etcd）、LiveConfig（ArcSwap）、etcd 监听
  cdn-cache         缓存策略、Key 生成、S3/OSS 客户端（AWS Sig v4）、批量清除
  cdn-image         图片优化：裁剪/缩放、格式转换、质量调整、Accept 协商
  cdn-streaming     视频流优化：URL 签名鉴权、MP4→HLS 动态转封装、HLS/DASH 智能预取
  cdn-middleware     WAF 引擎（IP/GeoIP + 请求体检查）、CC 引擎、重定向引擎、头部规则
  cdn-proxy          主程序：Pingora 代理、负载均衡、DNS、SSL、主动健康探测、管理 API
```

### 请求流程

```
客户端 -> Pingora 监听器（HTTP + 可选 TLS，动态证书选择，0-RTT Early Data）
  -> 健康检查/ACME（短路返回）
  -> 管理 API（/_admin/ 路径，Bearer Token 认证，短路返回）
  -> 站点路由（域名 -> SiteConfig，精确/通配符匹配）
  -> 客户端 IP 提取（XFF 防伪造，可配置信任代理）
  -> 0-RTT 重放保护（非幂等方法返回 425 Too Early）
  -> WAF 检查（IP 前缀树 -> GeoIP -> ASN -> 国家 -> 地区）
  -> 请求体预检（Content-Length 大小限制，超限直接 413）
  -> CC 检查（封禁缓存 -> 挑战验证 -> 计数器 -> 阈值）
  -> 边缘鉴权（URL 签名验证 Type A/B/C，剥离 Token 后传递原始路径）
  -> 重定向检查（域名 -> 协议 -> URL 规则）
  -> 协议检测（gRPC > WebSocket > SSE > HTTP）
  -> 图片参数解析（w/h/fit/fmt/q/dpr 查询参数）
  -> 动态转封装检测（?format=hls 或 Accept 头协商）
  -> Range 请求处理（解析 Range 头，透传或缓存分片服务）
  -> 缓存查找（Key 生成 -> Redis 元数据 -> OSS 对象体）
  -> 负载均衡（健康过滤 -> 算法选择 -> DNS 解析 -> HttpPeer）
  -> 请求体检查（逐块大小累计 + 魔数字节内容类型校验，超限 413/403）
  -> 上游请求（头部注入，协议特定头部）
  -> 响应过滤（头部规则、缓存写入、安全头部）
  -> 动态转封装（MP4 解析 -> fMP4 分片生成 / m3u8 播放列表生成）
  -> 智能预取（解析 HLS/DASH 清单，异步预取后续分片）
  -> 图片优化（检测图片类型，协商格式，缓冲+处理）
  -> Range 响应（缓存分片或中继源站 206，跳过压缩）
  -> 压缩（gzip/Brotli/Zstd，图片和 Range 响应跳过）
  -> 日志（Prometheus 指标、Redis Streams、被动健康更新）
```

### 主动健康检查

源站通过独立于真实流量的后台服务进行探测：

- **HTTP 探测**：GET 请求到可配置路径，验证状态码（默认 200-299）
- **TCP 探测**：仅连接检查，带超时
- **按站点配置**：类型、路径、间隔、超时、阈值、期望状态码、Host 头覆盖
- **监督循环**：每 5 秒对比运行中的探测任务与实时配置；按需创建/终止任务
- **共存机制**：主动探测使用站点级阈值；被动检查使用全局阈值；两者写入同一健康状态

## 环境要求

- Rust 1.84+（stable）
- OpenSSL 开发头文件（`libssl-dev` / `openssl-devel`）
- etcd v3.5+（配置存储）
- Redis 7+ with Sentinel（可选，用于分布式 CC 计数器和日志流）
- MaxMind GeoLite2 数据库（可选，用于 WAF 地理过滤）

## 快速开始

### Docker（推荐）

```bash
# 启动所有服务（CDN + etcd 集群 + Redis Sentinel）
docker compose --profile dev up

# 生产构建
docker compose --profile prod up -d
```

### 从源码构建

```bash
# 安装依赖（Debian/Ubuntu）
apt-get install -y libssl-dev pkg-config cmake protobuf-compiler

# 启动基础设施
docker compose --profile infra up -d

# 构建并运行
cargo build --release
./target/release/cdn-proxy -c config/default.yaml \
  --env development --log-level info
```

### 验证

```bash
# 健康检查
curl http://localhost:6188/health
# -> OK

# Prometheus 指标
curl http://localhost:6190/metrics

# OpenAPI 规范（无需认证）
curl http://localhost:6188/_admin/openapi.json

# Swagger UI（浏览器打开）
# http://localhost:6188/_admin/swagger

# 管理 API（Bearer Token 认证，token 来自 etcd global/security）
curl -H "Authorization: Bearer my_admin_bearer_token" \
  http://localhost:6188/_admin/upstream/health

# 缓存清除（精确 URL）
curl -X POST http://localhost:6188/_admin/cache/purge \
  -H "Authorization: Bearer my_admin_bearer_token" \
  -H "Content-Type: application/json" \
  -d '{"type":"url","site_id":"example","host":"example.com","path":"/logo.png"}'

# 缓存清除（全站，异步）
curl -X POST http://localhost:6188/_admin/cache/purge \
  -H "Authorization: Bearer my_admin_bearer_token" \
  -H "Content-Type: application/json" \
  -d '{"type":"site","site_id":"example"}'

# 缓存清除（按 Tag，源站通过 Surrogate-Key/Cache-Tag 响应头设置 tag）
curl -X POST http://localhost:6188/_admin/cache/purge \
  -H "Authorization: Bearer my_admin_bearer_token" \
  -H "Content-Type: application/json" \
  -d '{"type":"tag","site_id":"example","tag":"product"}'

# 缓存预热（批量回源写入缓存，异步）
curl -X POST http://localhost:6188/_admin/cache/warm \
  -H "Authorization: Bearer my_admin_bearer_token" \
  -H "Content-Type: application/json" \
  -d '{"site_id":"example","urls":[{"host":"example.com","path":"/page1"},{"host":"example.com","path":"/page2"}]}'
# -> {"status":"accepted","task_id":"...","site_id":"example","urls_count":2}

# 缓存预热任务状态
curl -H "Authorization: Bearer my_admin_bearer_token" \
  http://localhost:6188/_admin/cache/warm/status

# 图片优化（缩放 + 自动格式协商）
curl -H "Accept: image/avif,image/webp,*/*" \
  "http://localhost:6188/photo.jpg?w=200&h=150&fit=cover&q=80" -o optimized.avif

# 动态转封装（MP4 -> HLS）
curl "http://localhost:6188/video.mp4?format=hls" -o playlist.m3u8
curl "http://localhost:6188/video.mp4?format=hls&segment=init" -o init.mp4
curl "http://localhost:6188/video.mp4?format=hls&segment=0" -o segment0.m4s

# URL 签名鉴权（Type B 示例）
# 签名 URL: /video.mp4?auth_key={timestamp}-{rand}-{uid}-{hmac_hash}
```

## 配置

Nozdormu 使用三层配置系统，优先级：**CLI 参数 > etcd > 默认值**。

### 第一层：启动参数（CLI）

在 etcd 可用之前需要的参数，通过命令行标志传入。运行 `cdn-proxy --help` 查看完整列表。

| 分类 | CLI 标志 |
|------|----------|
| 节点标识 | `--node-id`, `--node-labels`, `--env` |
| etcd | `--etcd-endpoints`, `--etcd-prefix`, `--etcd-username`, `--etcd-password` |
| 路径 | `--cert-path`, `--geoip-path`, `--log-path` |
| 日志级别 | `--log-level` |

### 第二层：集群共享（etcd 全局配置）

启动时从 etcd `{prefix}/global/*` 加载，所有节点共享。环境变量可覆盖 etcd 值用于单节点紧急覆盖。

| etcd Key | 内容 |
|----------|------|
| `{prefix}/global/redis` | Redis 模式、哨兵、主机、端口、超时、连接池 |
| `{prefix}/global/security` | WAF 模式、CC 默认值、信任代理、CC 挑战密钥、管理 Token |
| `{prefix}/global/balancer` | 负载均衡算法、重试、DNS、健康检查阈值 |
| `{prefix}/global/proxy` | 连接/发送/读取/WebSocket/SSE/gRPC 超时 |
| `{prefix}/global/cache` | OSS 端点、桶、区域、SSL、TTL、最大大小 |
| `{prefix}/global/ssl` | ACME 环境、邮箱、提供商、续期天数 |
| `{prefix}/global/logging` | Redis 日志推送、Stream 最大长度 |
| `{prefix}/global/compression` | 压缩算法、级别、最小大小、MIME 类型 |
| `{prefix}/global/image_optimization` | 图片格式、质量、最大尺寸、可优化类型 |

示例：为整个集群设置 Redis 配置：

```bash
etcdctl put /nozdormu/global/redis '{
  "mode": "sentinel",
  "sentinel": {
    "master_name": "mymaster",
    "nodes": ["sentinel1:26379", "sentinel2:26379", "sentinel3:26379"]
  },
  "password": null,
  "db": 0,
  "pool_size": 200
}'
```

如果 etcd 中没有全局 Key，系统回退到默认值（完全向后兼容）。

### 第三层：站点配置（etcd 按站点）

站点以 JSON 格式存储在 etcd `{prefix}/sites/{site_id}`。示例：

```json
{
  "site_id": "example",
  "enabled": true,
  "port": 80,
  "domains": ["example.com", "*.example.com"],
  "origins": [
    {
      "id": "origin-1",
      "host": "backend.example.com",
      "port": 443,
      "protocol": "https",
      "weight": 10
    }
  ],
  "load_balancer": {
    "algorithm": "round_robin",
    "retries": 2,
    "health_check": {
      "enabled": true,
      "type": "http",
      "path": "/health",
      "interval": 10,
      "timeout": 5,
      "healthy_threshold": 2,
      "unhealthy_threshold": 3
    }
  },
  "cache": {
    "enabled": true,
    "default_ttl": 3600,
    "rules": [
      { "type": "extension", "match": ["js", "css", "png"], "ttl": 86400 },
      { "type": "path", "match": "/api", "ttl": 0 }
    ]
  },
  "waf": {
    "enabled": true,
    "mode": "block",
    "rules": {
      "ip_blacklist": ["192.168.0.0/16"],
      "country_whitelist": ["US", "JP", "DE"]
    },
    "body_inspection": {
      "enabled": true,
      "max_body_size": 26214400,
      "allowed_content_types": ["image/*", "application/pdf"],
      "blocked_content_types": ["application/x-executable"],
      "inspect_methods": ["POST", "PUT", "PATCH"]
    }
  },
  "cc": {
    "enabled": true,
    "default_rate": 100,
    "default_window": 60,
    "rules": [
      { "path": "/api/login", "rate": 5, "window": 60, "action": "challenge" }
    ]
  },
  "streaming": {
    "auth": {
      "enabled": true,
      "auth_type": "b",
      "auth_key": "your-secret-key",
      "expire_time": 1800
    },
    "dynamic_packaging": {
      "enabled": true,
      "segment_duration": 6.0,
      "max_mp4_size": 2147483648
    },
    "prefetch": {
      "enabled": true,
      "prefetch_count": 3,
      "concurrency_limit": 4
    }
  },
  "compression": {
    "enabled": true,
    "algorithms": ["zstd", "brotli", "gzip"]
  },
  "image_optimization": {
    "enabled": true,
    "formats": ["avif", "webp"],
    "default_quality": 80
  },
  "range": {
    "enabled": true,
    "chunk_size": 4194304
  }
}
```

配置变更通过 etcd watch 自动生效（无需重启）。也可通过管理 API 手动重载：

```bash
curl -X POST http://localhost:6188/_admin/reload \
  -H "Authorization: Bearer my_admin_bearer_token"
```

详见 [`docs/global/`](docs/global/) 全局配置示例和 [`docs/site/`](docs/site/) 站点配置示例。

## 端口

| 端口 | 服务 |
|------|------|
| 6188 | HTTP 代理 + 管理 API（`/_admin/` 路径，Bearer Token 认证） |
| 6189 | HTTPS/TLS 代理（可选，需配置 `tls_listen` + 证书） |
| 6190 | Prometheus 指标 |

## 开发

```bash
# 运行单元/集成测试（449 个测试）
cargo test

# 代码检查
cargo clippy --workspace

# 格式化
cargo fmt --all

# 开发模式热重载（需要 cargo-watch）
cargo watch -x "run -p cdn-proxy -- -c config/default.yaml"
```

### E2E 功能测试

端到端测试使用真实基础设施（etcd、Redis、Python 后端、GeoIP）运行完整代理：

```bash
# 启动基础设施 + 后端 + 代理
bash tests/e2e/setup.sh

# 运行全部 79 个测试（WAF、CC、缓存、负载均衡、压缩、重定向、协议、头部、管理 API、跨功能）
bash tests/e2e/run_tests.sh

# 运行特定测试组
bash tests/e2e/run_tests.sh waf cc compress

# 停止所有服务
bash tests/e2e/teardown.sh
```

详见 [CLAUDE.md](CLAUDE.md) 开发指南。

### CI/CD

项目使用 GitHub Actions 进行持续集成和发布：

- **CI**（`push`/`PR` 到 `main`）：check → clippy → fmt → test → build release
- **Release**（推送 `v*` tag）：构建 x86_64 Linux 二进制（glibc + musl 静态链接），创建 GitHub Release

```bash
# 发布新版本
git tag v0.2.1
git push origin v0.2.1
# GitHub Actions 自动构建并创建 Release
```

也可从 [Releases](https://github.com/JamCh01/Nozdormu/releases) 页面下载预编译二进制。

## 许可证

内部项目，未发布。
