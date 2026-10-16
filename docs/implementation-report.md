# herdr-plugin-agent-overview 实现报告

本报告记录设计与返工结论：可复现的测试、限制与设计决策。环境特定的本地
session/pane/路径细节已清理。

## 返工记录（主 Agent 复核后）

### 第六轮：三档状态筛选 + 底部滚动复核（用户需求变更）

**需求**：默认只显示 Working 的 Agent；`f` 键循环 `Working → Done → Idle`；非当前档不 pane.read；空状态/footer 显示当前档位。另用户反馈“底部最后 pane 底线没展示、还能向下滚一两行”。

**实现**：
- `state::FilterMode`（Working/Done/Idle，`next()`/`label()`/`accepts()`），`Shared.filter` 为唯一真源（默认 Working）。
- `discovery::build_sections(snapshot, agents, filter)` 在卡片构建层过滤：非当前档不进入卡片、不派发 pane.read；状态变化随下一轮刷新自动增删。
- `input::handle_key` 新增 `f → Action::CycleFilter`；main.rs 改 `shared.filter` 后 `wakeup.notify()` 让刷新线程尽快重建（≤120ms）。
- footer 右侧显示 `Filter: <档位> · ⟳ N · 4Hz`，帮助行含 `f filter`（窄终端缩减分支也保留 f 提示）；空状态按档位 `No working/done/idle agents`。

**底部滚动复核（用数据定论，未臆改）**：
- 新增 TestBackend/plan 测试锁定：恰满时 `plan.total_height == body.height` 且滚轮不可滚（scroll 恒 0）；内容不足时同样不可滚；滚动模式（`MIN_TILE_H=12` 保底导致内容超视口）下 offset 上限 = total−viewport，滚到底最后一行必为卡片下边框。
- 真实 PTY（121×40）屏幕重建实测：默认档末行（body 第 39 行）为完整 `└──…` 边框、无空白；滚轮 ×10 后末行不变、footer 不变（无“无内容下滚”）；档位卡数超出视口时进入滚动模式，滚动范围仍以内容底为界。
- 结论：虚拟画布/渲染循环无 off-by-one；“能滚一两行”的场景是内容超出视口时保 12 行卡片下限的正常滚动，不是滚出空白。README 验收矩阵已如实记录。

**测试**（新增/重写）：`filter_mode_cycles_and_accepts_only_its_status`、`filter_mode_selects_matching_cards`、`f_cycles_filter_action`、`empty_state_copy_matches_filter_mode`、`footer_shows_current_filter`（120 宽）、`plan_exact_fit_equals_viewport_height`、`exact_fit_viewport_cannot_scroll_and_shows_bottom_border`、`short_content_fills_viewport_and_cannot_scroll`、`scrolling_mode_clamps_offset_to_last_content_row`、mock 集成 `filter_switch_changes_cards_and_stops_reading_other_states`。

**复验**：真实 socket PTY：默认 `Filter: Working` 只显示 working 卡；`f` → Done（footer `Filter: Done`）、`f` → Idle（`Filter: Idle`）、再 `f` 回 Working；Enter 回归 exit 0。`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`（0 warning）、`cargo test`（43 单元 + 10 集成全过）、release 构建全部通过。


### 第五轮：TUI 日志污染终端 + 失败 pane 4Hz 连打（用户实机反馈）

**现象**：Overview 打开后内容不断向上滚动，持续刷“一轮完成 N panes / pane 读取失败”日志。

**根因**：
1. `refresh::log` 用 `eprintln!`，与 popup PTY 共享 stderr → 4Hz 日志污染 alternate screen；
2. 真实 session 中存在 restore_error 的 pane（snapshot/agent.list 可见、保留 agent_session 身份，但 runtime 不可用），`pane.read` 每轮 `pane_not_found` → 持续失败日志。

