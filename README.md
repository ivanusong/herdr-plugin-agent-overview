# herdr-plugin-agent-overview

A full-screen, read-only **agent overview for [Herdr](https://herdr.dev)**:
one live ANSI tile per agent pane, grouped by workspace, with a one-keystroke
jump back to the real pane. By default it shows only **working agents** —
press `f` to cycle the filter `Working → Done → Idle`.

Built in Rust with ratatui, derived from
[vjeantet/herdr-mission-control](https://github.com/vjeantet/herdr-mission-control)
(MIT). Thanks to the upstream author.

## Features

- **Live tiles** — every agent pane becomes a card with its real terminal
  viewport (`pane.read`, source `visible`, format `ansi`), refreshed at ~4 Hz,
  ANSI colors/styles and Unicode preserved, bottom-cropped to the latest
  output. Source panes are never resized, scrolled or written to.
- **Status filter** — only the selected official state is shown and read:
  `Working` (default), `Done`, `Idle`. `blocked` / `unknown` are not shown in
  this version. Cards appear/disappear automatically as agent state changes.
- **Keyboard + mouse** — grid navigation, `Enter` / double-click to focus the
  original pane and close the popup, wheel scrolling (mouse depends on your
  terminal forwarding mouse reports to the popup).
- **Safe toggle** — invoking the action twice closes only this plugin's own
  popup (pid + socket fingerprint), never someone else's popup.
- **Resilient** — disconnects keep the last data with a banner and reconnect
  automatically; a single unreadable pane never blocks the rest.

## Install

From a local checkout:

```bash
git clone https://github.com/<owner>/herdr-plugin-agent-overview.git
cd herdr-plugin-agent-overview
sh scripts/build.sh            # cargo build --release
herdr plugin link "$(pwd)"
```

Once this repository is published you can also install it directly:

```bash
herdr plugin install <owner>/herdr-plugin-agent-overview
```

Requirements: Herdr ≥ 0.9.4, a Rust toolchain (`rustup`), macOS or Linux.

## Keybinding

Add to `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+o"
type = "plugin_action"
command = "ivan.agent-overview.open"
description = "toggle agent overview"
```

Or invoke it directly: `herdr plugin action invoke ivan.agent-overview.open`

## Keys

| Key | Action |
| --- | --- |
| `f` | cycle status filter: `Working → Done → Idle` (shown in the footer) |
| `←↓↑→` / `h j k l` | move selection across the grid |
| `Tab` / `Shift+Tab` | cycle through all cards |
| `Enter` / double-click | focus the original pane and close the overview |
| single click | select a card |
| wheel | scroll the grid |
| `Esc` / `q` | close the overview |

## Notes

- Read-only: the only write the plugin performs is `pane.focus` when the user
  explicitly presses `Enter` or double-clicks a card.
- Previews are the pane's **visible viewport**, cropped to the live tail; the
  source pane's viewport, scroll position and size are never touched.
- No agents in the current filter shows `No working agents` /
  `No done agents` / `No idle agents`.

---

## 中文说明

全 Session 的 Agent 网格总览（Mission Control 风格）：一个全屏 popup 里按
workspace 分区展示 agent pane（每 pane 一卡），实时 ANSI 终端预览 + 官方状态灯，
默认只显示 **Working** 的 Agent（`f` 切 Done/Idle），方向键/鼠标选中，
Enter 跳回原 pane。

### 功能

- **状态筛选（默认 Working）**：只展示当前档位的 Agent——按 `f` 循环
  `Working → Done → Idle`，footer 显示 `Filter: <档位>`。
- **全 Session 发现**：`agent.list` + `session.snapshot` 映射出所有真实 agent，
  再按当前筛选档位过滤后按 workspace 分区；普通 shell、已退出但 pane 尚存
  的无主终端自动过滤；blocked/unknown 状态本版不展示。
- **只读实时预览**：默认 4Hz `pane.read`（`source=visible`，`format=ansi`），
  保留 ANSI/Unicode 样式，取可视区尾部裁切；不改变源 pane 的
  viewport/scroll/尺寸。读取并发受限（4），单卡读取失败不影响其它卡片；
  非当前档位的 pane 完全不读取。
- **官方状态**：仅 `working / blocked / idle / done / unknown` 五种（含颜色），
  不猜文本；卡片随状态变化自动增删（切档/刷新即生效）。
- **卡片信息**：状态灯 + Agent 名 + 类型 `[pi]`、会话焦点标记 `◈`、
  workspace 分区头、tab · 项目目录 · pane_id 信息条，过长按显示宽度截断。
- **导航**：方向键 / `hjkl` 网格移动，`Tab` / `Shift+Tab` 跨分区环游，
  滚动时选中按稳定 `terminal_id` 保持；窗口 resize 自适应网格（先收缩后滚动）。
- **跳转**：`Enter` 或双击卡片 → `pane.focus` 原 pane 后退出 popup
  （全插件唯一的写操作，仅在用户明确触发时执行）。
- **切换**：重复触发 action 只关闭**本插件自己的** popup，绝不误关其它
  popup（通过插件 state 目录的 pid + socket 指纹判定属主）。
- **断线韧性**：socket 断开时保留最后一次数据、顶部红色横幅提示、每 tick
  自动重试恢复；`events.subscribe` 作为刷新加速器，订阅失败自动退避重订，
  轮询始终兜底。
- 当前档位无 agent 时显示对应文案：`No working agents` / `No done agents` / `No idle agents`。

### 安装（本地开发）

```bash
cd /path/to/herdr-plugin-agent-overview
sh scripts/build.sh          # cargo build --release
herdr plugin link "$(pwd)"
```

要求：Rust 工具链（rustup）；macOS（arm64/x86_64）与 Linux 原生支持。

### 绑定快捷键与键位

```toml
# ~/.config/herdr/config.toml
[[keys.command]]
key = "prefix+o"
type = "plugin_action"
command = "ivan.agent-overview.open"
description = "toggle agent overview"
```

键位：`f` 切筛选（Working→Done→Idle）、`←↓↑→`/`hjkl` 移动、`Tab`/`Shift+Tab`
环游、`Enter`/双击跳转、单击选中、滚轮滚动、`Esc`/`q` 关闭（详见上方英文表）。

### Headless 诊断

不进 TUI，直接读取真实 snapshot / agent.list / 一条 pane 预览并打印：

```bash
HERDR_SOCKET_PATH=~/.config/herdr/herdr.sock \
  ./target/release/herdr-agent-overview --check
# 或指定 socket：
./target/release/herdr-agent-overview --check --socket ~/.config/herdr/herdr.sock
```

如需查看插件内部刷新/事件日志，仅在调试时设置：

```bash
AGENT_OVERVIEW_DEBUG=1 \
  HERDR_SOCKET_PATH=~/.config/herdr/herdr.sock \
  ./target/release/herdr-agent-overview
```

日志会写入 Herdr 插件 state 目录的 `agent-overview.log`（512KB 自动轮转）。
默认不写 stdout/stderr，避免 4Hz 刷新日志污染 popup 画面。

### 测试

```bash
cargo test --locked     # 单元测试（ANSI/发现/状态/布局/导航/toggle）
                        # + mock Unix socket 集成测试
                        #   （三档筛选切换/动态增减/状态变化/单卡失败/断线恢复/20 agents）
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
```

CI 在 ubuntu-latest 与 macos-latest 上运行同样的检查（见 `.github/workflows/ci.yml`）。

### 验收矩阵

| 能力 | 验证方式 | 状态 |
| --- | --- | --- |
| agent.list + snapshot 映射、三档筛选（默认 Working）、过滤 shell/已退出/blocked/unknown | 单元测试 + mock 集成测试 | ✅ 已测（模拟） |
| 官方五态状态、非官方值回落 unknown | 单元测试 + mock 集成测试 | ✅ 已测（模拟） |
| 筛选切换（f：Working→Done→Idle）、状态变化卡片自动增删、非当前档不 pane.read | 单元测试 + mock 集成测试 + 真实 PTY 抓帧 | ✅ 已测（模拟+真实 PTY） |
| 空状态分档文案（No working/done/idle agents）、footer 显示 Filter | 单元测试（TestBackend 文案/渲染） | ✅ 已测 |
| 布局边界：恰满 total==body、不足/恰满不可滚、滚动上限到内容底、末行完整下边框 | 单元测试（plan + TestBackend 渲染）+ 真实 PTY 屏幕重建 | ✅ 已测（模拟+真实 PTY） |
| 动态增减 agent、选中按 terminal id 保持 | mock 集成测试 | ✅ 已测（模拟） |
| 单卡读取失败隔离与恢复、失败退避 | mock 集成测试 | ✅ 已测（模拟） |
| socket 断线保留数据 + 自动恢复 | mock 集成测试 | ✅ 已测（模拟） |
| 20 agents 全部读取（并发限 4） | mock 集成测试 | ✅ 已测（模拟） |
| 真实 socket snapshot/agent.list/pane.read、默认筛选 | herdr 实例上 `--check` | ✅ 已测（真实） |
| 真实 TUI 渲染、4Hz 刷新、三档切换、无日志污染 | pty 无头运行（真实 socket）捕获/重建屏幕 | ✅ 已测（真实，仅无头） |
| popup 打开/关闭、Enter 跳转 | herdr 实例上 link 后在真实 attached client 中操作 | ✅ 已测（真实） |
| 鼠标单击/双击/滚轮 | synthetic SGR PTY（真实 socket）：双击跳转（含启动窗口时序）、单击仅选中、header 不误触、滚轮滚动 | ✅ synthetic PTY 已测；真实 popup 实机物理点击待用户验收（synthetic bytes 可能被 attached client 丢弃，属传输层行为） |
| toggle 只关闭本插件 popup | 本插件打开→重复 action 关闭；非本插件 popup 为代码路径+单测 | ✅ 本插件路径已验证 |
| macOS ARM release 构建 | 本机 cargo build --release + test | ✅ 已测（真实） |
| Linux aarch64-musl 编译 | `cargo check --target` 类型检查通过 | ⚠️ 仅类型检查，未产出/未运行 Linux 二进制 |
| events.subscribe 真实订阅 | 真实 socket 订阅成功（subscription_started）；事件驱动加速的实机效果 | ⚠️ 连接已测，加速效果未量化 |

### Linux 编译

代码只依赖 std、libc、ratatui/crossterm/serde/unicode-width，均支持 Linux。
宿主直接 `cargo build --release` 即可；如需免 glibc 版本依赖的静态二进制：

```bash
rustup target add x86_64-unknown-linux-musl   # 或 aarch64-unknown-linux-musl
AGENT_OVERVIEW_MUSL=1 sh scripts/build.sh
```

### 设计说明

- 直接使用 `HERDR_SOCKET_PATH` 的 newline-delimited JSON 协议（一条连接一个
  请求，herdr 服务器语义），不经过 CLI 子进程。
- 元数据每 tick 全量重建（snapshot + agent.list），预览按 `terminal_id`
  保留旧值，避免闪烁；选中跨重建保持。
- 过滤顺序：先按当前档位（只接受对应官方状态），再要求存在 agent 身份
  （AgentInfo/PaneInfo 的 agent / display_agent / name / title / agent_session
  任一非空）；blocked/unknown 状态不在任何档位内，永不显示。
- 事件订阅失败/`events_lost` 只影响刷新延迟（≤250ms），不影响正确性。

## License

MIT — 见 [LICENSE](LICENSE)（含上游 herdr-mission-control 版权声明）。