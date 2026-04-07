---
name: Chapter 11 - Header Processing Design
description: Request/response header rules (set/add/remove/append), variable substitution, auto headers (XFF, X-Request-ID), sensitive header removal
type: reference
---

## 请求头 → upstream_request_filter()

1. **自动头** (始终执行, 在自定义规则之前):
   - X-Forwarded-For: 追加 remote_addr
   - X-Forwarded-Proto: 原始协议
   - X-Request-ID: 请求唯一标识

2. **自定义规则** (headers.request 配置):
   - set: 设置/覆盖
   - add: 仅在不存在时添加
   - remove: 移除
   - append: 追加 (逗号分隔或数组)

## 响应头 → response_filter()

1. **敏感头移除** (始终执行, 不可覆盖):
   - 移除 X-Powered-By
   - Server 覆盖为 "CDN"

2. **自定义规则** (headers.response 配置): 同上四种操作

3. **自动头** (仅在未被自定义规则设置时):
   - X-Cache-Status: HIT/MISS/BYPASS
   - X-Request-ID

## 变量替换 ${variable}

请求变量: client_ip, request_id, host, uri, scheme, node_group_id, node_id, site_id
响应变量: 以上 + cache_status, upstream_addr, upstream_status, upstream_response_time

## 数据结构

```rust
struct HeaderRule {
    action: HeaderAction,  // set | add | remove | append
    name: String,
    value: Option<String>,
}
struct HeadersConfig {
    request: Option<Vec<HeaderRule>>,
    response: Option<Vec<HeaderRule>>,
}
```
