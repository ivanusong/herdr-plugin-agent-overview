//! herdr-agent-overview：全 Session 的 Agent 网格总览。
//!
//! 模块划分：
//! - [`api`]：herdr Unix socket NDJSON 客户端（一次请求一条连接）。
//! - [`ansi`]：SGR/ANSI 解析（派生自 herdr-mission-control，MIT）。
//! - [`discovery`]：`agent.list` + `session.snapshot` → Agent 卡片（过滤普通 shell / 已退出）。
//! - [`state`]：共享状态与卡片数据结构。
//! - [`refresh`]：后台 4Hz 刷新线程 + 并发受限读取池 + events.subscribe 加速器。
//! - [`ui`]：自适应网格布局与渲染（派生自 herdr-mission-control，MIT）。
//! - [`toggle`]：popup 打开/关闭切换（只关本插件的 popup）。

pub mod ansi;
pub mod api;
pub mod discovery;
pub mod input;
pub mod refresh;
pub mod state;
pub mod toggle;
pub mod ui;
