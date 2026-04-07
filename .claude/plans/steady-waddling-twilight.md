# Plan: CDN Lua 补充单元测试

## Context

上一轮代码审查修复了 19 个问题（SSL manager 未执行、IP 欺骗、WebSocket/SSE SSL 缺失等）。现在需要编写补充测试来：
1. 验证修复的 bug 不会回归
2. 覆盖现有 `test_all.sh` 未测试的功能模块

## 现有测试覆盖分析

`scripts/test_all.sh` 已覆盖 10 大模块约 70+ 测试用例，但缺少以下关键场景。

## 新增测试内容（追加到 test_all.sh 风格）

创建 `scripts/test_fixes.sh`，专门测试修复项 + 补充缺失覆盖：

### Section 11: Bug Fix 回归测试
- **11.1 SSL manager 执行验证** — HTTPS 请求返回 200（证明 ssl_certificate_by_lua_file 正确执行）
- **11.2 IP 欺骗防护** — 伪造 XFF 头不影响 WAF 判断（直连时 remote_addr 不在可信代理列表中，XFF 被忽略）
- **11.3 WebSocket SSL 回源** — 配置 HTTPS 源站的 WebSocket 站点，验证 `@websocket_ssl` location 可达
- **11.4 SSE SSL 回源** — 配置 HTTPS 源站的 SSE 站点，验证 `@sse_ssl` location 可达
- **11.5 gRPC SSL 路由** — gRPC 请求根据源站协议选择 `@grpc` 或 `@grpc_insecure`
- **11.6 Redis Stream MAXLEN** — 发送请求后检查 Redis Stream 长度不超过限制

### Section 12: 管理端点
- **12.1 /health** — 返回 200 "OK"
- **12.2 /health/detail** — 从 127.0.0.1 访问返回 JSON（含 Redis 状态）
- **12.3 /metrics** — 从 127.0.0.1 访问返回 Prometheus 格式
- **12.4 /status** — 从 127.0.0.1 访问返回 JSON 状态
- **12.5 /upstream/health** — 返回健康状态 JSON
- **12.6 外部 IP 访问受限端点** — 从外部 IP 访问 /metrics 返回 403

### Section 13: 通配符域名
- **13.1 *.example.com 匹配** — 配置通配符域名，子域名请求正确路由
- **13.2 精确域名优先于通配符** — 同时配置精确和通配符，精确优先

### Section 14: 健康检查与故障转移
- **14.1 被动健康检查** — 源站返回 502 后标记为不健康
- **14.2 全部源站不可用返回 502** — 所有源站都不可达时返回 502
- **14.3 健康状态管理接口** — /upstream/health 显示健康状态

### Section 15: gRPC 路由检测
- **15.1 gRPC Content-Type 检测** — application/grpc 请求被路由到 @grpc location
- **15.2 非 gRPC 站点拒绝 gRPC** — gRPC 未启用的站点返回错误

### Section 16: 日志与指标
- **16.1 请求日志推送到 Redis Stream** — 请求后 Redis Stream 有新记录
- **16.2 X-Request-ID 自动生成** — 每个请求都有唯一的 X-Request-ID
- **16.3 X-Cache-Status 头** — 缓存启用时返回正确的缓存状态

### Section 17: Pub/Sub 配置更新
- **17.1 通过 Pub/Sub 更新站点** — PUBLISH 消息后站点配置生效
- **17.2 通过 Pub/Sub 删除站点** — PUBLISH delete 消息后站点不可用
- **17.3 广播频道** — 通过 group broadcast 频道更新

### Section 18: 边界情况
- **18.1 超大 Host 头** — 超长域名返回 404 而非崩溃
- **18.2 特殊字符 URI** — URL 编码字符正常处理
- **18.3 并发请求** — 10 个并发请求全部成功

## 实现方案

### 文件: `scripts/test_fixes.sh`
- 与 `test_all.sh` 相同的风格和工具函数
- 复用 `pass()`/`fail()`/`skip()`/`section()`/`write_site()`/`http_code()`/`http_body()`/`http_header()` 等函数
- 测试站点 ID 使用 980-999 范围，避免与 test_all.sh 的 900-972 冲突
- 测试结束后清理所有测试数据

### 执行方式
```bash
# 在远程服务器上执行
ssh root@103.141.183.132 'bash -s' < scripts/test_fixes.sh
```

### 关键文件
- `scripts/test_fixes.sh` — 新建，补充测试脚本
- 远程服务器: 103.141.183.132（已部署运行中的 CDN）

## 验证
1. 将脚本传输到服务器并执行
2. 所有测试用例应通过（PASS）
3. 测试结束后自动清理测试数据
