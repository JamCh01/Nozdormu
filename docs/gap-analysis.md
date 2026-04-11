# Nozdormu CDN 与现代化 CDN 差距分析报告

## 项目现状概览

Nozdormu 是基于 Cloudflare Pingora 框架构建的高性能 CDN 反向代理，从 OpenResty/Lua 栈迁移而来。当前已实现 7 个 crate、432 个单元/集成测试、79 个 E2E 测试。核心能力包括：WAF（IP/GeoIP/ASN/Body 检测）、CC 限速（混合本地+Redis 计数）、缓存（OSS/S3 + Redis 元数据）、SSL/TLS 管理（ACME 脚手架）、多协议支持（WebSocket/SSE/gRPC）、动态配置（etcd 热加载）、视频流优化（URL 签名/MP4→HLS 动态封装/智能预取）、图片优化（实时 resize/格式转换）。

---

## 一、基础加速能力

### 1.1 覆盖范围与节点架构

| 维度 | 项目现状 | 现代化 CDN 标准 | 差距 |
|------|---------|----------------|------|
| 节点布局 | 单节点部署，无分层架构 | 全球 200+ PoP，边缘→区域→核心三层架构（Cloudflare 330+ 城市，Akamai 4000+ 节点） | **缺失** |
| 智能调度 | 无 DNS 调度层，依赖外部 DNS | 基于 Anycast + GeoDNS + 实时网络质量的智能路由（RTT、丢包率、节点负载） | **缺失** |
| 多节点协同 | etcd 配置同步，Redis 共享状态 | 节点间缓存协同（Cache Mesh/Tiered Cache）、请求合并（Request Coalescing） | **部分缺失** |

**优先级：中**

**优化建议：**
- **短期**：Nozdormu 作为单节点代理，节点编排应交给外部基础设施（Kubernetes + 外部 DNS 调度如 Route53/Cloudflare DNS）。当前架构合理。
- **中期**：实现 **Tiered Cache**（分层缓存），支持边缘节点 miss 时回源到区域中心节点而非直接回源站。可在 `cdn-cache` 中增加 `mid-tier origin` 概念。
- **中期**：实现 **Request Coalescing**（请求合并），多个并发 cache miss 请求只回源一次。Pingora 原生支持此特性（`cache_miss_handler`），需要接入。

---

### 1.2 协议支持

| 维度 | 项目现状 | 现代化 CDN 标准 | 差距 |
|------|---------|----------------|------|
| HTTP/1.1 | ✅ 完整支持 | 基线 | 无 |
| HTTP/2 | ✅ 通过 Pingora 支持（gRPC 强制 H2） | 基线 | 无 |
| **HTTP/3 (QUIC)** | ❌ 未实现 | Cloudflare/Akamai/阿里云均已全面支持，移动端提升 30%+ 首屏速度 | **缺失** |
| TLS 1.3 | ✅ 通过 OpenSSL 支持 | 基线 | 无 |
| **0-RTT** | ❌ 未配置 | TLS 1.3 Early Data + QUIC 0-RTT，减少握手延迟 | **缺失** |
| WebSocket | ✅ RFC 6455 完整验证 | 基线 | 无 |
| SSE | ✅ 完整支持（禁压缩、no-cache、透传 Last-Event-ID） | 基线 | 无 |
| gRPC | ✅ 支持 Native/Web/WebText 三种变体 | 高级（多数 CDN 仅支持 gRPC-Web） | **领先** |

**优先级：高**

**优化建议：**
- **高优先**：接入 **HTTP/3 (QUIC)**。Pingora 0.8 已实验性支持 QUIC（需 `boringssl` feature），但尚不稳定。建议：
  1. 短期：在 Pingora 上游跟踪 QUIC 进展，准备 feature flag
  2. 中期：当 Pingora QUIC 稳定后，添加 `quic` feature 到 Cargo.toml，配置 UDP 监听
  3. 替代方案：前置 Nginx/Caddy 做 QUIC 终结，Nozdormu 作为 H2 后端
- **中优先**：启用 **TLS 1.3 0-RTT**（Early Data）。需在 SSL 配置中设置 `SSL_CTX_set_max_early_data`，并在 `request_filter` 中对 0-RTT 请求标记为 replay-safe（仅允许 GET/HEAD 幂等请求）

