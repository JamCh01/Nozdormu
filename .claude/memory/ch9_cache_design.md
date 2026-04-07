---
name: Chapter 9 - Content Cache Design
description: Cache strategy, key generation, Redis+OSS dual-layer storage, response collection, async write — confirmed architecture
type: reference
---

## 缓存架构 (已确认)

**Redis 元数据 + OSS/S3 数据体** — 保持原架构, 不使用 pingora-cache (实验性)。

理由: pingora-cache 实验性不适合生产, 无本地状态便于扩缩容, 迁移风险最低。
未来可选: 在 Redis 前加 moka 进程内元数据缓存作为 L0 优化。

```
Redis: nozdormu:cache:meta:{site_id}:{key} → {status, headers, cached_at, expires_at, size, etag}
OSS:   cache/{site_id}/{key前2位}/{key}     → 响应体原始数据
```

## 缓存策略 (strategy)

### 请求可缓存性
1. cache.enabled 检查
2. 仅 GET/HEAD 可缓存
3. 请求 Cache-Control: no-cache/no-store → 不缓存
4. Authorization 头 → 不缓存 (除非 cache_authorized)
5. 匹配缓存规则获取 TTL
6. TTL <= 0 → 不缓存

### 规则匹配优先级
1. **path** — 最长前缀匹配 (最高)
2. **extension** — 文件扩展名匹配 (大小写不敏感)
3. **mimetype** — Content-Type 匹配 (支持 image/* 通配符) [响应阶段]
4. **regex** — 正则匹配 (编译后缓存)
5. **default_ttl** — 默认 3600s

### 响应可缓存性 (body_filter 阶段)
- 可缓存状态码: 200, 203, 204, 206, 300, 301, 302, 304, 307, 308, 404, 410
- Cache-Control: private/no-cache/no-store → 不缓存
- Set-Cookie → 不缓存 (除非 cache_cookies)
- Vary: * → 不缓存
- Content-Length > max_size (100MB) → 不缓存

### TTL 调整优先级
s-maxage > max-age > Expires > 规则配置 TTL (取较小值)

## 缓存 Key 生成

```
MD5(site_id + ":" + host + ":" + uri + ":" + sorted_args + ":" + vary_values)
```

- sort_query_string: 参数排序提高命中率
- vary_headers: 指定请求头纳入 key (如 Accept-Language)

## 存储操作

### 读取 (content 阶段, request_filter)
1. Redis GET 元数据 → miss 返回 None
2. 检查 expires_at → 过期返回 None
3. OSS GET 数据体 → 失败清理 Redis
4. 返回 {status, headers, body, cached_at}

### 写入 (body_filter 阶段, response_body_filter)
1. 逐块收集响应体 (检查 max_size)
2. EOF 时异步写入: tokio::spawn
3. OSS PUT 数据体 (两位前缀目录)
4. Redis SETEX 元数据

### 缓存命中响应
- 直接写 status + headers + body
- 添加 X-Cache-Status: HIT
- 添加 Age: now - cached_at
- request_filter 返回 Ok(true) 短路

## OSS 客户端

AWS Signature V4 签名, 使用 `aws-sigv4` 或 `ring` crate 实现。
支持: PUT/GET/HEAD/DELETE/LIST/批量删除
URL 风格: 虚拟主机 (默认) 或路径风格
自定义元数据: x-amz-meta-* 头

## 排除的响应头 (不缓存)

set-cookie, x-cache-status, age, connection, keep-alive, transfer-encoding

## 新增依赖

- `aws-sigv4` 或手动实现 — AWS V4 签名
- `md5` crate — 缓存 key 哈希
- `reqwest` — OSS HTTP 客户端 (或 Pingora 内置 connector)
