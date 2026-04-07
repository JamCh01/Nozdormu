#!/bin/bash
set -euo pipefail

echo "=========================================="
echo "  Nozdormu CDN - WSL Development Setup"
echo "  Debian 13 (trixie)"
echo "=========================================="

# ============================================================
# 1. System dependencies
# ============================================================
echo "[1/6] Installing system dependencies..."
sudo apt-get update
sudo apt-get install -y \
    build-essential \
    pkg-config \
    libssl-dev \
    perl \
    cmake \
    protobuf-compiler \
    git \
    curl \
    unzip

# ============================================================
# 2. Rust toolchain
# ============================================================
echo "[2/6] Installing Rust toolchain..."
if command -v rustup &> /dev/null; then
    echo "  Rust already installed, updating..."
    rustup update stable
else
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
    source "$HOME/.cargo/env"
fi

rustup component add rustfmt clippy
echo "  Rust version: $(rustc --version)"

# ============================================================
# 3. Copy project to WSL native filesystem
# ============================================================
echo "[3/6] Setting up project..."
PROJECT_DIR="$HOME/Nozdormu"
WINDOWS_PATH="/mnt/c/Users/user/Desktop/Nozdormu"

if [ -d "$PROJECT_DIR/Cargo.toml" ] || [ -f "$PROJECT_DIR/Cargo.toml" ]; then
    echo "  $PROJECT_DIR already has source, syncing..."
    rsync -a --exclude='target/' --exclude='.git/' "$WINDOWS_PATH/" "$PROJECT_DIR/"
else
    echo "  Copying from Windows to WSL native filesystem..."
    rm -rf "$PROJECT_DIR"
    mkdir -p "$PROJECT_DIR"
    # Copy everything except target/ and .git/ for clean start
    rsync -a --exclude='target/' --exclude='.git/' "$WINDOWS_PATH/" "$PROJECT_DIR/"
    cd "$PROJECT_DIR"
    git init
    git add -A
    git commit -m "Initial import from Windows"
fi

cd "$PROJECT_DIR"

# ============================================================
# 4. Docker (via Docker Desktop WSL integration)
# ============================================================
echo "[4/6] Checking Docker..."
if command -v docker &> /dev/null; then
    echo "  Docker available: $(docker --version)"
    echo "  Docker Compose: $(docker compose version 2>/dev/null || echo 'not found')"
else
    echo ""
    echo "  !! Docker not found in WSL."
    echo "  Please enable Docker Desktop WSL integration:"
    echo "    1. Open Docker Desktop"
    echo "    2. Settings → Resources → WSL Integration"
    echo "    3. Enable for your Debian distro"
    echo "    4. Apply & Restart"
    echo "    5. Re-run this script"
    echo ""
fi

# ============================================================
# 5. Verify build
# ============================================================
echo "[5/6] Verifying build..."
cd "$PROJECT_DIR"
cargo check 2>&1 | tail -5
echo "  Build check complete."

# ============================================================
# 6. Summary
# ============================================================
echo ""
echo "=========================================="
echo "  Setup Complete!"
echo "=========================================="
echo ""
echo "  Project path:  $PROJECT_DIR"
echo "  Rust:          $(rustc --version)"
echo "  Cargo:         $(cargo --version)"
echo "  protoc:        $(protoc --version)"
echo ""
echo "  Next steps:"
echo "    cd $PROJECT_DIR"
echo "    docker compose -f docker/docker-compose.yml --profile infra up -d"
echo "    cargo run -p cdn-proxy -- -c config/default.yaml"
echo ""
