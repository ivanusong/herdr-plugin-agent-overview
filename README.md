# herdr-agent-overview

全 Session 的 Agent 网格总览（Mission Control 风格）for [Herdr](https://herdr.dev)。
一个全屏 popup 里按 workspace 分区展示**所有** coding agent（每 pane 一卡）：
实时 ANSI 终端预览 + 官方五态状态灯，默认只显示 **Working** 的 Agent（`f` 切 Done/Idle），方向键/鼠标选中，Enter 跳回原 pane。

基于 [vjeantet/herdr-mission-control](https://github.com/vjeantet/herdr-mission-control)
（MIT）的 Rust/ratatui 网格布局与 ANSI 渲染改造，感谢上游。

## 功能

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
  不猜文本；卡片随状态变化自动增删（切档/切换刷新即生效）。
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

## 安装（本地开发）

```bash
cd /path/to/herdr-agent-grid
sh scripts/build.sh          # cargo build --release
herdr plugin link "$(pwd)"
```

要求：Rust 工具链（rustup）；macOS（arm64/x86_64）与 Linux 原生支持。

## 绑定快捷键

在 `~/.config/herdr/config.toml` 增加（或用命令面板触发 action）：

```toml
[[keys.command]]
key = "prefix+o"
type = "plugin_action"
command = "agent-overview.open"
description = "toggle agent overview"
```

手动打开/关闭：

```bash
herdr plugin action invoke agent-overview.open
```

## 键位

| 输入 | 动作 |
| --- | --- |
| `f` | 切换状态筛选：`Working → Done → Idle` 循环（footer 显示当前档位） |
| `←↓↑→` / `h j k l` | 网格移动选中 |
| `Tab` / `Shift+Tab` | 扁平顺序环游全部卡片 |
| `Enter` / 双击卡片 | `pane.focus` 跳回原 pane 并关闭总览 |
| 单击卡片 | 选中 |
| 滚轮 | 滚动网格（暂离选中跟随） |
| `Esc` / `q` | 关闭总览 |

## Headless 诊断

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

## 测试

```bash
cargo test            # 单元测试（ANSI/发现/状态/布局/导航/toggle）
                      # + mock Unix socket 集成测试
                      #   （三档筛选切换/动态增减/状态变化/单卡失败/断线恢复/20 agents）
cargo clippy --all-targets
cargo fmt --check
```

## 验收矩阵

| 能力 | 验证方式 | 状态 |
| --- | --- | --- |
| agent.list + snapshot 映射、三档筛选（默认 Working）、过滤 shell/已退出/blocked/unknown | 单元测试 + mock 集成测试 | ✅ 已测（模拟） |
| 官方五态状态、非官方值回落 unknown | 单元测试 + mock 集成测试 | ✅ 已测（模拟） |
| 筛选切换（f：Working→Done→Idle）、状态变化卡片自动增删、非当前档不 pane.read | 单元测试 + mock 集成测试 + 真实 PTY 抓帧 | ✅ 已测（模拟+真实 PTY） |
| 空状态分档文案（No working/done/idle agents）、footer 显示 Filter | 单元测试（TestBackend 文案/渲染） | ✅ 已测 |
| 布局边界：恰满 total==body、不足/恰满不可滚、滚动上限到内容底、末行完整下边框 | 单元测试（plan + TestBackend 渲染）+ 真实 PTY 屏幕重建 | ✅ 已测（模拟+真实 PTY） |
| 动态增减 agent、选中按 terminal id 保持 | mock 集成测试 | ✅ 已测（模拟） |
| 单卡读取失败隔离与恢复 | mock 集成测试 | ✅ 已测（模拟） |
| socket 断线保留数据 + 自动恢复 | mock 集成测试 | ✅ 已测（模拟） |
| 20 agents 全部读取（并发限 4） | mock 集成测试 | ✅ 已测（模拟） |
| 真实 socket snapshot/agent.list/pane.read | 本机 herdr 0.9.4/0.9.5 实例 `--check` | ✅ 已测（真实） |
| 3 workspaces × 2 agents、同一 tab 多 agent | 临时真实 Herdr session 启动 6 个空闲 Pi 后 `--check` | ✅ 已测（真实） |
| 新增/退出 agent 自动增减卡片 | 临时真实 session 新增第 7 个 Pi、关闭其 tab 后回落 6 个 | ✅ 已测（真实，headless 聚合层） |
| 真实 TUI 渲染与 4Hz 刷新 | pty 无头运行（120x40）捕获帧 | ✅ 已测（真实，仅无头） |
| popup 打开/关闭、Enter 跳转 | 已 link 到本机 Herdr，在真实 attached client 中操作 | ✅ 已测（真实） |
| 鼠标单击/双击/滚轮 | synthetic SGR PTY（真实 socket）：双击跳转 3/3（含启动窗口时序）、单击仅选中、header 不误触、滚轮滚动 | ✅ synthetic PTY 已测；真实 popup 实机物理点击待验收（synthetic bytes 可能被 attached client 丢弃，属传输层行为） |
| toggle 只关闭本插件 popup | 本插件打开→重复 action 关闭已实测；“其它 popup 不关闭”为代码路径+单测 | ✅ 本插件路径真实；非本插件 popup 仅模拟/代码审查 |
| macOS ARM release 构建 | 本机 cargo build --release + test | ✅ 已测（真实） |
| Linux aarch64-musl 编译 | `cargo check --target` 类型检查通过 | ⚠️ 仅类型检查，未产出/未运行 Linux 二进制 |
| events.subscribe 真实订阅 | 真实 socket 订阅成功（subscription_started）；事件驱动加速的实机效果 | ⚠️ 连接已测，加速效果未量化 |

## Linux 编译

代码只依赖 std、libc、ratatui/crossterm/serde/unicode-width，均支持 Linux。
宿主直接 `cargo build --release` 即可；如需免 glibc 版本依赖的静态二进制：

```bash
rustup target add x86_64-unknown-linux-musl   # 或 aarch64-unknown-linux-musl
AGENT_OVERVIEW_MUSL=1 sh scripts/build.sh
```

## 设计说明

- 直接使用 `HERDR_SOCKET_PATH` 的 newline-delimited JSON 协议（一条连接一个
  请求，herdr 服务器语义），不经过 CLI 子进程。
- 元数据每 tick 全量重建（snapshot + agent.list），预览按 `terminal_id`
  保留旧值，避免闪烁；选中跨重建保持。
- “已退出但 pane 尚存”的过滤只用官方字段：AgentInfo/PaneInfo 的
  agent / display_agent / name / title / agent_session 全空且状态 unknown
  → 视为无主终端，不显示。保留 agent_session（herdr 的 resume 引用）的
  agent 仍会显示（herdr 官方仍视其为 agent 终端）。
- 事件订阅失败/`events_lost` 只影响刷新延迟（≤250ms），不影响正确性。

## License

MIT — 见 [LICENSE](LICENSE)（含上游 herdr-mission-control 版权声明）。