---

### 1.3 缓存策略

| 维度 | 项目现状 | 现代化 CDN 标准 | 差距 |
|------|---------|----------------|------|
| 缓存规则 | ✅ 路径正则匹配、自定义 TTL、per-site 规则 | 基线 | 无 |
| 缓存键 | ✅ MD5 直接哈希（site_id+host+path+query+vary），零中间分配 | 基线 | 无 |
| 缓存刷新 | ✅ URL 精确刷新（同步）+ 站点全量刷新（异步） | 基线 | 无 |
| **缓存预热** | ❌ 无通用缓存预热 API（仅有视频段预取） | 支持批量 URL 预热、定时预热任务 | **缺失** |
| **Stale-While-Revalidate** | ❌ 未实现（仅有保守 60s 回退 TTL） | 标准支持 `stale-while-revalidate` 和 `stale-if-error` 指令 | **缺失** |
| **Surrogate Keys / Cache Tags** | ❌ 未实现 | Fastly/Cloudflare 支持 `Surrogate-Key` 头，按标签批量刷新 | **缺失** |
| **Request Coalescing** | ❌ 未实现 | 并发 cache miss 合并为单次回源 | **缺失** |
| 缓存存储 | ✅ OSS/S3 + Redis 元数据 | 多数 CDN 使用本地 SSD + 内存分层 | **架构差异** |

**优先级：高**

**优化建议：**
- **高优先**：实现 **Stale-While-Revalidate**。在 `cdn-cache` 的 `CacheStrategy` 中解析 `Cache-Control: stale-while-revalidate=N` 指令，cache hit 但已过期时：立即返回 stale 响应，后台异步回源刷新。Pingora 的 cache 模块原生支持此模式。
- **高优先**：实现 **Request Coalescing**。利用 Pingora 的 `cache_miss_handler` + `cache_lock`，对同一 cache key 的并发 miss 请求排队，仅第一个回源。
- **中优先**：实现 **Cache Tags**。在 `CacheEntry` 元数据中增加 `tags: Vec<String>` 字段，从响应头 `Surrogate-Key` / `Cache-Tag` 解析。Redis 中维护 `tag → [cache_keys]` 反向索引，支持 `POST /_admin/cache/purge` 按 tag 刷新。
- **中优先**：增加 **缓存预热 API**。`POST /_admin/cache/warm` 接受 URL 列表，后台批量回源并写入缓存。

---

## 二、边缘计算与增值能力

### 2.1 边缘函数（Serverless）

| 维度 | 项目现状 | 现代化 CDN 标准 | 差距 |
|------|---------|----------------|------|
| **边缘函数运行时** | ❌ 无 | Cloudflare Workers (V8 Isolates)、阿里云 EdgeRoutine、Akamai EdgeWorkers | **缺失** |
| 自定义逻辑 | 仅通过配置规则（WAF/CC/重定向/Header 操作） | 用户可部署任意 JS/Wasm 代码在边缘执行 | **缺失** |

**优先级：低**（对 CDN 核心功能非必需，但对平台化至关重要）

**优化建议：**
- **长期**：考虑集成 **Wasm 运行时**（如 wasmtime/wasmer）作为边缘函数引擎。在 `request_filter` 和 `response_filter` 中增加 Wasm 钩子点，允许用户上传 `.wasm` 模块执行自定义逻辑。
- **替代方案**：增强现有规则引擎的表达力——支持条件组合（AND/OR/NOT）、变量提取、正则捕获组替换，覆盖 80% 的边缘函数使用场景。

---

### 2.2 安全防护

