# NetPulse Agent — Cross-Platform Release Build Plan

## Context

需要为 NetPulse Agent 构建 6 个平台的 release 二进制文件。当前环境是 Windows x86_64，有 Docker（可用 `cross-rs/cross`），Rust 1.94.1。

## Target Matrix

| Platform | Target Triple | 工具 | musl |
|----------|--------------|------|------|
| Linux x86_64 | `x86_64-unknown-linux-musl` | cross | Yes |
| Linux ARM64 | `aarch64-unknown-linux-musl` | cross | Yes |
| macOS x86_64 | `x86_64-apple-darwin` | cross | N/A (macOS 无 musl) |
| macOS ARM64 | `aarch64-apple-darwin` | cross | N/A |
| Windows x86_64 | `x86_64-pc-windows-msvc` | cargo (本机) | N/A |
| Windows ARM64 | `aarch64-pc-windows-msvc` | cargo + target | N/A |

## Release 优化 (Cargo.toml [profile.release])

```toml
[profile.release]
opt-level = 3          # 最大优化
lto = "fat"            # 全程序链接时优化，减小体积
codegen-units = 1      # 单编译单元，更好优化
panic = "abort"        # 不需要 unwind，减小体积
strip = true           # 去除调试符号
```

## 实施步骤

### Step 1: Cargo.toml 添加 release profile
- 文件: `Cargo.toml`

### Step 2: 安装 cross-rs
```
cargo install cross
```

### Step 3: 创建 build 脚本
- 文件: `scripts/build-release.sh`
- 本机 cargo build: Windows x86_64, Windows ARM64
- cross build: Linux musl x86_64/ARM64, macOS x86_64/ARM64
- 输出到 `dist/` 目录，命名: `netpulse-agent-{target}{.exe}`

### Step 4: 逐个构建并验证
1. `x86_64-pc-windows-msvc` — cargo build (本机)
2. `aarch64-pc-windows-msvc` — cargo build + rustup target
3. `x86_64-unknown-linux-musl` — cross build
4. `aarch64-unknown-linux-musl` — cross build
5. `x86_64-apple-darwin` — cross build
6. `aarch64-apple-darwin` — cross build

### Step 5: 验证产物
- 检查文件大小
- `file` 命令确认架构
- Windows 本机运行测试

## 关键文件
- `Cargo.toml` — 添加 [profile.release]
- `scripts/build-release.sh` — 构建脚本

## 验证
- 6 个二进制文件在 `dist/` 目录
- Linux 二进制为静态链接 (musl)
- 文件大小合理 (预期 8-15MB)
