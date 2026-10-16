#!/bin/sh
# Action 入口：切换 Agent Overview popup。
# 重复触发时关闭本插件自己的 popup；绝不关闭其它插件/会话的 popup（见 src/toggle.rs）。
set -e
cd "$(dirname "$0")/.."
exec ./target/release/herdr-agent-overview toggle
