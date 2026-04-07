---
name: OpenResty to Pingora Migration Mapping
description: Complete mapping of OpenResty concepts, libraries, and patterns to Rust/Pingora equivalents for CDN migration
type: reference
---

## 核心概念映射

| OpenResty | Rust/Pingora |
|-----------|-------------|
| Nginx worker 进程 | Tokio 线程池 (单进程多线程) |
| LuaJIT 协程 per-request | async Future per-request |
| cosocket yield/resume | `.await` on async I/O |
| `ngx.shared.DICT` | `Arc<DashMap>` / `Arc<moka::Cache>` |
| `lua-resty-lrucache` (worker 本地) | `moka` / thread-local cache |
| `ngx.ctx` (请求上下文) | `ProxyHttp::CTX` 泛型参数 |
| `ngx.var` | `Session` 方法 |
| `ngx.timer.at/every` | `tokio::spawn` + `tokio::time` |
| `ngx.thread.spawn/wait` | `tokio::spawn` / `tokio::join!` |
| `ngx.re.*` (PCRE) | `regex` crate |
| `cjson` | `serde_json` |
| APISIX/Kong 插件系统 | Trait-based 中间件链 |

## 多级缓存 (lua-resty-mlcache → Rust)

| 层级 | OpenResty | Rust |
|------|-----------|------|
| L1 (最快) | `lua-resty-lrucache` (worker 本地) | `mini-moka` / thread-local |
| L2 (共享) | `ngx.shared.DICT` | `pingora-memory-cache` RTCache / `moka::future::Cache` |
| L3 (外部) | 回调 (Redis/DB) | async 回调 (Redis/HTTP) |
| 防击穿 | `lua-resty-lock` | `RTCache` 内置 lookup coalescing / `moka` loader |

## Pingora 缓存组件

| 组件 | 用途 |
|------|------|
| `pingora-memory-cache::MemoryCache` | TinyUFO 淘汰, TTL, 线程安全 |
| `pingora-memory-cache::RTCache` | 读穿缓存, 自动填充, lookup 合并 |
| `tinyufo` | S3-FIFO + TinyLFU 淘汰算法 (比 LRU 命中率更高) |
| `pingora-lru` | 分片 LRU, 权重淘汰 |

## 限流映射

| 算法 | OpenResty | Rust |
|------|-----------|------|
| 漏桶 | `resty.limit.req` | `governor` crate (GCRA) |
| 令牌桶 | `resty.limit.traffic` | `governor` + Quota |
| 固定窗口 | `resty.limit.count` | `pingora_limits::rate::Rate` + `observe()` |
| 滑动窗口 | 自定义 | `Rate::rate_with()` + 插值函数 |
| 并发限制 | `resty.limit.conn` | `pingora_limits::inflight::Inflight` |
| 分布式限流 | Redis Lua 脚本 | Redis Lua 脚本 (同方案) |

## Pingora ProxyHttp 完整回调

```
new request
  → early_request_filter        [rewrite_by_lua]
  → request_filter              [access_by_lua, 可短路返回]
  → upstream_peer (required)    [balancer_by_lua]
  → [IO: connect]
  → connected_to_upstream / fail_to_connect
  → upstream_request_filter     [before_proxy]
  → request_body_filter         [无直接对应, WAF 用]
  → [IO: send request, read response]
  → upstream_response_filter    [header_filter_by_lua, 缓存前]
  → response_filter             [header_filter_by_lua, 缓存后]
  → upstream_response_body_filter [body_filter_by_lua, 缓存前]
  → response_body_filter        [body_filter_by_lua, 缓存后]
  → logging                     [log_by_lua]
```

### Pingora 独有回调 (OpenResty 无对应)
- `request_body_filter()` — 过滤请求体 (WAF)
- `request_cache_filter()` — 启用/配置缓存
- `cache_key_callback()` — 生成缓存 key
- `cache_hit_filter()` — 检查/失效缓存命中
- `proxy_upstream_filter()` — 缓存 miss 后决定是否继续上游
- `response_cache_filter()` — 决定上游响应是否可缓存
- `fail_to_connect()` — 连接失败处理, 决定重试
- `error_while_proxy()` — 代理中 IO 错误
- `fail_to_proxy()` — 终端错误处理
- `is_purge()` / `purge_response_filter()` — 缓存清除

## 插件系统设计 (cdn-middleware)

```rust
#[async_trait]
pub trait CdnPlugin: Send + Sync {
    fn name(&self) -> &str;
    fn priority(&self) -> i32;  // 越高越先执行
    
    async fn request_filter(...) -> Result<Option<bool>>;
    async fn upstream_peer_filter(...) -> Result<()>;
    async fn response_filter(...) -> Result<()>;
    fn response_body_filter(...) -> Result<()>;
    async fn logging(...);
}
```

- `CdnProxy` 持有 `Vec<Arc<dyn CdnPlugin>>` 按优先级排序
- 每个 ProxyHttp 回调中遍历执行
- 插件配置通过 etcd 动态加载, 用 `serde` + JSON Schema 验证

## 路由匹配设计

```rust
// 两级路由: host → URI
HashMap<String, matchit::Router<RouteConfig>>  // host → router
matchit::Router<RouteConfig>                    // 无 host 的 catch-all

// 匹配流程 (在 request_filter 或 upstream_peer 中):
1. 从 Host header 查找 host router
2. 在 host router 中匹配 URI path
3. 检查 method 约束
4. 存入 CTX 供后续阶段使用
```
