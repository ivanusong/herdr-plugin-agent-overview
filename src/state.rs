//! 共享状态：卡片、分区、连接状态。后台刷新线程写入，UI 线程短暂加锁读取。

use std::time::Instant;

use ratatui::text::Line;

/// 官方 agent 状态，仅五种；不可猜文本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Working,
    Blocked,
    Idle,
    Done,
    Unknown,
}

impl Status {
    /// 官方枚举之外的一切值一律回落 Unknown（不发明新状态名）。
    pub fn parse(raw: &str) -> Self {
        match raw {
            "working" => Status::Working,
            "blocked" => Status::Blocked,
            "idle" => Status::Idle,
            "done" => Status::Done,
            _ => Status::Unknown,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Status::Working => "working",
            Status::Blocked => "blocked",
            Status::Idle => "idle",
            Status::Done => "done",
            Status::Unknown => "unknown",
        }
    }
}

/// 状态筛选档位（footer 显示、`f` 键循环）：默认 Working → Done → Idle。
/// 其它官方状态（blocked/unknown）本版不展示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterMode {
    #[default]
    Working,
    Done,
    Idle,
}

impl FilterMode {
    /// 循环顺序：Working → Done → Idle → Working。
    pub fn next(self) -> Self {
        match self {
            FilterMode::Working => FilterMode::Done,
            FilterMode::Done => FilterMode::Idle,
            FilterMode::Idle => FilterMode::Working,
        }
    }

    /// 显示名（footer / 空状态文案）。
    pub fn label(self) -> &'static str {
        match self {
            FilterMode::Working => "Working",
            FilterMode::Done => "Done",
            FilterMode::Idle => "Idle",
        }
    }

    /// 对应的小写状态词（空状态第一行 `No working agents`）。
    pub fn label_lower(self) -> &'static str {
        match self {
            FilterMode::Working => "working",
            FilterMode::Done => "done",
            FilterMode::Idle => "idle",
        }
    }

    /// 该档位对应的官方状态。
    pub fn status(self) -> Status {
        match self {
            FilterMode::Working => Status::Working,
            FilterMode::Done => Status::Done,
            FilterMode::Idle => Status::Idle,
        }
    }

    /// 官方状态字符串是否命中当前档位（非官方值一律不命中）。
    pub fn accepts(self, raw: &str) -> bool {
        Status::parse(raw) == self.status()
    }
}

/// 一张卡片 = 一个真实 Agent 所在的 pane。
#[derive(Debug, Clone)]
pub struct Card {
    /// 稳定标识：跨 pane.move/cross-workspace 移动不变，用于选中保持。
    pub terminal_id: String,
    /// 当前公共 pane id（用于 pane.read / pane.focus）。
    pub pane_id: String,
    /// 展示名：display_agent > name > agent > terminal_title_stripped > pane_id。
    pub title: String,
    /// Agent 类型（pi / claude / codex / …，官方 `agent` 字段）。
    pub agent_kind: Option<String>,
    pub status: Status,
    pub workspace_id: String,
    pub workspace_label: String,
    pub workspace_number: usize,
    pub tab_id: String,
    pub tab_label: String,
    /// 项目目录名（foreground_cwd 优先，取 basename）。
    pub cwd_label: String,
    /// 该 pane 是否是会话当前聚焦 pane。
    pub focused: bool,
    /// pane 可视区 ANSI 渲染结果（后台线程解析好，UI 直接画）。
    pub preview: Vec<Line<'static>>,
    /// 最近一次读取失败原因（保留上一次成功预览）。
    pub read_error: Option<String>,
}

/// 一个 workspace 分区（分区头 + 其下卡片）。
#[derive(Debug, Clone)]
pub struct Section {
    pub workspace_id: String,
    pub label: String,
    pub number: usize,
    pub cards: Vec<Card>,
}

impl Section {
    pub fn card_count(&self) -> usize {
        self.cards.len()
    }
}

/// 后台刷新与 UI 之间共享的可变状态。
#[derive(Debug, Default)]
pub struct Shared {
    pub sections: Vec<Section>,
    /// socket 最近一次交互是否成功。
    pub connected: bool,
    /// 最近一次错误（断线原因等），UI 顶部横幅展示。
    pub last_error: Option<String>,
    pub last_refresh: Option<Instant>,
    /// 当前状态筛选档位（刷新线程据此构建卡片，UI 按 `f` 循环）。
    pub filter: FilterMode,
}

