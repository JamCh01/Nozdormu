# Self-Update Feature

## Context

后端新增了 Agent 自更新能力：通过 NATS 推送更新通知，Agent 下载新二进制、校验 SHA256、替换自身、退出后由服务管理器（systemd/WinSW）自动拉起新版本。同时注册和心跳需要上报 platform 和 version。

## Changes Overview

| 变更 | 说明 |
|------|------|
| 注册上报 platform + version | `RegisterRequest` 增加 `platform`, `agent_version` 字段 |
| 心跳上报 version | `HeartbeatRequest` 增加可选 `agent_version` 字段 |
| 订阅更新通知 | 新增 `subscribe_updates()` 订阅 `netpulse.updates.{uuid}` |
| 自更新模块 | 新增 `src/updater.rs`：版本比较、下载、SHA256 校验、二进制替换 |
| 优雅退出重启 | 更新成功后 break 主循环 → 正常 shutdown → exit 0 → 服务管理器重启 |

## New Dependencies (`Cargo.toml`)

```toml
sha2 = "0.10"       # SHA256 校验
semver = "1"         # 语义版本比较
tempfile = "3"       # 安全临时文件
```

## New File: `src/updater.rs`

```
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

should_update(msg_version) -> Result<bool>     # semver 比较，仅 msg > current 时返回 true
download_binary(client, url, dest_dir) -> Result<NamedTempFile>  # 流式下载到临时文件
verify_checksum(path, expected_sha256) -> Result<()>             # SHA256 校验
replace_binary(temp_path, current_exe) -> Result<()>             # 平台特定替换
perform_update(client, msg) -> Result<bool>    # 编排：比较→下载→校验→替换，true=需要重启
```

**二进制替换策略：**
- Unix: `set_permissions(0o755)` → `fs::rename()` (同文件系统原子操作)
- Windows: 删除旧 `.old` → rename 当前为 `.old` → rename 新文件为原路径

**临时文件：** 使用 `tempfile::Builder::new().tempfile_in(binary_dir)` 确保同文件系统，避免跨文件系统 rename 失败。

## New Function: `src/nats/subscriber.rs`

```rust
subscribe_updates(jetstream, agent_uuid) -> Result<Stream>
```
- Stream: `NETPULSE_UPDATES`
- Consumer: `agent-updates-{uuid}`
- Subject: `netpulse.updates.{uuid}`
- DeliverPolicy::New（同 subscribe_tasks 模式）

## Modified Files

| File | Change |
|------|--------|
| `Cargo.toml` | +sha2, +semver, +tempfile |
| `src/main.rs` | +`mod updater;` |
| `src/error.rs` | +`UpdateFailed`, `UpdateChecksumMismatch`, `UpdateDownloadFailed` |
| `src/types.rs` | `RegisterRequest` +platform/agent_version; `HeartbeatRequest` +agent_version(Option); +`UpdateMessage` struct |
| `src/config.rs` | +`platform: Option<String>` 字段 + `platform_string()` 方法（auto-detect `{ARCH}-{OS}`） |
| `src/nats/subscriber.rs` | +`subscribe_updates()` |
| `src/api/heartbeat.rs` | `build_heartbeat()` 签名增加 `agent_version` 参数 |
| `src/agent.rs` | 注册传 platform/version; 订阅 updates（非致命）; select! 增加第 4 分支处理更新; 心跳传 version |
| `CLAUDE.md` | 更新文档 |

## Agent.rs Integration

### Registration (Agent::new)
```rust
RegisterRequest {
    agent_uuid, access_key,
    platform: config.platform_string(),
    agent_version: updater::CURRENT_VERSION.to_string(),
}
```

### Update Subscription (Agent::run, 非致命)
```rust
let mut update_messages: Option<Stream> = match retry(subscribe_updates, 3 attempts) {
    Ok(stream) => Some(stream),
    Err(e) => { warn!("updates disabled"); None }
};
```

### Select! 第 4 分支
```
update msg received:
  ├─ parse UpdateMessage
  ├─ ack message
  ├─ perform_update(http_client, msg)
  │   ├─ Ok(true)  → info!("shutting down for restart") → break
  │   ├─ Ok(false) → info!("no update needed")
  │   └─ Err(e)    → error!("update failed, continuing")
  └─ stream None → update_stream_active = false
```

### Heartbeat
```rust
build_heartbeat(task_count, Some(updater::CURRENT_VERSION.to_string()))
```

## New Types (`src/types.rs`)

```rust
// RegisterRequest 增加字段
pub platform: String,
pub agent_version: String,

// HeartbeatRequest 增加字段
#[serde(skip_serializing_if = "Option::is_none")]
pub agent_version: Option<String>,

// 新增
pub struct UpdateMessage {
    pub action: String,
    pub version: String,
    pub sha256: String,
    pub download_url: String,
    #[serde(default)]
    pub release_notes: Option<String>,
}
```

## Update Flow

```
NATS msg → parse UpdateMessage → ack
  → should_update(version)?
    → false: skip
    → true:
      → download_binary(url) → temp file (same dir as exe)
      → verify_checksum(temp, sha256)
      → replace_binary(temp, current_exe)  [platform-specific]
      → break select! loop
      → graceful shutdown (cancel → scheduler → flusher drain)
      → process exit 0
      → systemd/WinSW auto-restart with new binary
```

## Verification

```bash
cargo build && cargo clippy && cargo test
```
