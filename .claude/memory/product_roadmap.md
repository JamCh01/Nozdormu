---
name: Product Roadmap - Commercial CDN
description: Gap analysis vs commercial CDNs, prioritized feature roadmap for post-migration expansion. Strategy confirmed: migrate first, expand later.
type: project
---

## 产品定位

面向外部客户的商业 CDN 产品。

## 实现策略 (已确认)

**先迁移后扩展**: 先完成原系统 15 章功能的 1:1 迁移，再逐步添加商业化功能。

## 第一优先级 (核心缺失, 迁移完成后立即补充)

1. **Cache Purge API** — 单URL/前缀/Cache Tag/全站清除
2. **边缘压缩** — Brotli/Gzip/Zstd, Pingora 内置支持
3. **本地缓存层** — moka 进程内热点缓存 (L0), 解决 Redis+OSS 延迟问题
4. **OWASP 基础 WAF** — SQLi/XSS/路径遍历/请求体限制
5. **Web 管理面板 + REST API** — 完整认证, 多租户
6. **多租户与权限** — 租户隔离, API Key/OAuth, 角色权限

## 第二优先级 (竞争力差异化)

7. 实时分析面板 — 流量/缓存/攻击可视化
8. 配置版本控制 + 回滚 — etcd revision 历史
9. 边缘规则引擎 — URL重写/条件路由/访问控制 (配置化, 非代码)
10. 图片优化 — WebP/AVIF 自动转换/缩放
11. 告警系统 — 源站宕机/证书过期/异常流量, 邮件/Webhook
12. 计费与用量统计 — 流量/请求/带宽计费, 用量报表

## 第三优先级 (成熟期)

- Bot 管理 (JS指纹/行为分析)
- API 安全 (Schema验证/异常检测)
- 智能路由 (延迟最优路径)
- 视频优化 (HLS/DASH 分片缓存)
- HTTP/3 QUIC (等 Pingora 支持)
- 分布式追踪 (OpenTelemetry)
- Terraform Provider

## 当前 vs 商业 CDN 已具备的能力

- 动态路由 + 通配符域名 ✓
- 多协议代理 (HTTP/WS/SSE/gRPC) ✓
- 负载均衡 + 健康检查 ✓
- WAF 地理位置过滤 ✓
- CC 防护 + JS 挑战 ✓
- ACME 多提供商自动证书 ✓
- Redis Streams 日志 + Prometheus ✓
- 多节点部署 + 分布式锁 ✓
