---
name: Chapter 5 - CC Protection Design
description: CC anti-DDoS — hybrid counters (moka + Redis), rule matching, JS challenge, block/challenge/log actions, manual ban API
type: reference
---

## 检查流程

1. 封禁状态检查 (moka cache, TTL 自动过期)
2. 最长路径前缀匹配规则
3. 生成计数 key (ip / ip_url / ip_path)
4. 混合计数 (本地 moka + 异步 Redis)
5. 阈值判断 → block / challenge / log

## 实现位置

`cdn-middleware/src/cc/mod.rs` — 在 `request_filter()` 阶段, WAF 之后执行

## 封禁状态 → moka::future::Cache

```rust
pub struct CcState {
    blocked: moka::future::Cache<(String, IpAddr), ()>,   // TTL 自动过期
    counters: moka::future::Cache<String, AtomicU64>,      // 本地计数器
}
```

TTL 过期自动解封, 无需清理定时器。等同于原系统 ngx.shared.cc_blocked。

## 混合计数器策略

| 本地计数 | 行为 |
|---------|------|
| 1-10 | 仅本地计数 (减少 Redis 压力) |
| 10, 20, 30... | 本地 + tokio::spawn 异步 Redis 同步 |
| 其他 | 仅本地计数 |

tokio::spawn 替代 ngx.timer.at(0, ...), 不阻塞当前请求。

## 规则匹配

最长路径前缀匹配:
```rust
fn match_rule(uri: &str, rules: &[CcRule]) -> Option<&CcRule> {
    rules.iter()
        .filter(|r| uri.starts_with(r.path.as_deref().unwrap_or("/")))
        .max_by_key(|r| r.path.as_deref().unwrap_or("/").len())
}
```

无匹配时使用默认规则 (default_rate=100, default_window=60s, default_block_duration=600s)。

## 计数 Key 类型

| 类型 | Key 格式 | 粒度 |
|------|---------|------|
| ip | `nozdormu:cc:counter:{site}:{ip}` | 全站限流 |
| ip_url | `...:{ip}:{crc32(uri)}` | 接口限流 (含查询参数) |
| ip_path | `...:{ip}:{crc32(path)}` | 路径限流 (不含查询参数) |

使用 `crc32fast` crate 替代 ngx.crc32_short()。

## JS 挑战

签发: `HMAC-SHA256(secret, "{ip}|{timestamp}")` → base64 → Set-Cookie → 503 HTML
验证: 读 cookie → base64 解码 → 检查 5 分钟有效期 → 验证 HMAC

使用 `ring` crate hmac::sign() 替代 lua-resty-hmac。

## 动作类型

| 动作 | 行为 | HTTP 状态 |
|------|------|----------|
| block | 封禁 IP + 返回响应 | 429 + Retry-After |
| challenge | JS 挑战验证 | 503 (挑战页) |
| delay | ngx.sleep → tokio::time::sleep | 继续处理 |
| log | 仅记录 | 继续处理 |

## 手动封禁/解封

通过 Admin API (待定) 暴露:
- `block_ip(site_id, ip, duration)` → moka insert with TTL
- `unblock_ip(site_id, ip)` → moka invalidate
- `is_ip_blocked(site_id, ip)` → moka contains_key
- `get_blocked_ips(site_id)` → 遍历 (需设计)

## 新增依赖

- `moka = { version = "0.12", features = ["future"] }` — TTL 缓存 (封禁 + 计数器)
- `crc32fast = "1"` — URL/路径哈希
- `ring = "0.17"` — HMAC-SHA256 (JS 挑战)
- `base64 = "0.22"` — 编解码