| 维度 | 项目现状 | 现代化 CDN 标准 | 差距 |
|------|---------|----------------|------|
| WAF | ✅ IP/CIDR/ASN/GeoIP 黑白名单 + Body 检测（magic bytes） | 基线 | 无 |
| CC 限速 | ✅ 混合计数（本地 moka + Redis），3 种 key 策略，4 种动作 | 基线 | 无 |
| JS Challenge | ✅ CC 挑战模式（cookie 验证） | 基线 | 无 |
| **OWASP 规则集** | ❌ 无 SQL 注入/XSS/SSRF 等规则 | Cloudflare/Akamai 内置 OWASP CRS，支持 ModSecurity 规则 | **缺失** |
| **Bot 管理** | ❌ 仅有基础 CC 限速 | 浏览器指纹、行为分析、ML 评分、CAPTCHA 集成 | **缺失** |
| **L3/L4 DDoS** | ❌ 仅 L7 防护 | 网络层 DDoS 清洗（SYN Flood、UDP Flood、反射放大） | **缺失**（需网络层基础设施） |
| SSL 证书管理 | ⚠️ ACME 脚手架已搭建，协议流程未实现 | 自动签发、自动续期、泛域名证书、多 CA 轮转 | **部分实现** |
| **mTLS** | ❌ 未实现 | 客户端证书验证（API 网关场景） | **缺失** |

**优先级：高**（ACME 和 OWASP 规则集）

**优化建议：**
- **高优先**：**完成 ACME 实现**。`acme.rs` 已有完整的 provider 配置和多 CA 轮转逻辑，需要：
  1. 使用 `instant-acme` crate 实现 HTTP-01 challenge 流程
  2. 在 `ChallengeStore` 中存储 token（已有结构）
  3. 通过 `DistributedLock`（已实现）确保集群中只有一个节点执行签发
  4. 证书存储到 etcd + 本地文件系统
  5. 实现自动续期扫描（`renewal_scan` lock 已定义）
- **高优先**：增加 **OWASP 基础规则**。在 `cdn-middleware/src/waf/` 中增加 `rules.rs` 模块：
  - SQL 注入检测（基于正则模式匹配 `UNION SELECT`、`OR 1=1` 等）
  - XSS 检测（`<script>`、`javascript:` 等）
  - Path Traversal（`../`、`%2e%2e`）
  - 不需要完整 ModSecurity 引擎，覆盖 Top 10 即可
- **中优先**：增强 **Bot 管理**。分阶段实现：
  1. User-Agent 分类（已知爬虫/搜索引擎/自动化工具）
  2. 请求频率异常检测（基于现有 CC 引擎扩展）
  3. JS Challenge 增强（增加 PoW 难度调节）

---

### 2.3 媒体处理

| 维度 | 项目现状 | 现代化 CDN 标准 | 差距 |
|------|---------|----------------|------|
| 图片优化 | ✅ 实时 resize/crop、格式转换（JPEG/PNG/WebP/AVIF）、DPR 感知 | 接近行业标准 | 无 |
| 视频 URL 签名 | ✅ Type A/B/C，HMAC-SHA256 | 基线 | 无 |
| MP4→HLS 动态封装 | ✅ fMP4 init/media segment + m3u8 生成 | 高级功能 | 无 |
| 智能预取 | ✅ HLS/DASH manifest 解析 + 后台预取 | 高级功能 | 无 |
| **边缘转码** | ❌ 无 | 阿里云/腾讯云支持边缘视频转码、截图、水印 | **缺失** |
| **自适应码率 (ABR)** | ❌ 仅静态 HLS 封装 | 基于客户端带宽的动态码率切换 | **缺失** |
| **DASH 动态封装** | ❌ 仅 HLS | 同时支持 HLS + DASH | **部分缺失** |

**优先级：中**

**优化建议：**
- **中优先**：增加 **DASH 动态封装**。在 `cdn-streaming/src/packaging/` 中增加 `dash_gen.rs`，复用现有 MP4 解析器，生成 DASH MPD manifest + CMAF segments。
- **低优先**：边缘转码需要 GPU/硬件加速，不适合在 Pingora 进程内实现。建议作为独立微服务部署，通过回源链路集成。

---

## 三、智能调度与运维能力

### 3.1 智能调度

