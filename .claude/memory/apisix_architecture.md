---
name: APISIX Architecture Reference
description: Apache APISIX plugin system, route matching, upstream management, etcd integration, SSL/TLS, Admin API — migration reference for Nozdormu
type: reference
---

## 插件系统

### 阶段与优先级
- 6 个阶段: `rewrite` → `access` → `before_proxy` → `header_filter` → `body_filter` → `log`
- 数字优先级 (越高越先执行): real-ip=22000, ip-restriction=3000, jwt-auth=2510, limit-req=1001, prometheus=500
- 可通过 `_meta.priority` 按路由覆盖默认优先级
- 合并优先级: Consumer > Consumer Group > Route > Plugin Config > Service

### 常用插件分类

**认证**: key-auth, jwt-auth, basic-auth, hmac-auth, openid-connect, forward-auth
**安全**: ip-restriction, ua-restriction, cors, csrf, uri-blocker
**流量控制**: limit-req (漏桶), limit-count (固定/滑动窗口), limit-conn (并发), traffic-split, api-breaker
**可观测**: prometheus, zipkin, skywalking, datadog, http/tcp/kafka-logger
**转换**: proxy-rewrite, response-rewrite, body-transformer, grpc-transcode, grpc-web
**代理**: proxy-mirror, proxy-cache, redirect

### 插件配置验证
- 每个插件定义 JSON Schema
- `check_schema(conf)` 在加载和更新时验证
- Rust 等价: `serde` + `#[derive(Deserialize)]` + 自定义验证

## 路由匹配

- 默认路由器: `radixtree_host_uri` (host + URI 两级匹配)
- 匹配参数: `uri/uris`, `host/hosts`, `methods`, `remote_addr`, `vars` (表达式条件)
- 支持通配符: `/*`, `*.foo.com`
- 路由按 `priority` 排序 (默认 0)
- 路由变更时重建 radixtree
- **Rust 等价**: `matchit` crate (基数树) + `HashMap<host, Router>` 两级结构

## 上游管理

### 负载均衡
- `roundrobin` (加权轮询, 默认)
- `chash` (一致性哈希, 支持 var/header/cookie/consumer 作为 key)
- `ewma` (指数加权移动平均, 选最低延迟节点)
- `least_conn` (最少连接)
- 节点优先级: 高优先级组优先, 全部不健康时降级

### 健康检查
- **主动**: HTTP/HTTPS/TCP 探针, 可配间隔/阈值/状态码
- **被动**: 基于实际流量, 失败计数达阈值标记不健康
- 全部不健康时回退使用所有节点

### 服务发现
- 可插拔接口: `nodes(service_name)` 返回节点列表
- 内置: DNS, Consul, Nacos, Eureka, Kubernetes

### 超时与重试
- `retries` + `retry_timeout`
- `timeout: {connect, send, read}` (默认各 60s)
- Keepalive: 320 连接, 1000 请求/连接, 60s 超时

## etcd 集成

### Key 前缀结构
```
/apisix/
├── routes/{id}
├── upstreams/{id}
├── services/{id}
├── consumers/{username}
├── consumer_groups/{id}
├── ssls/{id}
├── plugins/{name}
├── plugin_configs/{id}
├── global_rules/{id}
├── stream_routes/{id}
├── protos/{id}
└── secrets/{manager}/{id}
```

### 部署模式
1. **Traditional**: 单实例, 数据面+控制面, 读写 etcd
2. **Decoupled**: 分离数据面 (读 etcd) 和控制面 (Admin API 写 etcd)
3. **Standalone**: 无 etcd, 本地 YAML 文件, 适合 K8s

## SSL/TLS

- 证书作为一等对象存储在 etcd `/apisix/ssls/{id}`
- SNI 路由: `radixtree_sni`, 支持精确域名和通配符 `*.test.com`
- 多证书: 同域名 ECC + RSA 双栈
- mTLS: Admin API / etcd 通信 / 上游连接

## Admin API

- REST API, 端口 9180, `X-API-KEY` 认证
- 资源: routes, services, upstreams, consumers, ssls, global_rules, plugin_configs
- 支持: PUT/POST/PATCH/DELETE, 分页, 过滤, TTL, 强制删除
- 本质: etcd 的 REST 薄封装 + JSON Schema 验证
- Control API (端口 9090): 内部状态如健康检查状态

## Nozdormu 需要构建的能力 (对标 APISIX)

1. **插件系统**: trait-based, 阶段钩子 + 优先级排序 → `cdn-middleware`
2. **路由匹配**: host+URI+method 基数树 → `matchit` crate
3. **动态配置**: etcd watch + Admin API → `cdn-config` 扩展
4. **更多 LB 算法**: EWMA, least_conn
5. **HTTP 健康检查**: 主动 HTTP 探针 + 被动健康检查
6. **SSL 管理**: 动态证书 + SNI 选择
7. **服务发现**: DNS, Consul 等可插拔接口