**修复**：
1. **日志 sink 抽象**：默认完全静默（`LogSink::Disabled`，不写 stdout/stderr）；仅 `AGENT_OVERVIEW_DEBUG=1` 时写 `HERDR_PLUGIN_STATE_DIR/agent-overview.log`（512KB 上限轮转 `.log.1`）；写入失败不反噬主流程。toggle 显式用户输出仍走 stdout/stderr（非 TUI）。
2. **读取失败退避**（`ReadBackoff`）：按 pane_id 记录失败，退避序列 500ms→1s→2s→4s→5s 封顶；成功即清零；仅首次失败/恢复各记一条日志；卡片保留旧预览与红色 `⚠ <错误>` 信息条；拓扑移除则自然不再读取。
3. 跑验时另发现并修复：`collect_until` 的 expected 误用卡片总数，退避跳过时每轮白等 1.5s 超时（刷新频率掉到 0.7Hz）——改为实际派发数后实测恢复 4Hz。

**测试**（新增 6 个）：退避首次阻止 500ms 内重试并逐步翻倍、封顶与成功清零、未启用 debug 时日志不产生任何文件、日志写入与轮转、失败卡片渲染 `⚠ pane_not_found` 信息条、默认静默行为。

**复验**（真实 socket PTY）：默认 6s / 调试 5s，pty 输出均 **0 条日志**（无 [INFO]/一轮完成/读取失败/pane_not_found），画面稳定；调试模式日志只落盘（5s ≈ 4Hz）；不可用 pane 仅首次退避一条记录（退避序列把 5s 内读尝试从 ~20 次降到 ~5 次）；Enter 回归 exit 0。

### 第四轮：鼠标首击丢失打断双击链（PTY 验收发现）

**现象**：PTY 中发送 SGR 双击序列（press+release 两遍）不跳转；调试日志实证 `DBG-HIT hit=None selected=None`（首击）→ `hit=Some((0,0)) selected=Some((0,0))`（次击仅选中）。

**根因**：与 Enter 同类的启动窗口竞态——首击到达时 `tile_rects`/`selected` 仍是空帧遗留；首击未命中导致 `last_click` 未记录，第二击只算单击。

**修复**：
1. 鼠标 Down 处理时若 `tile_rects` 为空而 shared 已有卡，先补绘一帧再命中（与 `handle_key` 的 sync 同理）；
2. 点击决策抽为可测的 `input::handle_click` + `input::ClickTracker`（时间注入，双击窗口 500ms），main.rs 改用。

**测试**：新增 3 个单测——双击同卡窗口内成立、换卡/超窗不成立、单击选中→双击跳转语义。

**复验**（真实 socket PTY，synthetic SGR bytes）：等日志立即双击 **3/3 跳转成功**（焦点转移到默认第一卡对应 pane，验收后已恢复原焦点）；单击仅选中；header 行点击不误触；滚轮不退出；q 退出且状态清理。协议说明：attached Herdr client 对 synthetic input bytes 可能不转发（传输层行为，与本插件无关），真实 popup 实机鼠标未声称已验收（README 验收矩阵已如实区分）。

### 第二轮：Enter 在启动窗口内被静默吞掉（PTY 验收发现）

**现象**：PTY 中等首轮刷新日志出现后立即写 `\r`，进程不退出、不调 pane.focus；`q` 正常。

**根因**（调试日志实证 `DBG key=Enter selected=None total=N connected=true`）：选中映射只在每帧 `draw()` 开头的 `sync_selection` 维护；首轮刷新把卡片写入 shared 后、下一帧绘制前的窗口（≤50ms）内按键到达时 `selected` 仍是 None，Enter/导航键被静默吞掉。「等日志立即写」必然落入窗口（修复前 5/5 复现）。

**修复**：新建 `src/input.rs`，把键盘决策抽为可测的 `input::handle_key`，**处理前先 `sync_selection`**（选中映射与渲染帧节奏解耦）；`main.rs` event_loop 改用该函数。

**测试**：新增 5 个用例——启动竞态下 Enter 打开默认第一卡、无卡 Enter 不退出、q/Esc 退出、导航后 Enter 用当前选中、重建后选中回退不吞键。

**复验**（真实 socket PTY，等日志立即写 Enter，waitpid 每轮调用）：修复前 5/5 失败 → 修复后 **5/5 成功**：exit 0、聚焦默认第一卡对应 pane、状态文件清理；每次验收后已调 `pane.focus` 恢复原焦点。`q`/`Esc` 退出 status=0 且清状态。注：raw mode 下 `\n`（0xA）被 crossterm 刻意解析为 Ctrl+J（issue #371），真实终端 Enter 发送 `\r`，主路径不受影响。