| 维度 | 项目现状 | 现代化 CDN 标准 | 差距 |
|------|---------|----------------|------|
| 负载均衡 | ✅ 加权轮询、IP Hash、随机，支持 backup 故障转移 | 基线 | 无 |
| 健康检查 | ✅ 主动（HTTP/TCP 探测）+ 被动（5xx 计数），双写同一 DashMap | 高级 | 无 |
| DNS 解析 | ✅ hickory-resolver + moka 缓存（60s TTL） | 基线 | 无 |
| GeoIP | ✅ MaxMindDB（City/Country/ASN） | 基线 | 无 |
| **最少连接 LB** | ✅ AtomicU32 per-origin 活跃连接计数 | 基于活跃连接数的负载均衡 | **已完成** |
| **一致性哈希 LB** | ✅ Ketama 哈希环（SipHash + 加权虚拟节点） | Ketama 一致性哈希，节点变更时最小化缓存失效 | **已完成** |
| **动态权重调整** | ✅ 滑动窗口 P99 延迟 + 错误率自适应权重（per-origin） | 基于响应时间/错误率自动调整权重 | **已完成** |
| **多源站 failover** | ✅ backup origin 支持 | 基线 | 无 |

**优先级：中**

**优化建议：**
- ~~**中优先**：增加 **Least Connections** 算法。~~ ✅ 已完成（AtomicU32 per-origin 活跃连接计数，DashMap 存储，权重 tie-break）
- ~~**中优先**：增加 **一致性哈希**。~~ ✅ 已完成（Ketama 哈希环，SipHash + 40 vnodes/weight，源站增减仅 1/N 重映射）
- ~~**低优先**：实现 **动态权重**。~~ ✅ 已完成（滑动窗口 P99 延迟 + 错误率，线性惩罚因子 [0.1, 1.0]，per-site 可配置阈值，默认关闭）

---

### 3.2 监控与可观测性

| 维度 | 项目现状 | 现代化 CDN 标准 | 差距 |
|------|---------|----------------|------|
| Prometheus 指标 | ✅ 16 个 Counter + 8 个 Histogram + 1 个 Gauge，覆盖全链路 | 优秀 | 无 |
| 请求日志 | ✅ Redis Streams（XADD，批量写入，背压丢弃） | 基线 | 无 |
| **日志多后端** | ❌ 仅 Redis Streams | Kafka、Elasticsearch、S3、Datadog、Splunk 等 | **缺失** |
| **实时分析仪表盘** | ❌ 无内置 | Cloudflare Analytics、阿里云 CDN 实时监控 | **缺失** |
| **告警系统** | ❌ 无内置告警 | 基于阈值的自动告警（错误率、延迟、带宽） | **缺失** |
| **分布式追踪** | ❌ 无 OpenTelemetry/Jaeger 集成 | 全链路追踪（请求 ID 跨节点传播） | **缺失** |
| **Access Log 格式** | 仅 JSON 到 Redis | 可配置格式（Apache/Nginx 兼容）、实时流式输出 | **部分缺失** |

**优先级：中**

**优化建议：**
- **中优先**：增加 **日志多后端支持**。将 `logging/queue.rs` 抽象为 `LogSink` trait，实现 `RedisStreamSink`（现有）、`KafkaSink`、`FileSink`、`StdoutSink`。通过全局配置选择后端。
- **中优先**：集成 **OpenTelemetry**。在 `ProxyCtx` 中增加 trace context 传播（`traceparent` / `tracestate` 头），支持导出到 Jaeger/Zipkin。
- **低优先**：告警和仪表盘建议通过外部系统实现（Grafana + Alertmanager），Nozdormu 只需确保 Prometheus 指标完整（已满足）。

---

### 3.3 自动化运维

| 维度 | 项目现状 | 现代化 CDN 标准 | 差距 |
|------|---------|----------------|------|
| 配置热加载 | ✅ ArcSwap + etcd watch，原子切换 | 优秀 | 无 |
| 故障自动切换 | ✅ 主动+被动健康检查，自动摘除/恢复 | 基线 | 无 |
| **灰度发布** | ❌ 无 | 按百分比/区域/用户分组灰度切换配置 | **缺失** |
| **配置版本管理** | ❌ etcd 无版本历史 UI | 配置变更审计、回滚、diff 对比 | **缺失** |
| **自动扩缩容** | ❌ 无 | 基于流量/CPU 的自动扩缩（K8s HPA） | **缺失**（需外部编排） |
| CI/CD | ✅ GitHub Actions（CI + Release，4 目标平台含 musl） | 基线 | 无 |

