---
name: Chapter 10 - SSL/TLS Certificate Management Design
description: Dynamic cert selection via TlsAcceptCallbacks, ACME v2 multi-provider client, HTTP-01 challenge, cert storage in etcd, auto-renewal
type: reference
---

## 动态证书选择 → Pingora TlsAcceptCallbacks

实现 `TlsAccept` trait 的 `certificate_callback`:
1. 从 SNI 获取域名 (小写)
2. 查找优先级: moka 缓存(TTL 300s) → etcd → 通配符 → 自定义证书 → 默认证书
3. `ssl.clear_certs()` → 逐个 `set_der_cert()` (叶子+中间CA) → `set_der_priv_key()`

通配符匹配: RFC 6125, 仅单级子域 (`*.example.com` 匹配 `foo.example.com`)

## ACME v2 客户端

**4 个提供商**: Let's Encrypt, ZeroSSL (EAB), Buypass, Google (EAB)
**Rust crate**: `instant-acme` (或自定义实现)

流程: 分布式锁 → 获取目录 → 创建/加载账户 → 创建订单 → HTTP-01 挑战 → CSR → 签发 → 下载

**EAB**: ZeroSSL/Google 需要 HMAC-SHA256 签名的 JWS 绑定外部账户
**多提供商轮询**: 逐个尝试, 续期时优先原始提供商
**账户共享**: etcd `/nozdormu/acme/accounts/{provider}`, 所有节点复用

## HTTP-01 挑战

在 `request_filter()` 中拦截 `/.well-known/acme-challenge/{token}`:
- 从 etcd `/nozdormu/acme/challenges/{domain}/{token}` 获取响应
- 返回 200 text/plain
- 挑战响应通过 etcd lease 设置 TTL

## 证书存储 → etcd + 文件备份

```
etcd:
  /nozdormu/certs/{domain}                  → {cert_pem, key_pem, expires_at, provider, domains}
  /nozdormu/certs/custom:{site_id}:{domain} → {cert_pem, key_pem, expires_at}
  /nozdormu/acme/accounts/{provider}         → {public, private, account_url}
  /nozdormu/acme/challenges/{domain}/{token} → key_authorization (lease TTL)

文件备份:
  certs/acme/{domain}/fullchain.pem + privkey.pem
  certs/custom/{site_id}/fullchain.pem + privkey.pem
  certs/default/fullchain.pem + privkey.pem
```

证书过期解析: `x509-parser` crate (替代 openssl CLI)

## 自动续期 → BackgroundService

- 启动 60s 后首次检查, 之后每天一次
- 两级分布式锁: 扫描锁(300s) + 域名锁(600s)
- 二次检查防止重复续期
- 续期间隔 5s (避免 ACME 速率限制)
- 默认续期: 到期前 30 天

## 新增依赖

- `instant-acme = "0.7"` — ACME v2 客户端
- `x509-parser = "0.16"` — 证书解析