### 第一轮：两处 PRD 字面不一致

1. **状态颜色改按 PRD 推荐**（`src/ui.rs status_style`）：Working=Green、Blocked=Yellow、Done=Blue、Idle/Unknown=Gray；删除过弱的 `status_styles_cover_all_states`，新增 `status_colors_match_prd` 逐一断言图标+颜色映射，防止回归。
2. **空状态文案改按 PRD 字面**（`src/ui.rs draw_empty`）：第一行 `No active agents`、空行、`Start an agent in Herdr to see it here.`、空行、`Press Esc to close.`（保留居中/灰色样式）；新增 `empty_state_copy_matches_prd` 用 ratatui TestBackend 实际渲染并断言三行文案精确出现、旧中文提示不再存在。（注：该文案后随需求变更改为按筛选档位生成，见下。）

返工后验证：`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`（0 warning）、`cargo test`（33 单元 + 9 集成全过）、`cargo build --release` 全部通过。

### 第三轮：主 Agent 真实 Herdr 验收

- 插件已通过 `herdr plugin link` 安装到真实默认 session（server/CLI protocol 22）。
- `--check` 在真实默认 session 中发现多个官方 agent、跨多个 workspace，`pane.read` ANSI 正常；普通 shell 未进入卡片。
- 真实 action 打开 popup 后 TUI 进程与 state 文件存在；重复 action 识别 pid + socket 指纹，仅关闭本插件 popup，进程与 state 文件清理。
- 真实 attached Herdr client 中确认全屏卡片、状态/预览/footer 渲染与 4Hz 刷新；发送 Enter 后聚焦默认第一卡对应 pane，popup 关闭且 state 清理。验收后已恢复原焦点。
- 在临时独立 session 中启动多个真实空闲 Pi agent（多 workspace、多 pane），`--check` 全部发现；新增后计数上升，关闭其 tab 后回落。测试 session 已停止。

## 一、完成内容

按任务要求交付**完整 P0**（非原型）：全屏 popup 的全 Session Agent 网格总览。

### 功能清单与实现方式

