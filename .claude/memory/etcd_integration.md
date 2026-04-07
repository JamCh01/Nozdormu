---
name: etcd Integration for Nozdormu CDN
description: etcd-client crate usage, key hierarchy design, watch mechanism, service discovery, and deployment recommendations for CDN config
type: reference
---

## Rust etcd 客户端

**推荐**: `etcd-client` v0.14（基于 tonic gRPC，Tokio 原生）
- Watch 支持（prefix, range, revision）
- Lease/TTL（Grant, Revoke, KeepAlive stream）
- 事务（If/Then/Else）
- 认证（用户名/密码 + TLS）

```toml
etcd-client = "0.14"
```

## Key 层级设计

```
/nozdormu/
├── config/global                       # 全局设置
├── upstreams/{name}/config             # 后端定义 (LB算法、健康检查)
├── upstreams/{name}/targets/{id}       # 后端节点 (地址、TLS、SNI、权重)
├── routes/{id}                         # 路由规则 (域名、路径、上游、优先级、插件)
├── certs/{domain}                      # TLS 证书 (PEM)
├── plugins/rate_limit/{rule_id}        # 限流规则
├── plugins/waf/{rule_id}              # WAF 规则
├── nodes/{node_id}/status              # 节点注册 (lease TTL 自动过期)
└── leader                              # Leader 选举 (CAS 事务)
```

## 架构模式

启动时全量加载 → watch stream 增量更新 → `ArcSwap<LiveConfig>` 热路径零锁读取

### 可靠 Watch 流程
1. `GET` with prefix 获取全量 + 记录 revision
2. `WATCH` from revision+1 接收增量
3. 断线重连时从 last_revision 恢复，不丢事件
4. 遇到 Compacted 错误时回退到全量重载

### 节点注册
- Lease Grant (TTL=15s) → PUT with lease → KeepAlive loop (每5s)
- 节点宕机 → lease 过期 → key 自动删除 → 其他节点通过 watch 感知

## 部署建议

| CDN 规模 | etcd 集群 | 说明 |
|----------|----------|------|
| 小型 (1-10 节点) | 3 节点 | 标准 HA |
| 中型 (10-100) | 3 或 5 节点 | 5 节点提升读吞吐 |
| 大型 (100+) | 5 节点 + learner | learner 扩展读 |

- CDN 热路径永远不访问 etcd，只读内存中的 ArcSwap
- `--auto-compaction-retention=1h` 保留1小时历史
- `--quota-backend-bytes=67108864` (64MB 足够)