impl Shared {
    /// 扁平卡片总数。
    pub fn total_cards(&self) -> usize {
        self.sections.iter().map(|s| s.cards.len()).sum()
    }

    /// 按 terminal_id 查卡片，返回 (section_idx, card_idx)。
    pub fn find(&self, terminal_id: &str) -> Option<(usize, usize)> {
        for (si, section) in self.sections.iter().enumerate() {
            for (ci, card) in section.cards.iter().enumerate() {
                if card.terminal_id == terminal_id {
                    return Some((si, ci));
                }
            }
        }
        None
    }

    /// 按 (section, card) 下标取卡片。
    pub fn card_at(&self, pos: (usize, usize)) -> Option<&Card> {
        self.sections.get(pos.0).and_then(|s| s.cards.get(pos.1))
    }

    /// 按扁平时序取卡片（Tab 环游用）。
    pub fn card_flat(&self, index: usize) -> Option<(usize, usize, &Card)> {
        let mut count = 0;
        for (si, section) in self.sections.iter().enumerate() {
            for (ci, card) in section.cards.iter().enumerate() {
                if count == index {
                    return Some((si, ci, card));
                }
                count += 1;
            }
        }
        None
    }
}

/// 以 basename 形式展示 cwd（Home 目录缩写为 ~）。
pub fn cwd_label(cwd: &str) -> String {
    let home = std::env::var("HOME").ok();
    let mut path = cwd.trim_end_matches('/').to_string();
    if path.is_empty() {
        path = "/".to_string();
    }
    if let Some(home) = home {
        let home = home.trim_end_matches('/');
        if !home.is_empty() && (path == home || path.starts_with(&format!("{home}/"))) {
            path = format!("~{}", &path[home.len()..]);
        }
    }
    path.rsplit('/').next().unwrap_or(&path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parse_only_official_values() {
        assert_eq!(Status::parse("working"), Status::Working);
        assert_eq!(Status::parse("blocked"), Status::Blocked);
        assert_eq!(Status::parse("idle"), Status::Idle);
        assert_eq!(Status::parse("done"), Status::Done);
        assert_eq!(Status::parse("unknown"), Status::Unknown);
        // 非官方值不可猜：一律 Unknown，且 label 仍是官方词。
        assert_eq!(Status::parse("running"), Status::Unknown);
        assert_eq!(Status::parse(""), Status::Unknown);
        assert_eq!(Status::parse("running").label(), "unknown");
    }

    #[test]
    fn filter_mode_cycles_and_accepts_only_its_status() {
        // 默认 Working；循环 Working → Done → Idle → Working。
        assert_eq!(FilterMode::default(), FilterMode::Working);
        assert_eq!(FilterMode::Working.next(), FilterMode::Done);
        assert_eq!(FilterMode::Done.next(), FilterMode::Idle);
        assert_eq!(FilterMode::Idle.next(), FilterMode::Working);
        // 各档只接受自己的官方状态。
        assert!(FilterMode::Working.accepts("working"));
        assert!(!FilterMode::Working.accepts("done"));
        assert!(FilterMode::Done.accepts("done"));
        assert!(FilterMode::Idle.accepts("idle"));
        // 非官方值与本版不展示的状态一律不命中。
        for raw in ["blocked", "unknown", "running", ""] {
            assert!(!FilterMode::Working.accepts(raw));
            assert!(!FilterMode::Done.accepts(raw));
            assert!(!FilterMode::Idle.accepts(raw));
        }
        // 文案。
        assert_eq!(FilterMode::Working.label(), "Working");
        assert_eq!(FilterMode::Done.label_lower(), "done");
    }

    #[test]
    fn cwd_label_basenames_and_tilde_home() {
        std::env::set_var("HOME", "/Users/ivan");
        assert_eq!(cwd_label("/Users/ivan/Desktop/ERP1"), "ERP1");
        assert_eq!(cwd_label("/Users/ivan"), "~");
        assert_eq!(cwd_label("/Users/ivan/"), "~");
        assert_eq!(cwd_label("/var/log"), "log");
        assert_eq!(cwd_label("/"), "");
        // 空串不 panic
        assert_eq!(cwd_label(""), "");
    }
}
