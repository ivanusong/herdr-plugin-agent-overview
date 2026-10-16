# herdr-plugin-agent-overview 中文说明

[English README](../README.md)

全屏、只读的 **Herdr Agent 总览**：每个 Agent pane 一张实时 ANSI 卡片，
按 workspace 分区，一键跳回原 pane。默认只显示 **Working** 的 Agent，
按 `f` 在 `Working → Done → Idle` 之间循环切换。

基于 Rust + ratatui 实现，派生自
[vjeantet/herdr-mission-control](https://github.com/vjeantet/herdr-mission-control)
（MIT），感谢上游作者。

## 核心功能

- **实时卡片** — 每个可见 Agent pane 一张卡，显示真实终端可视区
  （`pane.read`，`source=visible`，`format=ansi`），约 4Hz 刷新，保留
  ANSI 颜色/样式与 Unicode，裁切到最新输出；不改变源 pane 的
  viewport/scroll/尺寸。
- **状态筛选** — 只展示并读取当前档位的官方状态：`Working`（默认）、
  `Done`、`Idle`；本版不展示 `blocked` / `unknown`。卡片随 Agent 状态变化
  自动增删；空档位显示 `No working agents` / `No done agents` /
  `No idle agents`。
- **键盘与鼠标** — 网格导航、`Tab` 环游、`Enter` / 双击聚焦原 pane 并关闭
  总览、滚轮滚动（鼠标需要宿主终端把 mouse report 转发给 popup）。
- **安全切换** — 重复触发 action 只关闭本插件自己的 popup（pid + socket
  指纹判定），不会误关其它 popup。
- **断线韧性** — socket 断开时保留最后一次数据并显示横幅，自动重连；单个
  pane 读取失败不影响其它卡片。

## 安装

```bash
git clone https://github.com/ivanusong/herdr-plugin-agent-overview.git
cd herdr-plugin-agent-overview
sh scripts/build.sh            # cargo build --release
herdr plugin link "$(pwd)"
```

或直接从 GitHub 安装：

```bash
herdr plugin install ivanusong/herdr-plugin-agent-overview
```

要求：Herdr ≥ 0.9.4、Rust 工具链（`rustup`）、macOS 或 Linux。

## 快捷键

在 `~/.config/herdr/config.toml` 中增加：

```toml
[[keys.command]]
key = "prefix+o"
type = "plugin_action"
command = "ivan.agent-overview.open"
description = "toggle agent overview"
```

或直接调用：

```bash
herdr plugin action invoke ivan.agent-overview.open
```

## 键位

| 输入 | 动作 |
| --- | --- |
| `f` | 切换状态筛选：`Working → Done → Idle`（footer 显示当前档位） |
| `←↓↑→` / `h j k l` | 网格移动选中 |
| `Tab` / `Shift+Tab` | 环游全部卡片 |
| `Enter` / 双击卡片 | 聚焦原 pane 并关闭总览 |
| 单击卡片 | 选中 |
| 滚轮 | 滚动网格 |
| `Esc` / `q` | 关闭总览 |

## License

MIT — 见 [LICENSE](../LICENSE)，其中包含上游 herdr-mission-control
版权声明。