| 要求 | 实现 |
| --- | --- |
| plugin id `ivan.agent-overview`、action `ivan.agent-overview.open`、entrypoint `overview` | `herdr-plugin.toml`：id/action id/pane id 均按规范命名；manifest 声明 `placement="popup"`、`width/height="100%"`（全屏） |
| 重复 open toggle，只关自己的 popup | `src/toggle.rs`：TUI 启动时在 `HERDR_PLUGIN_STATE_DIR/popup.json` 写 `{pid, socket_path}`；toggle 时仅当「状态文件存在 + socket 一致 + 进程存活」才 `popup.close`；`ui_busy`（其它 popup/Settings 模态）时只报错不关闭。残留文件按 pid 存活自动清理 |
| 直接 Unix socket API | `src/api.rs`：NDJSON 协议、一条连接一个请求（与 herdr server 语义一致），读写超时 1s/2s，保留错误码（`ui_busy`/`popup_not_open` 等供分支） |
| 全 Session 发现 + 正确映射 | `agent.list` 为主（失败回落 `snapshot.agents`），join `session.snapshot` 的 pane/tab/workspace 记录；卡片段按 `workspace.number → tab.number → pane 顺序` 排序分区 |
| 稳定 pane/terminal id | 卡片以 `terminal_id` 为稳定标识（跨 `pane.move`/跨 workspace 移动不变），选中跨 4Hz 重建保持 |
| 过滤普通 shell / 已退出但 pane 尚存 | 先按当前筛选档位过滤（默认 Working，`f` 循环 Done/Idle；blocked/unknown 不在任何档）；再要求存在 agent 身份（agent/display_agent/name/title/agent_session 任一非空）；pane 已从 snapshot 消失则丢卡。只用官方字段，不猜文本 |
| 仅官方五态 | `Status::parse`：working/blocked/idle/done/unknown，其余一律回落 unknown（label 仍输出官方词）；三档筛选只接受 working/done/idle |
| 4Hz `pane.read` visible/ansi、底部裁切、不动源 pane | `src/refresh.rs`：4Hz 全量刷新；读取走 worker 池并发上限 4；ANSI 解析后取可视区尾部渲染（先裁尾部空行）；只读，不改 viewport/scroll/size |
| 元数据缓存、单卡失败不卡 UI | 元数据每 tick 重建时按 terminal_id 保留旧预览/旧错误；单卡 read 失败仅标记本卡（保留上次成功预览） |
| 网络与键盘独立、断线显示+自动恢复 | 刷新/读取/事件全部在后台线程；UI 线程只持锁渲染。断线保留最后一次数据 + 顶部红色横幅 + 每 tick 自动重试（mock 集成测试覆盖） |
| events.subscribe | 已实现为**加速器**：全局 lifecycle 事件 + 逐 pane `pane.agent_status_changed`，事件到达提前触发刷新（120ms 节流）；订阅失败/`events_lost` 退避重订，**轮询始终兜底**（已在代码注释/README 记载降级设计） |
| 自适应网格、resize、分页/滚动、选中保持 | `src/ui.rs`：mission-control 的先收缩后滚动布局（卡片下限 52×12），视口逐行滚动；选中按 terminal_id 保持；滚轮独立滚动 |
| 键盘/鼠标 | `←↓↑→`/`hjkl` 网格移动（行内循环、跨行对齐）、`Tab`/`Shift+Tab` 扁平环游、`Enter`=pane.focus 后退出、`Esc`/`q` 退出；鼠标单击选中/双击跳转/滚轮滚动 |
| 只读安全 | 全插件唯一写操作 = 用户 Enter/双击后的 `pane.focus`（TUI 拆除后、进程退出前执行）。无 pane.close/resize/zoom/send/restart/create-agent。已删除上游 mission-control 的 Backspace 关闭 pane 功能 |
| 空状态显示 | 按档位居中显示 `No working/done/idle agents`（早期版本的 `No active agents` 已随需求变更） |
| headless 诊断 | `--check`：打印 socket、workspace/tab/pane/agent 记录数、发现 agent 表（状态/名/类型/tab/cwd/pane_id）、一条 ANSI 预览；`--check --socket PATH` 可指定 |
| mock 集成测试 | `tests/mock_socket.rs`：内存 mock 服务器（单请求连接语义），覆盖发现/过滤、动态增减+选中保持、官方状态变化、单卡失败隔离与恢复、断线保留数据+自动恢复、agent.list 失败回落、20 agents 并发读取、pane.focus、网格导航 |

### 依赖

`ratatui`（含 crossterm）、`serde`、`serde_json`、`unicode-width`、`libc`（仅 `kill(pid,0)` 存活检查）。无其他运行时依赖。

## 二、验证结果

在 macOS ARM（aarch64-apple-darwin）上验证：

- `cargo fmt --check` ✅
- `cargo clippy --all-targets -- -D warnings` ✅ 0 warning / 0 error
- `cargo test --locked` ✅ 43 单元 + 10 集成 = **53 全过**
- `cargo build --release` ✅（`target/release/herdr-agent-overview`）
- Linux：`cargo check --target aarch64-unknown-linux-musl` ✅（**仅类型检查**；未产出、未运行 Linux 二进制，不做声称）
- 真实实例 smoke（通过 `HERDR_SOCKET_PATH` 连接真实 herdr server，protocol 22）：
  - `--check`：按默认档位（Working）发现真实 agent，官方状态正确，普通 shell 与已退出 pane 被过滤，`pane.read` ANSI 读取正常
  - pty 无头 TUI（120×40/121×40）：分区头、状态卡片（`○ working <agent> [pi]`）、信息条（`<tab> · <cwd> · <pane_id>`）、实时 ANSI 预览、footer（`Filter: …`）、`‹scrolling›`、断线横幅均正确渲染；4Hz 刷新每轮毫秒级；`q` 干净退出（alternate screen/鼠标捕获恢复、状态文件清理）
  - toggle 错误路径：插件未 link 时 `plugin.pane.open` 返回 `plugin_not_found` → 干净报错退出，无副作用
- 复现命令：见下方「文件路径/运行命令」

