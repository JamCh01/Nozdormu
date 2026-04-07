---
name: Pingora Dynamic Configuration Patterns
description: How Pingora supports dynamic config via ServiceDiscovery trait, BackgroundService, ArcSwap, and reference projects (PingSIX, Pingap)
type: reference
---

## Pingora 动态配置核心机制

### ServiceDiscovery trait
- 位置: `pingora-load-balancing/src/discovery.rs`
- 方法: `async fn discover(&self) -> Result<(BTreeSet<Backend>, HashMap<u64, bool>)>`
- 实现此 trait 可从 etcd/Redis/API 动态获取后端列表
- `LoadBalancer` 内置轮询循环，按 `update_frequency` 定期调用 `discover()`

### BackgroundService trait
- 位置: `pingora-core/src/services/background.rs`
- 用于后台轮询/监听外部系统
- 接收 `ShutdownWatch` 观察优雅关闭信号
- 通过 `.task()` 返回 `Arc<A>` 与 proxy 共享状态

### ArcSwap 模式
- `LoadBalancer` 内部使用 `ArcSwap<BTreeSet<Backend>>` 和 `ArcSwap<S>` (selector)
- 热路径 `select()` 调用 `self.selector.load()` — 无锁读取
- 后台服务通过 `ArcSwap::store()` 原子替换配置

### 两种实现路径
1. **自定义 ServiceDiscovery** — 最少代码，让 LoadBalancer 内置循环处理轮询
2. **自定义 BackgroundService + ArcSwap<Config>** — 适用于超出上游列表的配置（限流规则、路由、中间件配置）
3. **etcd watch 推送模式** — 实现 Service trait，用 watch stream 实时接收变更（PingSIX 方案）

### 参考项目
- **PingSIX** (64 stars): Pingora + etcd 完整实现，APISIX 兼容 API 网关
  - etcd watch + list 模式，DashMap 全局注册表
  - https://github.com/zhu327/pingsix
- **Pingap** (1191 stars): 支持 file 和 etcd 配置后端，零停机热重载
  - https://github.com/vicanso/pingap
- **River** (2333 stars): memorysafety.org 的 Pingora 反向代理
- **Proksi** (212 stars): 带 Docker 支持的 CDN/LB
