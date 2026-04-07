---
name: OpenResty Ecosystem Reference
description: Complete OpenResty architecture, request phases, core/third-party lua-resty libraries, ngx.shared.DICT, and concurrency model for migration reference
type: reference
---

## 架构

- Nginx (C 核心) + LuaJIT 2.1 + ngx_lua 模块
- 多 worker 进程模型，每个 worker 独立 LuaJIT VM
- 协程 per-request (cosocket yield/resume = Rust async/await)
- worker 间共享: 仅 `ngx.shared.DICT` (mmap 共享内存 + slab 分配器 + 自旋锁)

## 请求处理阶段 (执行顺序)

| 阶段 | 用途 | Pingora 等价 |
|------|------|-------------|
| `init_by_lua` | 启动初始化 (master 进程) | `fn main()` / `Server::new()` |
| `init_worker_by_lua` | worker 初始化, 定时器 | `background_service` / `tokio::spawn` |
| `ssl_certificate_by_lua` | 动态 TLS 证书选择 (SNI) | 自定义 TLS acceptor / `TlsAcceptCallbacks` |
| `set_by_lua` | 设置 Nginx 变量 (同步) | 不需要 |
| `rewrite_by_lua` | URL 重写, 请求修改 | `early_request_filter()` |
| `access_by_lua` | 认证, 限流, ACL | `request_filter()` |
| `content_by_lua` | 直接生成响应 (非代理) | `request_filter()` 返回 `Ok(true)` |
| `balancer_by_lua` | 动态上游选择 | `upstream_peer()` |
| `header_filter_by_lua` | 修改响应头 | `response_filter()` / `upstream_response_filter()` |
| `body_filter_by_lua` | 修改响应体 (逐块) | `response_body_filter()` |
| `log_by_lua` | 日志, 指标 | `logging()` |

## 核心 lua-resty 库 → Rust 映射

| OpenResty | 用途 | Rust 等价 |
|-----------|------|----------|
| `lua-resty-lrucache` | worker 本地 LRU 缓存 | `moka` / `mini-moka` / `quick_cache` |
| `lua-resty-string` | MD5/SHA/AES/随机 | `ring` / `sha2` / `aes-gcm` / `rand` |
| `lua-resty-lock` | 共享字典锁 (防缓存击穿) | `tokio::sync::Mutex` / `moka` loader |
| `lua-resty-dns` | 非阻塞 DNS | `hickory-dns` / Pingora 内置 |
| `lua-resty-upload` | 流式文件上传 | `multer` crate |
| `lua-resty-websocket` | WebSocket 协议 | `tokio-tungstenite` / `fastwebsockets` |
| `lua-resty-http` | HTTP 客户端 | `reqwest` / `hyper` |
| `lua-resty-redis` | Redis 客户端 | `redis` / `fred` crate |
| `lua-resty-mysql` | MySQL 客户端 | `sqlx` / `sea-orm` |
| `lua-resty-kafka` | Kafka 生产/消费 | `rdkafka` crate |
| `lua-resty-jwt` | JWT 处理 | `jsonwebtoken` crate |
| `lua-resty-openidc` | OpenID Connect | `openidconnect` crate |
| `lua-resty-limit-traffic` | 限流 (漏桶/令牌桶/计数) | `governor` / `pingora-limits` |
| `lua-resty-balancer` | 负载均衡算法 | Pingora `LoadBalancer` 内置 |
| `lua-resty-healthcheck` | 主动/被动健康检查 | Pingora `health_check` 模块 |
| `lua-resty-mlcache` | 多级缓存 (L1/L2/L3) | `pingora-memory-cache` RTCache / `moka` 分层 |
| `lua-resty-etcd` | etcd v3 客户端 | `etcd-client` crate |
| `lua-resty-radixtree` | 基数树路由匹配 | `matchit` crate |

## ngx.shared.DICT → Rust

| 特性 | OpenResty | Rust/Pingora |
|------|-----------|-------------|
| 跨 worker 共享 | mmap 共享内存 | `Arc<DashMap>` / `Arc<moka::Cache>` (线程共享) |
| 原子递增 | `incr()` | `DashMap::entry()` / `AtomicU64` |
| TTL | 每 entry 过期 | `moka` 内置 TTL |
| LRU 淘汰 | slab 满时自动淘汰 | `moka` / `tinyufo` |
| CAS 操作 | `add()` (key 不存在才写) | `DashMap::try_insert()` |

## 关键 ngx.* API → Rust

| API | 用途 | Rust 等价 |
|-----|------|----------|
| `ngx.ctx` | 请求级上下文 | `ProxyHttp::CTX` 泛型参数 |
| `ngx.var` | Nginx 变量 | `Session` 方法 |
| `ngx.timer.at/every` | 定时任务 | `tokio::spawn` + `tokio::time` |
| `ngx.thread.spawn/wait` | 并发协程 | `tokio::spawn` / `tokio::join!` |
| `ngx.socket.tcp` | cosocket | `tokio::net::TcpStream` |
| `ngx.re.*` | PCRE 正则 | `regex` crate |
| `cjson.encode/decode` | JSON | `serde_json` |

## 关键架构差异

1. **进程 vs 线程**: OpenResty 多进程 → Pingora 多线程 (共享状态更简单, `Arc<T>` 即可)
2. **同步外观 vs 显式 async**: Lua 看起来同步但底层 yield → Rust 显式 `.await`
3. **阶段限制**: OpenResty 部分阶段禁止 cosocket → Pingora 部分回调是 `fn` 非 `async fn`
4. **热重载**: Lua 代码可热重载 → Rust 需重编译, 用配置驱动替代代码变更
5. **连接池**: OpenResty per-worker → Pingora 全局共享 (更高效)
