---
name: Chapter 6 - Redirect Engine Design
description: Three-tier redirect engine (domain→protocol→URL rules), 4 URL match types, variable substitution, query preservation
type: reference
---

## 三级优先级

1. **域名跳转** (最高) — old.example.com → new.example.com
2. **协议跳转** — HTTP → HTTPS (force_https)
3. **URL 规则跳转** — /old-path → /new-path

在 `request_filter()` 阶段, WAF/CC 之后执行。命中时直接写 Location 响应头 + 状态码, `Ok(true)` 短路。

## 协议跳转

- ACME 挑战路径 `/.well-known/acme-challenge/` 必须排除 (避免证书签发死锁)
- `https_exclude_paths` 配置排除路径
- force_https + force_http 同时启用时优先 HTTPS
- 非标准端口保留在 URL 中

## URL 规则 — 4 种匹配类型

| 类型 | 匹配方式 | 捕获 |
|------|---------|------|
| exact | URI 完全一致 | 无 |
| prefix | URI 前缀匹配 | $1 = 剩余部分 |
| regex | 正则匹配 (`regex` crate) | $1, $2... 捕获组 |
| domain | Host 匹配 (支持通配符) | $1 = 子域名部分 |

正则编译后缓存在 SiteConfig 加载时 (等同原系统 "jo" 选项)。

## 变量替换

按长度降序替换: `$request_uri`, `$query_string`, `$server_name`, `$scheme`, `$host`, `$uri`, `$args`
正则捕获组: `$1`, `$2`...

## 查询参数保留

默认 `preserve_query_string=true`: 目标有 `?` 用 `&` 连接, 否则用 `?`。

## 域名跳转

```rust
struct DomainRedirect {
    enabled: bool,
    target_domain: String,
    source_domains: Vec<String>,  // 空 = 所有非目标域名都跳转
    status_code: u16,             // 默认 301
}
```

- 已是目标域名 → 不跳转
- source_domains 支持 `*.old.com` 通配符后缀匹配
- 保留原始协议和路径

## 规则附加功能

- `methods`: 限制 HTTP 方法 (未配置 = 所有方法)
- `enabled`: 单条规则启用/禁用
- `cache_control`: 自定义 Cache-Control
- `response_headers`: 自定义响应头
- 状态码: 301/302/303/307/308