**优先级：低**

**优化建议：**
- **中优先**：增加 **配置版本管理**。在 etcd 中存储配置时附加版本号和时间戳，`/_admin/config/history` API 返回变更历史，支持 `/_admin/config/rollback/{version}` 回滚。
- **低优先**：灰度发布可通过 etcd 配置中增加 `traffic_split` 字段实现，在 `request_filter` 中按比例路由到不同配置版本。

---

## 四、业务场景适配

### 4.1 新兴场景支持

| 场景 | 项目现状 | 现代化 CDN 标准 | 差距 |
|------|---------|----------------|------|
| 静态加速 | ✅ 完整 | 基线 | 无 |
| 动态加速 | ✅ 反向代理 + 负载均衡 | 基线 | 无 |
| 视频点播 | ✅ MP4→HLS + 预取 + URL 签名 | 高级 | 无 |
| 图片处理 | ✅ 实时优化 + 格式协商 | 高级 | 无 |
| **直播** | ❌ 无 RTMP/SRT 接入 | 低延迟直播（WebRTC/LL-HLS）、连麦、转推 | **缺失** |
| **全站加速 (DCDN)** | ⚠️ 基础反向代理，无动态路由优化 | 智能路由 + 协议优化 + 连接复用 + 预建连接池 | **部分缺失** |
| **IoT** | ❌ 无 | MQTT 代理、小包优化、设备认证 | **缺失** |
| **AI 推理** | ❌ 无 | 边缘 AI 模型部署（Cloudflare AI） | **缺失** |

**优先级：因业务而异**

**优化建议：**
- **如果面向视频业务**：优先实现 **LL-HLS**（Low-Latency HLS），在现有 HLS 封装基础上增加 `#EXT-X-PART` 和 `#EXT-X-PRELOAD-HINT` 支持。
- **如果面向全站加速**：优先实现 **连接池预热** 和 **动态路由优化**（基于 RTT 选择最优回源路径）。

---

### 4.2 定制化与开放能力

| 维度 | 项目现状 | 现代化 CDN 标准 | 差距 |
|------|---------|----------------|------|
| Admin API | ✅ 9 个端点，Bearer 认证，OpenAPI 3.1 + Swagger UI | 优秀 | 无 |
| **API 版本管理** | ❌ 无版本前缀 | `/v1/`, `/v2/` 版本化 API | **缺失** |
| **自定义错误页** | ❌ 硬编码文本响应 | 用户可配置 HTML 错误页（per-site, per-status-code） | **缺失** |
| **Webhook 通知** | ❌ 无 | 证书到期、健康状态变更、缓存刷新完成等事件通知 | **缺失** |
| **Terraform Provider** | ❌ 无 | IaC 管理 CDN 配置 | **缺失** |
| **SDK/CLI** | ❌ 无客户端 SDK | 多语言 SDK（Go/Python/JS）+ CLI 工具 | **缺失** |

**优先级：中**

**优化建议：**
- **高优先**：实现 **自定义错误页**。在 `SiteConfig` 中增加 `error_pages: HashMap<u16, String>`（URL 或内联 HTML），`serve_error` 方法优先使用配置的页面。
- **中优先**：增加 **Webhook 通知**。在关键事件（证书续期、健康状态变更、缓存刷新完成）时，POST 到用户配置的 webhook URL。
- **中优先**：**API 版本化**。将 `/_admin/` 迁移到 `/_admin/v1/`，保持向后兼容。

---

## 五、优先级总览

### 🔴 高优先级（影响核心竞争力）

| # | 差距项 | 维度 | 预估工作量 | 依赖 |
|---|--------|------|-----------|------|
| 1 | **完成 ACME 证书自动签发** | 安全 | 2-3 周 | instant-acme, ChallengeStore, DistributedLock（均已就绪） |
| 2 | **Stale-While-Revalidate** | 缓存 | 1 周 | Pingora cache 模块 |
| 3 | **Request Coalescing** | 缓存 | 1 周 | Pingora cache_lock |
| 4 | **OWASP 基础规则集** | 安全 | 2 周 | 无 |
| 5 | **自定义错误页** | 定制化 | 3 天 | SiteConfig 扩展 |
| 6 | **HTTP/3 (QUIC)** | 协议 | 取决于 Pingora 上游 | Pingora QUIC 稳定性 |

