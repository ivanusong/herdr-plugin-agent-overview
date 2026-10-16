#!/bin/sh
# 构建脚本：macOS / Linux 通用，直接 cargo 原生编译。
# herdr 从 GitHub 安装（plugin install）时执行；本地开发请自行运行本脚本后 `herdr plugin link .`。
set -e
cd "$(dirname "$0")/.."

# rustup 安装后 cargo 不一定在 PATH 里，尽量补齐常见位置。
if ! command -v cargo >/dev/null 2>&1; then
  for dir in "$HOME/.cargo/bin" "$HOME/.rustup/toolchains"/*/bin /opt/homebrew/bin /usr/local/bin; do
    if [ -x "$dir/cargo" ]; then
      PATH="$dir:$PATH"
      break
    fi
  done
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "[build] 未找到 cargo，请先安装 Rust 工具链（https://rustup.rs）" >&2
  exit 1
fi

echo "[build] cargo=$(command -v cargo)"
# Linux 上如需完全静态链接，可提前 `rustup target add $(uname -m)-unknown-linux-musl`
# 并设置 AGENT_OVERVIEW_MUSL=1，脚本会用 musl 目标编译（避免 glibc 版本依赖）。
TARGET=""
case "$(uname -s)/$(uname -m)" in
  Linux/x86_64)  MUSL_TRIPLE="x86_64-unknown-linux-musl" ;;
  Linux/aarch64) MUSL_TRIPLE="aarch64-unknown-linux-musl" ;;
  *) MUSL_TRIPLE="" ;;
esac
if [ -n "$MUSL_TRIPLE" ] && [ "${AGENT_OVERVIEW_MUSL:-0}" = "1" ]; then
  if rustup target list --installed 2>/dev/null | grep -q "$MUSL_TRIPLE"; then
    TARGET="$MUSL_TRIPLE"
  fi
fi

if [ -n "$TARGET" ]; then
  echo "[build] cargo build --release --locked --target $TARGET"
  cargo build --release --locked --target "$TARGET"
  BIN="target/$TARGET/release/herdr-agent-overview"
else
  echo "[build] cargo build --release --locked"
  cargo build --release --locked
  BIN="target/release/herdr-agent-overview"
fi

echo "[build] 完成：$BIN"
