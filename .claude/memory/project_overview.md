---
name: Nozdormu CDN Project Overview
description: Enterprise CDN system built on Cloudflare Pingora 0.8, Cargo workspace with 5 crates, OpenSSL TLS, Windows + WSL/Docker dev
type: project
---

Enterprise CDN system "nozdormu-cdn" built on Cloudflare Pingora v0.8.0.

**Why:** User is building a production CDN system; feature list to be provided later.

**How to apply:**
- Development on Windows with WSL2 + Docker (编译和运行在 Linux 容器内，源码在 Windows)
- Internal project, will NOT be published to crates.io (publish = false)
- TLS backend: OpenSSL
- Rust MSRV: 1.84
- Cargo workspace with 5 crates: cdn-common, cdn-config, cdn-cache, cdn-middleware, cdn-proxy
- Config via YAML at config/default.yaml (Pingora ServerConf + CDN-specific `cdn:` section)
- Docker multi-stage build in docker/
- Prometheus metrics on port 6190, proxy on port 6188
- Caching is experimental in Pingora — cdn-cache is a stub awaiting feature requests