### 🟡 中优先级（提升产品完整度）

| # | 差距项 | 维度 | 预估工作量 |
|---|--------|------|-----------|
| 7 | Cache Tags / Surrogate Keys | 缓存 | 1-2 周 |
| 8 | 缓存预热 API | 缓存 | 3 天 |
| 9 | Bot 管理（UA 分类 + 行为检测） | 安全 | 2 周 |
| 10 | Least Connections / 一致性哈希 LB | 调度 | 1 周 |
| 11 | 日志多后端（Kafka/File/Stdout） | 可观测 | 1 周 |
| 12 | OpenTelemetry 分布式追踪 | 可观测 | 1-2 周 |
| 13 | DASH 动态封装 | 媒体 | 1-2 周 |
| 14 | 配置版本管理与回滚 | 运维 | 1 周 |
| 15 | Webhook 事件通知 | 定制化 | 3 天 |
| 16 | API 版本化 (v1) | 定制化 | 2 天 |
| 17 | Tiered Cache（分层缓存） | 缓存 | 2-3 周 |
| 18 | TLS 0-RTT Early Data | 协议 | 3 天 |

### 🟢 低优先级（锦上添花 / 长期规划）

| # | 差距项 | 维度 | 备注 |
|---|--------|------|------|
| 19 | 边缘函数 (Wasm) | 边缘计算 | 平台化方向，需大量投入 |
| 20 | 动态权重自动调整 | 调度 | 基于延迟/错误率 |
| 21 | 灰度发布 | 运维 | 配置级流量分割 |
| 22 | Terraform Provider | 定制化 | IaC 生态 |
| 23 | 多语言 SDK | 定制化 | Go/Python/JS |
| 24 | 直播 (LL-HLS/WebRTC) | 场景 | 视频业务方向 |
| 25 | mTLS 客户端证书 | 安全 | API 网关场景 |
| 26 | L3/L4 DDoS 防护 | 安全 | 需网络层基础设施 |

---

## 六、总体评估

### 项目优势（领先或持平现代 CDN 的领域）

- ✅ **多协议支持**：WebSocket + SSE + gRPC（含 3 种变体），超过多数商业 CDN
- ✅ **视频流优化**：MP4→HLS 动态封装 + 智能预取 + 3 种 URL 签名，功能完整
- ✅ **图片优化**：实时处理 + AVIF/WebP 自动协商 + DPR 感知，接近 Cloudflare Images
- ✅ **配置热加载**：ArcSwap + etcd watch，零停机配置更新
- ✅ **Prometheus 指标**：25 个指标覆盖全链路，可观测性基础扎实
- ✅ **工程质量**：432 单元测试 + 79 E2E 测试 + CI/CD + OpenAPI 文档

### 核心差距（需优先补齐）

- ❌ **ACME 未完成**：证书自动化是生产部署的硬性前提
- ❌ **缓存高级特性缺失**：SWR、Request Coalescing、Cache Tags 是现代 CDN 标配
- ❌ **WAF 规则薄弱**：仅有 IP/GeoIP 规则，缺少 OWASP Top 10 防护
- ❌ **HTTP/3 缺失**：移动端和弱网环境的关键加速协议

### 成熟度评估

以 Cloudflare 为 100 分基准：

| 维度 | 得分 | 说明 |
|------|------|------|
| 基础代理 | 85/100 | 协议支持完整，缺 QUIC |
| 缓存 | 60/100 | 基础完整，缺高级特性 |
| 安全 | 45/100 | WAF 基础薄弱，ACME 未完成 |
| 媒体处理 | 80/100 | 图片+视频功能丰富 |
| 可观测性 | 70/100 | 指标完整，缺追踪和多后端 |
| 边缘计算 | 0/100 | 无 |
| 运维自动化 | 65/100 | 热加载优秀，缺版本管理 |
| **综合** | **58/100** | 核心功能扎实，需补齐安全和缓存高级特性 |
