---
name: Protocol Proxying Guide for Nozdormu
description: How Pingora handles HTTP, HTTPS, WebSocket, WSS, SSE, gRPC proxying — configuration, limitations, and timeout considerations
type: reference
---

## 协议支持总览

| 协议 | Pingora 支持 | 配置要求 | 注意事项 |
|------|-------------|---------|---------|
| HTTP | 原生 | `add_tcp()` | 默认即可 |
| HTTPS | 原生 | `add_tls()` + 证书 | ALPN 自动协商 H1/H2 |
| WebSocket | 原生 | 无需额外配置 | 仅 HTTP/1.1 Upgrade 路径 |
| WSS | 原生 | TLS 终止 + WS Upgrade | TLS 终止后等同 WS |
| SSE | 原生 | 无需额外配置 | 流式转发不缓冲 |
| gRPC | 原生 | 强制 H2 + h2c | 需配置 ALPN::H2 |

## HTTP/HTTPS

- Pingora 自动桥接 H1 ↔ H2（下游和上游协议可不同）
- 上游协议由 `HttpPeer` ALPN 决定: `set_http_version(2, 1)` = H2H1
- H2C (明文 HTTP/2): `HttpServerOptions { h2c: true }`
- H2 下游: `tls_settings.enable_h2()` 或 `set_alpn(ALPN::H2H1)`
- 自动 H2 降级: 上游不支持 H2 时自动回退 H1

## WebSocket (WS/WSS)

- **零代码改动** — 内置完整 HTTP Upgrade 机制
- 流程: 检测 `Upgrade: websocket` → 转发 → 101 → 双向透传 (UpgradedBody)
- 升级后: 禁用连接复用、禁用压缩
- **限制**: 仅 HTTP/1.1 路径（H2 下游会剥离 Upgrade 头）
- **超时**: `read_timeout` 必须为 `None`，否则空闲断开
- WSS = TLS 终止 + WS Upgrade，可连接明文 WS 上游

## SSE (Server-Sent Events)

- 标准 HTTP 流式响应，逐块转发 (64KB chunks)
- `response_body_filter` 每 chunk 调用一次
- `read_timeout` 必须为 `None`
- macOS 已知刷新问题 (GitHub #841)，Linux 正常

## gRPC

### 下游配置
- TLS + ALPN H2: `tls_settings.enable_h2()`
- 明文 h2c: `HttpServerOptions { h2c: true }`

### 上游配置
```rust
peer.options.set_http_version(2, 2); // 强制 H2
peer.options.max_h2_streams = 10;    // 连接复用度
```

### Trailers
- 完整支持 `grpc-status`、`grpc-message` 转发
- H2→H2: trailers 直接转发
- H1 下游: trailers 是 TODO (用 gRPC-Web 模块解决)

### 四种流模式
- Unary / Server streaming / Client streaming / Bidirectional — 全部支持
- 双向流通过 `tokio::try_join!` + `tokio::select!` 实现全双工

### 负载均衡
- 每个 H2 stream 独立调用 `upstream_peer()` → per-request LB
- `max_h2_streams` 控制上游连接复用 (默认 1 = 每请求新连接)

### gRPC-Web
- 内置 `GrpcWeb` 模块: `application/grpc-web` ↔ `application/grpc` 自动转换
- 通过 `init_downstream_modules()` 注册

## Nozdormu 监听层规划

```
TCP  :6188  → HTTP, WS, SSE, h2c/gRPC 明文
TLS  :6443  → HTTPS, WSS, gRPC over TLS (ALPN: H2H1)

upstream_peer 逻辑:
  Content-Type: application/grpc* → ALPN::H2, max_h2_streams=10
  其他 → ALPN::H2H1 (自动协商)

超时策略:
  WS/SSE/gRPC streaming → read_timeout: None
  普通 HTTP → 可设置合理超时

HttpServerOptions { h2c: true }  // 支持明文 gRPC
```

## 关键源码位置 (cloudflare/pingora)

- Upgrade 检测: `pingora-core/src/protocols/http/v1/common.rs`
- H2 代理: `pingora-proxy/src/proxy_h2.rs`
- H1 代理: `pingora-proxy/src/proxy_h1.rs`
- gRPC-Web: `pingora-core/src/modules/http/grpc_web.rs`
- ALPN: `pingora-core/src/protocols/tls/mod.rs`
- Peer 配置: `pingora-core/src/upstreams/peer.rs`
- 连接池: `pingora-core/src/connectors/http/v2.rs`
