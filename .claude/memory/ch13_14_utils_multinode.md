---
name: Chapters 13-14 - Utils and Multi-Node Design
description: Redis connection (ConnectionManager), IP utils (ipnet + XFF), distributed locks, multi-node deployment with label selectors
type: reference
---

## Ch13: 工具库

### Redis 连接 → `redis` crate ConnectionManager
- Sentinel: `redis::sentinel::SentinelClient` 自动发现 Master
- Standalone: `redis::Client::open()`
- ConnectionManager 自动重连/复用/Clone 共享
- 不需要手动 set_keepalive / release_connection
- 便捷方法由 `redis::AsyncCommands` trait 提供

### IP 工具 → `ipnet` + 自定义 XFF
- CIDR 匹配: `ipnet::IpNet::contains()`, 支持 IPv4/IPv6
- XFF 防伪造: 从右向左遍历, 找第一个非可信代理 IP
- 可信代理: 127.0.0.0/8, 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
- 其他: is_valid_ip, is_private_ip, ip_in_cidrs

### 分布式锁 → Redis SETNX + Lua 脚本
- 获取: `SET key owner NX EX ttl`
- 释放: Lua 脚本原子检查 owner 后 DEL
- 续期: Lua 脚本原子检查 owner 后 EXPIRE
- 带重试: acquire_with_retry(max_retries, retry_delay)
- Owner: `{node_id}:{pid}` (单进程)
- Key 前缀: `nozdormu:lock:`

## Ch14: 多节点部署

### 架构变化 (已确认)
- 原系统: group_id:node_id 双级标识 + Redis HASH 隔离
- Nozdormu: node_id + labels 标签 + etcd 全局存储 + 标签选择器

### 节点标识
```rust
pub struct NodeIdentity {
    pub node_id: String,
    pub labels: HashSet<String>,  // ["region:asia", "dc:tokyo", "tier:edge"]
}
```

### 配置分发
- etcd 全局存储, 站点配置含 target_labels
- 节点按标签过滤, 只加载匹配站点
- etcd watch 天然广播, 不需要 Pub/Sub 双频道

### 分布式锁场景
| 场景 | Key | TTL |
|------|-----|-----|
| 证书申请 | nozdormu:lock:acme:obtain:{domain} | 300s |
| 续期扫描 | nozdormu:lock:renewal:scan | 300s |
| 单域名续期 | nozdormu:lock:renewal:{domain} | 600s |

### 健康状态
- 每个节点独立判断 (网络路径不同, 合理设计)
- 本地: Arc<DashMap> (主要)
- Redis: nozdormu:health:{site_id}:{origin_id} TTL 60s (辅助)

### ACME 账户共享
- etcd: /nozdormu/acme/accounts/{provider}
- 所有节点复用同一账户
