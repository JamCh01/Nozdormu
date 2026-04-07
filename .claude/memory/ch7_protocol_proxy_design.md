---
name: Chapter 7 - Multi-Protocol Proxy Design
description: Protocol detection, HTTP/WS/SSE/gRPC proxy implementation in Pingora — key simplifications vs OpenResty, L4 stream proxy
type: reference
---

## 核心架构差异

原系统: `ngx.exec(@location)` 跳转到不同 Nginx location
Pingora: 同一个 `ProxyHttp` 实现, `upstream_peer()` 根据协议类型配置不同 HttpPeer

**6 个 Nginx location → 1 个 upstream_peer()**

## 协议检测 (request_filter 中)

优先级: gRPC > WebSocket > SSE > HTTP

```rust
enum ProtocolType {
    Http,
    WebSocket,
    Sse,
    Grpc(GrpcVariant),  // Native | Web | WebText
}
```

- gRPC: Content-Type 以 `application/grpc` 开头
- WebSocket: Upgrade=websocket AND Connection 含 upgrade
- SSE: Accept 含 text/event-stream
- 非 HTTP 协议自动 cache_status = Bypass

## HTTP/HTTPS 代理

**简化**: 不再需要 SSL 回源多数投票 — 每个 origin 独立声明 protocol (http/https)
`upstream_peer()` 直接使用选中 origin 的 TLS 配置。

## WebSocket — Pingora 原生支持

零额外代码处理 Upgrade 机制。需要在 request_filter 中:
- 验证 RFC 6455 (方法/版本/Upgrade/Connection/Key/Version)
- Origin CORS 验证 (allowed_origins 配置)
- 子协议和扩展头透传
- read_timeout = None (长连接)

## SSE — Pingora 流式转发

天然支持, 逐块转发不缓冲。需要:
- request_filter: Accept-Encoding=identity (禁用压缩), Cache-Control=no-cache
- upstream_request_filter: 透传 Last-Event-ID
- response_filter: X-Accel-Buffering=no, Cache-Control=no-cache
- read_timeout = None

## gRPC — Pingora H2

```rust
// upstream_peer 中:
peer.options.set_http_version(2, 2);  // 强制 H2
peer.options.max_h2_streams = 10;
```

- Native gRPC: 端到端 H2, trailers 自动转发
- gRPC-Web: Pingora 内置 GrpcWeb 模块 (init_downstream_modules)
- 服务白名单: request_filter 中解析 /Service/Method 路径
- CORS 预检: gRPC-Web 的 OPTIONS 请求处理

## Layer4 TCP Stream — 独立 Service

不走 ProxyHttp trait, 使用 Pingora 底层 Service trait:
- 监听 :8443
- TLS preread 提取 SNI
- SNI 路由: 精确 → 单级通配符 → 多级通配符
- 配置从 etcd `/nozdormu/stream_routes/` 加载
- 独立的 TCP 健康检查 (连接探测)
- 加权轮询上游选择

## 超时配置

| 协议 | connect | read | 说明 |
|------|---------|------|------|
| HTTP | 10s | 60s | 标准请求-响应 |
| WebSocket | 10s | None | 双向长连接 |
| SSE | 10s | None | 单向推送流 |
| gRPC | 10s | 300s | RPC 调用 |

## 关键简化总结

| 原系统 | Pingora |
|--------|---------|
| 6 个 Nginx location | 1 个 upstream_peer() |
| SSL 多数投票 | per-origin protocol |
| ngx.exec 导致 ctx 丢失 | CTX 贯穿全程 |
| DNS 预解析 (balancer 禁止 cosocket) | upstream_peer() 是 async |
| 独立 WS/SSE location | Pingora 原生支持 |