## 三、剩余限制

1. **物理鼠标未做实机验收**：单击/双击/滚轮已通过真实 socket + synthetic SGR PTY 验证，但本机 attached client 会丢弃 synthetic 输入，未用真实终端物理点击验收；键盘路径已在真实 attached popup 验收。
2. **Linux**：仅 `cargo check` 类型级验证；未交叉链接（无 musl linker），未在 Linux 运行。`scripts/build.sh` 提供 `AGENT_OVERVIEW_MUSL=1` 静态构建选项但未实测。
3. **events 加速效果未量化**：真实订阅建立成功（`subscription_started` 已验证），但"事件触发提前刷新"的体感延迟未测量；轮询 4Hz 兜底保证正确性不依赖它。
4. **toggle 的 pid 复用极端场景**：若 TUI 被强杀后其 pid 恰好被复用（概率极低），toggle 可能误判属主。socket 指纹已缓解跨会话误判。
5. **双击依赖终端鼠标转发**：与上游一致，鼠标事件依赖宿主终端把 mouse report 交给 popup；键盘始终可用（herdr popup 接收全部输入）。
6. **min_herdr_version = "0.9.4"**：所用 API（agent.list / session.snapshot / pane.read / pane.focus / plugin.pane.open / popup.close）已在真实 server 实例（protocol 22）上验证；0.9.4 为验证过的最低版本。
7. **README 中 prefix+o**：仅文档说明，未改用户 herdr 配置（遵守约束）。

## 四、文件路径 / 运行命令

### 文件

```
herdr-plugin-agent-overview/
├── herdr-plugin.toml        # manifest：id=ivan.agent-overview, action=open, pane=overview(popup 100%)
├── Cargo.toml / Cargo.lock  # 依赖锁定
├── LICENSE                  # MIT（含上游 mission-control 版权声明）
├── README.md                # 安装/prefix+o 配置/键位/验收矩阵
├── .gitignore               # /target
├── scripts/
│   ├── build.sh             # cargo build --release（macOS/Linux，可选 musl）
│   └── open.sh              # action 入口 → 二进制 toggle 子命令
├── src/
│   ├── main.rs              # 入口：TUI / toggle / --check / --version
│   ├── lib.rs               # 模块声明
│   ├── api.rs               # socket NDJSON 客户端（超时、错误码）
│   ├── ansi.rs              # SGR 解析（派生自 mission-control，MIT）
│   ├── discovery.rs         # agent.list+snapshot → 卡片（过滤/排序/映射）
│   ├── state.rs             # Card/Section/Shared/Status/工具
│   ├── refresh.rs           # 4Hz 刷新 + 4 并发读取池 + events 加速器
│   ├── input.rs             # 可测键盘决策层（修复启动窗口 Enter 竞态）
│   ├── toggle.rs            # popup 属主判定与切换
│   └── ui.rs                # 网格布局/渲染/导航（派生自 mission-control，MIT）
├── tests/
│   └── mock_socket.rs       # mock Unix socket 集成测试（10 个用例）
└── docs/
    └── implementation-report.md  # 本报告
```

### 命令（复现验证）

```bash
cd /path/to/herdr-plugin-agent-overview

sh scripts/build.sh                                    # 构建
cargo test --locked                                    # 53 测试
cargo clippy --all-targets --locked -- -D warnings     # lint
cargo fmt --check
HERDR_SOCKET_PATH=~/.config/herdr/herdr.sock \
  ./target/release/herdr-agent-overview --check        # 真实实例诊断

# 验收：
sh scripts/build.sh && herdr plugin link "$(pwd)"
herdr plugin action list --plugin ivan.agent-overview
herdr plugin action invoke ivan.agent-overview.open         # 打开；再次 invoke 关闭
# 可选快捷键（~/.config/herdr/config.toml）：
#   [[keys.command]]
#   key = "prefix+o"
#   type = "plugin_action"
#   command = "ivan.agent-overview.open"
#   description = "toggle agent overview"
```

### 备注

- 构建需要稳定版 Rust 工具链（rustup）；可选 `aarch64/x86_64-unknown-linux-musl`
  target 用于静态构建。
- 开发过程未修改任何用户全局配置。
