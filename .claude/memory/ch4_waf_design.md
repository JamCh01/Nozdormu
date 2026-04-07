---
name: Chapter 4 - WAF Design
description: WAF check chain (IP whitelist→blacklist→ASN→country→region), GeoIP integration, block/log modes, statistics — migrated from OpenResty waf/
type: reference
---

## 检查顺序 (严格有序)

1. **IP 白名单** → 命中立即放行 (跳过所有后续)
2. **IP 黑名单** → block 或 log
3. **ASN 黑名单** → block 或 log (需 GeoIP)
4. **国家白名单** → 不在白名单中拒绝 (无法获取国家信息时也拒绝, 安全优先)
5. **国家黑名单** → 命中拒绝
6. **地区/省份黑名单** → 命中拒绝 (需 City 数据库)

## 实现位置

`cdn-middleware/src/waf/mod.rs` — 在 `request_filter()` 阶段执行

## IP CIDR 匹配

使用 `ipnet` crate, 支持 IPv4/IPv6 + CIDR:
```rust
fn ip_in_cidrs(ip: IpAddr, cidrs: &[IpNet]) -> bool
fn ip_match_cidrs(ip: IpAddr, cidrs: &[IpNet]) -> Option<&IpNet>  // 返回匹配的 CIDR
```

## GeoIP → `maxminddb` crate

**改进**: 可同时加载多个数据库 (City + Country + ASN), 不再需要原系统的降级逻辑。

```rust
pub struct GeoIpDb {
    city: Option<maxminddb::Reader<Vec<u8>>>,
    country: Option<maxminddb::Reader<Vec<u8>>>,
    asn: Option<maxminddb::Reader<Vec<u8>>>,
}
```

请求级缓存: `ProxyCtx.geo_info: Option<GeoInfo>`, 每请求只查询一次。

## block/log 双模式

```rust
enum WafResult {
    Allow,
    Block { block_type: &'static str, reason: String },
    Log { block_type: &'static str, reason: String },
}
```

- Block → ctx.waf_blocked=true, respond_error(403), Ok(true) 短路
- Log → ctx.waf_reason 记录, 继续处理

## 统计 → Prometheus Counter

```rust
static WAF_BLOCKED: IntCounterVec = register_int_counter_vec!(
    "cdn_waf_blocked_total", "WAF blocked requests",
    &["site_id", "block_type"]  // ip, asn, country, region
);
```

## 新增依赖

- `ipnet = "2"` — IP/CIDR 匹配
- `maxminddb = "0.24"` — GeoIP2 数据库查询
