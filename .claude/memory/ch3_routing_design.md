---
name: Chapter 3 - Request Routing Design
description: Domain matching (exact + wildcard), request context (ProxyCtx), context lifecycle across phases — migrated from OpenResty router.lua
type: reference
---

## 域名匹配 (router.lua → LiveConfig 查询)

纯内存操作，从 ArcSwap<LiveConfig> 读取:

1. 标准化: 端口剥离 + 小写
2. 精确匹配: `domain_index.get(host)` → site_id
3. 通配符匹配: `foo.example.com` → 查 `*.example.com` (仅单级子域)
4. 站点启用检查: `enabled == false` → 拒绝
5. 未匹配 → 404

## 请求上下文 (ngx.ctx → ProxyCtx)

原系统 ngx.ctx 在 ngx.exec 后丢失，需要 ngx.var.site_id 回退。
**Pingora 中不存在此问题** — CTX 贯穿整个请求生命周期。

```rust
pub struct ProxyCtx {
    // access 阶段
    pub client_ip: IpAddr,
    pub site_config: Arc<SiteConfig>,
    pub site_id: String,
    pub geo_info: Option<GeoInfo>,

    // WAF/CC 结果
    pub waf_blocked: bool,
    pub waf_reason: Option<String>,
    pub cc_blocked: bool,
    pub cc_reason: Option<String>,

    // content 阶段
    pub protocol_type: ProtocolType,     // HTTP | WebSocket | SSE | gRPC
    pub cache_status: CacheStatus,       // HIT | MISS | BYPASS | EXPIRED
    pub cache_key: Option<String>,
    pub cache_ttl: Option<u64>,
    pub resolved_ips: HashMap<String, IpAddr>,

    // balancer 阶段
    pub selected_origin: Option<OriginConfig>,
    pub balancer_tried: u32,

    // body_filter 阶段
    pub response_body: Option<Vec<u8>>,
    pub response_body_size: usize,
}
```

## 各阶段上下文使用

| 阶段 | 读取 | 写入 |
|------|------|------|
| request_filter | — | client_ip, site_config, site_id, waf/cc 结果 |
| (缓存/协议检测) | site_config | protocol_type, cache_status, cache_key, resolved_ips |
| upstream_peer | site_config, resolved_ips | selected_origin, balancer_tried |
| response_filter | site_config, cache_status | — |
| response_body_filter | site_config, cache_status, cache_key | response_body, response_body_size |
| logging | 全部读取 | — |
