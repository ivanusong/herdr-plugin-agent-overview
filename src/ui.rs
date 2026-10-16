//! 全屏网格布局与渲染。
//!
//! 布局/滚动/鼠标交互派生自 herdr-mission-control（MIT，Copyright (c) 2026
//! Valère Jeantet），按 Agent 卡片需求改造：
//! - 分区 = workspace（分区头显示 workspace 标签与 Agent 数）。
//! - 卡片标题：状态图标+状态词（官方五态）+ Agent 名 + 类型 + 会话焦点标记。
//! - 卡片首行信息条：tab · cwd · pane_id（或最近读取错误）。
//! - 移除了上游 Backspace 关闭 pane / 确认弹窗等破坏性功能；本插件只读，
//!   唯一的写操作是用户明确 Enter/双击后的 pane.focus（由 main.rs 执行）。

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::state::{Card, FilterMode, Section, Shared, Status};

/// 卡片最小尺寸（含边框）：低于此值不再收缩，转为滚动。
const MIN_TILE_W: u16 = 52;
const MIN_TILE_H: u16 = 12;

/// 官方状态 → (图标, 颜色)，语义与 herdr 自身 UI 一致。
/// 官方状态 → (图标, 颜色)。颜色按 PRD 推荐：Working=Green、Blocked=Yellow、
/// Done=Blue、Idle/Unknown=Gray（仅这五种官方状态）。
fn status_style(status: Status) -> (&'static str, Color) {
    match status {
        Status::Blocked => ("●", Color::Yellow),
        Status::Working => ("●", Color::Green),
        Status::Done => ("●", Color::Blue),
        Status::Idle => ("○", Color::Gray),
        Status::Unknown => ("○", Color::Gray),
    }
}

/// UI 线程持有的交互状态。选中以 terminal_id（稳定 id）为准，
/// 4Hz 刷新重建卡片后选中保持不变。
pub struct App {
    /// 选中的卡片（稳定 terminal id）。
    pub selected_terminal: Option<String>,
    /// 派生值：每帧从 shared 重新映射。
    pub selected: Option<(usize, usize)>,
    /// 扁平游标：选中卡片消失时用它就近回退。
    pub flat_cursor: usize,
    /// 持久滚动偏移（虚拟行）。
    pub scroll: u16,
    /// 视口跟随选中。滚轮滚动时暂时关闭，键盘移动选中后恢复。
    pub follow_selection: bool,
    /// 最近一帧可见卡片的屏幕矩形（鼠标命中测试）。
    pub tile_rects: Vec<(Rect, (usize, usize))>,
    /// 视觉行 → 卡片下标（网格导航用），每帧由 plan 重建。
    pub nav_rows: Vec<Vec<(usize, usize)>>,
    /// (section, card) → (视觉行, 列)。
    pub nav_pos: HashMap<(usize, usize), (usize, usize)>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            selected_terminal: None,
            selected: None,
            flat_cursor: 0,
            scroll: 0,
            follow_selection: true,
            tile_rects: Vec::new(),
            nav_rows: Vec::new(),
            nav_pos: HashMap::new(),
        }
    }
}

impl App {
    /// 每帧把 shared 里的卡片映射回选中位置（按 terminal_id 保持选中）。
    pub fn sync_selection(&mut self, shared: &Shared) {
        let total = shared.total_cards();
        if total == 0 {
            self.selected = None;
            self.selected_terminal = None;
            return;
        }
        if let Some(tid) = self.selected_terminal.clone() {
            if let Some(pos) = shared.find(&tid) {
                self.selected = Some(pos);
                self.flat_cursor = flat_index_of(shared, pos).unwrap_or(self.flat_cursor);
                return;
            }
        }
        // 选中卡片消失（pane 关闭/agent 退出）：就近回退到原扁平位置。
        let idx = self.flat_cursor.min(total - 1);
        if let Some((si, ci, _card)) = shared.card_flat(idx) {
            self.selected = Some((si, ci));
            self.selected_terminal = shared.card_at((si, ci)).map(|c| c.terminal_id.clone());
            self.flat_cursor = idx;
        }
    }

    /// 选中指定卡片并登记 terminal id。
    pub fn select(&mut self, shared: &Shared, pos: (usize, usize)) {
        if let Some(card) = shared.card_at(pos) {
            self.selected = Some(pos);
            self.selected_terminal = Some(card.terminal_id.clone());
            self.flat_cursor = flat_index_of(shared, pos).unwrap_or(self.flat_cursor);
            self.follow_selection = true;
        }
    }

    /// 左右移动：同一视觉行内循环。
    pub fn move_horizontal(&mut self, shared: &Shared, delta: isize) {
        let Some(pos) = self.selected else { return };
        let Some(&(row, col)) = self.nav_pos.get(&pos) else {
            return;
        };
        let Some(row_cards) = self.nav_rows.get(row) else {
            return;
        };
        if row_cards.is_empty() {
            return;
        }
        let len = row_cards.len() as isize;
        let new_col = ((col as isize + delta) % len + len) % len;
        self.select(shared, row_cards[new_col as usize]);
    }

    /// 上下移动：跨视觉行，列就近对齐。
    pub fn move_vertical(&mut self, shared: &Shared, delta: isize) {
        let Some(pos) = self.selected else { return };
        let Some(&(row, col)) = self.nav_pos.get(&pos) else {
            return;
        };
        let rows = self.nav_rows.len() as isize;
        if rows == 0 {
            return;
        }
        let new_row = (row as isize + delta).clamp(0, rows - 1) as usize;
        let row_cards = &self.nav_rows[new_row];
        // 目标行列数更少时取行尾（保持空间感）。
        let new_col = col.min(row_cards.len().saturating_sub(1));
        if let Some(&target) = row_cards.get(new_col) {
            self.select(shared, target);
        }
    }

    /// Tab / Shift+Tab：扁平顺序环游全部卡片（跨分区）。
    pub fn move_flat(&mut self, shared: &Shared, delta: isize) {
        let total = shared.total_cards();
        if total == 0 {
            return;
        }
        let idx = ((self.flat_cursor as isize + delta) % total as isize + total as isize)
            % total as isize;
        let idx = idx as usize;
        if let Some((si, ci, _card)) = shared.card_flat(idx) {
            self.select(shared, (si, ci));
        }
    }

    /// 当前选中卡片的 pane_id（Enter 跳转用）。
    pub fn selected_pane_id(&self, shared: &Shared) -> Option<String> {
        self.selected
            .and_then(|pos| shared.card_at(pos))
            .map(|c| c.pane_id.clone())
    }
}

fn flat_index_of(shared: &Shared, pos: (usize, usize)) -> Option<usize> {
    let mut count = 0;
    for (si, section) in shared.sections.iter().enumerate() {
        if si == pos.0 {
            return (pos.1 < section.cards.len()).then_some(count + pos.1);
        }
        count += section.cards.len();
    }
    None
}

// ---------- 布局规划 ----------

enum RowKind {
    Header(usize),
    Tiles {
        section: usize,
        start: usize,
        end: usize,
        columns: usize,
    },
}

struct VisualRow {
    y: u16,
    height: u16,
    kind: RowKind,
}

struct Plan {
    rows: Vec<VisualRow>,
    total_height: u16,
    scrolling: bool,
}

/// 先收缩后滚动：卡片均分可用空间；低于下限后保留下限尺寸、进入滚动。
fn plan_rows(sections: &[Section], body: Rect) -> Plan {
    let width = body.width.max(1);
    let mut columns_per_section = Vec::with_capacity(sections.len());
    let mut tile_row_count: u16 = 0;
    for section in sections {
        let columns = ((width / MIN_TILE_W).max(1) as usize).min(section.cards.len().max(1));
        columns_per_section.push(columns);
        tile_row_count += section.cards.len().div_ceil(columns) as u16;
    }

    let header_count = sections.len() as u16;
    let avail = body.height.saturating_sub(header_count);
    let fit_h = (avail / tile_row_count.max(1)).max(1);

    let (tile_h, mut extra, scrolling) = if fit_h >= MIN_TILE_H {
        (fit_h, avail % tile_row_count.max(1), false)
    } else {
        (MIN_TILE_H, 0, true)
    };

    let mut rows = Vec::new();
    let mut y: u16 = 0;
    for (section_index, section) in sections.iter().enumerate() {
        rows.push(VisualRow {
            y,
            height: 1,
            kind: RowKind::Header(section_index),
        });
        y += 1;
        let columns = columns_per_section[section_index];
        let mut start = 0;
        while start < section.cards.len() {
            let end = (start + columns).min(section.cards.len());
            let height = tile_h + u16::from(extra > 0);
            extra = extra.saturating_sub(1);
            rows.push(VisualRow {
                y,
                height,
                kind: RowKind::Tiles {
                    section: section_index,
                    start,
                    end,
                    columns,
                },
            });
            y += height;
            start = end;
        }
    }

    Plan {
        rows,
        total_height: y,
        scrolling,
    }
}

/// 滚动跟随：仅当选中卡片行（连同分区头）会离开视口时才移动偏移。
fn ensure_selected_visible(app: &App, plan: &Plan, viewport: u16) -> u16 {
    if plan.total_height <= viewport {
        return 0;
    }
    let mut offset = app.scroll.min(plan.total_height - viewport);
    if !app.follow_selection {
        return offset;
    }
    let Some(pos) = app.selected else {
        return offset;
    };
    let selected = plan.rows.iter().position(|row| {
        matches!(
            row.kind,
            RowKind::Tiles { section, start, end, .. }
                if section == pos.0 && (start..end).contains(&pos.1)
        )
    });
    if let Some(index) = selected {
        let row = &plan.rows[index];
        let top = match index.checked_sub(1).map(|i| &plan.rows[i]) {
            Some(prev) if matches!(prev.kind, RowKind::Header(s) if s == pos.0) => prev.y,
            _ => row.y,
        };
        let bottom = row.y + row.height;
        if bottom > offset + viewport {
            offset = bottom - viewport;
        }
        if top < offset {
            offset = top;
        }
    }
    offset
}

// ---------- 渲染 ----------

pub fn draw(frame: &mut Frame, app: &mut App, shared: &Shared) {
    app.sync_selection(shared);
    app.tile_rects.clear();
    app.nav_rows.clear();
    app.nav_pos.clear();

    let area = frame.area();
    let show_banner = !shared.connected || shared.last_error.is_some();
    let [banner, body, footer] = Layout::vertical([
        Constraint::Length(u16::from(show_banner)),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);

    if show_banner {
        let msg = shared
            .last_error
            .clone()
            .unwrap_or_else(|| "herdr socket 不可用".to_string());
        let line = Line::from(Span::styled(
            format!(" ⚠ {msg} · 自动恢复中 "),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(Paragraph::new(line), banner);
    }

    let sections = &shared.sections;
    if sections.is_empty() && shared.connected {
        draw_empty(frame, body, shared.filter);
        draw_footer(frame, footer, shared, false);
        return;
    }

    let plan = plan_rows(sections, body);
    let offset = ensure_selected_visible(app, &plan, body.height);
    app.scroll = offset;
    // 行先画到虚拟画布，再整体 blit 到视口：半可见卡片逐行滚动而非整卡弹出。
    let mut scratch = Buffer::empty(Rect {
        x: 0,
        y: 0,
        width: body.width,
        height: plan.total_height.max(1),
    });

    for row in &plan.rows {
        if row.y + row.height <= offset || row.y >= offset + body.height {
            continue;
        }
        let area = Rect {
            x: 0,
            y: row.y,
            width: body.width,
            height: row.height,
        };
        match row.kind {
            RowKind::Header(section_index) => {
                draw_header(sections, section_index, area, &mut scratch)
            }
            RowKind::Tiles {
                section,
                start,
                end,
                columns,
            } => {
                let cells = Layout::horizontal(vec![Constraint::Fill(1); columns]).split(area);
                let mut nav_row = Vec::with_capacity(end - start);
                for (offset_in_row, card_index) in (start..end).enumerate() {
                    let cell = cells[offset_in_row];
                    let pos = (section, card_index);
                    nav_row.push(pos);
                    app.nav_pos.insert(pos, (app.nav_rows.len(), offset_in_row));
                    // 可见部分换算成屏幕坐标，供鼠标命中测试。
                    let top = cell.y.max(offset);
                    let bottom = (cell.y + cell.height).min(offset + body.height);
                    if bottom > top {
                        app.tile_rects.push((
                            Rect {
                                x: body.x + cell.x,
                                y: body.y + top - offset,
                                width: cell.width,
                                height: bottom - top,
                            },
                            pos,
                        ));
                    }
                    let card = &sections[section].cards[card_index];
                    let selected = app.selected == Some(pos);
                    draw_tile(card, selected, cell, &mut scratch);
                }
                app.nav_rows.push(nav_row);
            }
        }
    }

    let frame_buf = frame.buffer_mut();
    for dy in 0..body.height {
        let sy = offset + dy;
        if sy >= plan.total_height {
            break;
        }
        for dx in 0..body.width {
            if let (Some(dst), Some(src)) = (
                frame_buf.cell_mut((body.x + dx, body.y + dy)),
                scratch.cell((dx, sy)),
            ) {
                *dst = src.clone();
            }
        }
    }

    draw_footer(frame, footer, shared, plan.scrolling);
}

fn draw_empty(frame: &mut Frame, area: Rect, filter: FilterMode) {
    // 空状态文案按当前筛选档位（`No working agents` / `No done agents` / …）。
    let lines = vec![
        Line::from(Span::styled(
            format!("No {} agents", filter.label_lower()),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("{} agents appear here automatically.", filter.label()),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Press Esc to close.",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let top = area.y + area.height.saturating_sub(lines.len() as u16) / 2;
    for (offset, line) in lines.into_iter().enumerate() {
        let width = (line.width() as u16).min(area.width);
        let x = area.x + area.width.saturating_sub(width) / 2;
        frame.render_widget(
            Paragraph::new(line),
            Rect {
                x,
                y: top + offset as u16,
                width,
                height: 1,
            },
        );
    }
}

fn draw_footer(frame: &mut Frame, area: Rect, shared: &Shared, scrolling: bool) {
    let total = shared.total_cards();
    let mut left = vec![
        Span::styled(" ←↓↑→/hjkl/Tab ", Style::default().fg(Color::Cyan)),
        Span::raw("navigate  "),
        Span::styled("f", Style::default().fg(Color::Cyan)),
        Span::raw(" filter  "),
        Span::styled("⏎", Style::default().fg(Color::Cyan)),
        Span::raw(" open  "),
        Span::styled("click", Style::default().fg(Color::Cyan)),
        Span::raw("/dbl-click open  "),
        Span::styled("esc", Style::default().fg(Color::Cyan)),
        Span::raw(" quit"),
    ];
    if scrolling {
        left.push(Span::styled(
            "  ‹scrolling›",
            Style::default().fg(Color::DarkGray),
        ));
    }
    let right = if shared.connected {
        Span::styled(
            format!("Filter: {} · ⟳ {total} · 4Hz ", shared.filter.label()),
            Style::default().fg(Color::DarkGray),
        )
    } else {
        Span::styled("⚠ reconnecting… ", Style::default().fg(Color::Red))
    };
    // 左侧帮助过长时给右侧让位。
    let right_w = right.width() as u16;
    let avail = area.width.saturating_sub(right_w);
    let mut left_line = Line::from(left);
    if left_line.width() as u16 > avail {
        // 窄终端也要保留筛切换提示。
        left_line = Line::from(vec![
            Span::styled(" ←↓↑→ ", Style::default().fg(Color::Cyan)),
            Span::styled("f", Style::default().fg(Color::Cyan)),
            Span::raw(" filter  "),
            Span::styled("⏎", Style::default().fg(Color::Cyan)),
            Span::raw(" open  "),
            Span::styled("esc", Style::default().fg(Color::Cyan)),
            Span::raw(" quit"),
        ]);
    }
    frame.render_widget(Paragraph::new(left_line), area);
    if right_w <= area.width {
        let right_area = Rect {
            x: area.x + area.width - right_w,
            y: area.y,
            width: right_w,
            height: 1,
        };
        frame.render_widget(Paragraph::new(Line::from(vec![right])), right_area);
    }
}

fn draw_header(sections: &[Section], index: usize, area: Rect, buf: &mut Buffer) {
    let section = &sections[index];
    let title = Line::from(vec![
        Span::styled(
            format!(" w{} ", section.number),
            Style::default()
                .fg(Color::Black)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} ", section.label),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("· {} agents", section.card_count()),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    Paragraph::new(title).render(area, buf);
}

fn draw_tile(card: &Card, selected: bool, area: Rect, buf: &mut Buffer) {
    let (icon, color) = status_style(card.status);
    let border_style = if selected {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    // 标题条：状态图标+状态词 · Agent 名 · [类型] · 焦点标记。
    let mut title_spans = vec![
        Span::styled(format!(" {icon} "), Style::default().fg(color)),
        Span::styled(card.status.label(), Style::default().fg(color)),
        Span::raw("  "),
        Span::styled(
            card.title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(kind) = &card.agent_kind {
        title_spans.push(Span::styled(
            format!(" [{kind}]"),
            Style::default().fg(Color::Magenta),
        ));
    }
    if card.focused {
        title_spans.push(Span::styled(" ◈", Style::default().fg(Color::Cyan)));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(title_spans));

    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // 信息条：tab · cwd · pane_id；读取失败时显示红色错误（截断到一行）。
    let info = if let Some(err) = &card.read_error {
        format!("⚠ {err}")
    } else {
        format!("{} · {} · {}", card.tab_label, card.cwd_label, card.pane_id)
    };
    let info_style = if card.read_error.is_some() {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let info_w = inner.width.saturating_sub(1) as usize;
    let info_line = Line::from(Span::styled(
        truncate_width(&info, info_w as f64),
        info_style,
    ));
    Paragraph::new(info_line).render(
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
        buf,
    );

    // 预览：取可视区尾部（终端底部才是“正在发生”的地方）。
    // 先去掉尾部空行，再截取最后 preview_h 行，避免内容在上半屏的 pane 画成空白。
    if inner.height > 1 {
        let preview_area = Rect {
            x: inner.x,
            y: inner.y + 1,
            width: inner.width,
            height: inner.height - 1,
        };
        let end = card
            .preview
            .iter()
            .rposition(|line| line.spans.iter().any(|s| !s.content.trim().is_empty()))
            .map_or(0, |i| i + 1);
        let tail_start = end.saturating_sub(preview_area.height as usize);
        let text: Vec<Line> = card.preview[tail_start..end].to_vec();
        Paragraph::new(text).render(preview_area, buf);
    }
}

/// 按显示宽度截断（超出补 …）。中文/emoji 等宽字符按 unicode-width 计算。
fn truncate_width(text: &str, max_width: f64) -> String {
    let max = max_width as usize;
    if text.width() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn truncate_respects_display_width() {
        assert_eq!(truncate_width("hello", 10.0), "hello");
        assert_eq!(truncate_width("hello world", 8.0), "hello w…");
        // 中文宽 2：预算 4 列时，放 1 个汉字 + …（宽 3），放不下第二个汉字。
        assert_eq!(truncate_width("中文中文", 4.0), "中…");
        assert_eq!(truncate_width("中文中文", 5.0), "中文…");
        assert_eq!(truncate_width("", 4.0), "");
    }

    #[test]
    fn status_colors_match_prd() {
        // 锁定 PRD 推荐的颜色映射，防止回归。
        assert_eq!(status_style(Status::Working), ("●", Color::Green));
        assert_eq!(status_style(Status::Blocked), ("●", Color::Yellow));
        assert_eq!(status_style(Status::Done), ("●", Color::Blue));
        assert_eq!(status_style(Status::Idle), ("○", Color::Gray));
        assert_eq!(status_style(Status::Unknown), ("○", Color::Gray));
    }

    /// 提取 TestBackend 渲染结果的每一行文本。
    fn buffer_rows(backend: &TestBackend) -> Vec<String> {
        let buffer = backend.buffer();
        let width = buffer.area.width as usize;
        let mut rows = vec![String::new(); buffer.area.height as usize];
        for (index, cell) in buffer.content().iter().enumerate() {
            rows[index / width].push_str(cell.symbol());
        }
        rows
    }

    #[test]
    fn empty_state_copy_matches_filter_mode() {
        // 三档空状态文案：`No <state> agents`；旧文案不得再出现。
        for (filter, first, second) in [
            (
                FilterMode::Working,
                "No working agents",
                "Working agents appear here automatically.",
            ),
            (
                FilterMode::Done,
                "No done agents",
                "Done agents appear here automatically.",
            ),
            (
                FilterMode::Idle,
                "No idle agents",
                "Idle agents appear here automatically.",
            ),
        ] {
            let backend = TestBackend::new(80, 24);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| draw_empty(frame, frame.area(), filter))
                .unwrap();
            let rows = buffer_rows(terminal.backend());
            let non_empty: Vec<String> = rows
                .iter()
                .map(|row| row.trim().to_string())
                .filter(|row| !row.is_empty())
                .collect();
            assert_eq!(
                non_empty,
                vec![
                    first.to_string(),
                    second.to_string(),
                    "Press Esc to close.".to_string(),
                ],
                "{filter:?} 档空状态文案"
            );
            let all = rows.join("\n");
            assert!(!all.contains("No active agents"));
            assert!(!all.contains("Start an agent in Herdr"));
            assert!(!all.contains("esc 关闭"));
            assert!(!all.contains("检测到 agent"));
        }
    }

    #[test]
    fn footer_shows_current_filter() {
        let shared = layout_test_shared(vec![Section {
            workspace_id: "w1".into(),
            label: "one".into(),
            number: 1,
            cards: vec![layout_test_card("t1")],
        }]);
        let backend = TestBackend::new(120, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::default();
        for (filter, expected) in [
            (FilterMode::Working, "Filter: Working"),
            (FilterMode::Done, "Filter: Done"),
            (FilterMode::Idle, "Filter: Idle"),
        ] {
            let mut s = Shared {
                filter,
                ..layout_test_shared(shared.sections.clone())
            };
            s.filter = filter;
            terminal.draw(|frame| draw(frame, &mut app, &s)).unwrap();
            let rows = buffer_rows(terminal.backend());
            let footer = rows.last().unwrap();
            assert!(footer.contains(expected), "footer: {footer:?}");
            assert!(footer.contains('f'), "footer 应提示 f 键: {footer:?}");
        }
    }

    fn layout_test_card(tid: &str) -> Card {
        Card {
            terminal_id: tid.into(),
            pane_id: format!("w1:{tid}"),
            title: tid.into(),
            agent_kind: None,
            status: Status::Working,
            workspace_id: "w1".into(),
            workspace_label: "w".into(),
            workspace_number: 1,
            tab_id: "w1:t1".into(),
            tab_label: "1".into(),
            cwd_label: "-".into(),
            focused: false,
            preview: Vec::new(),
            read_error: None,
        }
    }

    fn layout_test_shared(sections: Vec<Section>) -> Shared {
        Shared {
            sections,
            connected: true,
            last_error: None,
            last_refresh: None,
            filter: FilterMode::Working,
        }
    }

    /// 120x40 终端：footer 1 行 → body 39 行。
    /// 构造 2 个 section（2 headers）+ 5 卡（2 列 → 3 tile 行）：
    /// avail = 37、fit = 12、extra = 1 → total = 2 + 13 + 12 + 12 = 39 == body。
    fn exact_fit_sections() -> Vec<Section> {
        vec![
            Section {
                workspace_id: "w1".into(),
                label: "one".into(),
                number: 1,
                cards: vec![layout_test_card("t1"), layout_test_card("t2")],
            },
            Section {
                workspace_id: "w2".into(),
                label: "two".into(),
                number: 2,
                cards: vec![
                    layout_test_card("t3"),
                    layout_test_card("t4"),
                    layout_test_card("t5"),
                ],
            },
        ]
    }

    #[test]
    fn plan_exact_fit_equals_viewport_height() {
        let sections = exact_fit_sections();
        let body = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 39,
        };
        let plan = plan_rows(&sections, body);
        assert!(!plan.scrolling);
        assert_eq!(plan.total_height, body.height, "恰满时 total 必须等于 body");
    }

    #[test]
    fn exact_fit_viewport_cannot_scroll_and_shows_bottom_border() {
        let shared = layout_test_shared(exact_fit_sections());
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::default();

        terminal
            .draw(|frame| draw(frame, &mut app, &shared))
            .unwrap();
        assert_eq!(app.scroll, 0);

        // 滚轮向下（模拟 main.rs 的 ScrollDown）：恰满时不能产生任何滚动。
        app.follow_selection = false;
        app.scroll = app.scroll.saturating_add(3);
        terminal
            .draw(|frame| draw(frame, &mut app, &shared))
            .unwrap();
        assert_eq!(app.scroll, 0, "恰满视口时滚轮不能下滚");

        // body 最后一行（y=38）必须显示卡片下边框，而不是空白。
        let rows = buffer_rows(terminal.backend());
        let last_body_row = rows[38].trim_end();
        assert!(
            last_body_row.contains('└') || last_body_row.contains('┘'),
            "body 最后一行应有卡片下边框，实际: {last_body_row:?}"
        );
        // footer 行仍是 footer（Filter 状态）。
        assert!(
            rows[39].contains("Filter: Working"),
            "footer: {:?}",
            rows[39]
        );
    }

    #[test]
    fn short_content_fills_viewport_and_cannot_scroll() {
        // 内容不足（1 卡）：plan 会把卡片行拉高填满 body，滚轮同样不能滚。
        let shared = layout_test_shared(vec![Section {
            workspace_id: "w1".into(),
            label: "one".into(),
            number: 1,
            cards: vec![layout_test_card("t1")],
        }]);
        let body = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 39,
        };
        let plan = plan_rows(&shared.sections, body);
        assert_eq!(plan.total_height, body.height);

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App {
            follow_selection: false,
            scroll: 25,
            ..App::default()
        };
        terminal
            .draw(|frame| draw(frame, &mut app, &shared))
            .unwrap();
        assert_eq!(app.scroll, 0, "内容不足时不得向底部滚动");
    }

    #[test]
    fn scrolling_mode_clamps_offset_to_last_content_row() {
        // 内容超过视口（卡片保 12 行下限）：total > body，滚动上限 = total - body，
        // 滚到底时最后一行仍是内容（不能滚出无内容空白）。
        let cards: Vec<Card> = (0..13)
            .map(|i| layout_test_card(&format!("t{i}")))
            .collect();
        let sections = vec![Section {
            workspace_id: "w1".into(),
            label: "one".into(),
            number: 1,
            cards,
        }];
        let body = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 39,
        };
        let plan = plan_rows(&sections, body);
        assert!(plan.scrolling);
        assert!(plan.total_height > body.height);
        assert_eq!(plan.total_height, 1 + 7 * MIN_TILE_H, "1 header + 7 行卡片");

        let shared = layout_test_shared(sections);
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        // 滚到底。
        let mut app = App {
            follow_selection: false,
            scroll: u16::MAX,
            ..App::default()
        };
        terminal
            .draw(|frame| draw(frame, &mut app, &shared))
            .unwrap();
        assert_eq!(app.scroll, plan.total_height - body.height);
        let rows = buffer_rows(terminal.backend());
        let last_body_row = rows[38].trim_end();
        assert!(
            last_body_row.contains('└') || last_body_row.contains('┘'),
            "滚到底时最后一行应是卡片下边框，实际: {last_body_row:?}"
        );
    }

    #[test]
    fn read_error_card_renders_warning_line() {
        // 读取失败的卡片：信息条变为红色 ⚠ 错误文本（如 w66 不可用 pane 的
        // pane_not_found），卡片本体保留旧预览，不影响断言以外的布局。
        use crate::state::Card;
        let card = Card {
            terminal_id: "term_w66".into(),
            pane_id: "w66:p1".into(),
            title: "cicd-deploy".into(),
            agent_kind: Some("pi".into()),
            status: Status::Unknown,
            workspace_id: "w66".into(),
            workspace_label: "cicd-deploy".into(),
            workspace_number: 7,
            tab_id: "w66:t1".into(),
            tab_label: "1".into(),
            cwd_label: "cicd-deploy".into(),
            focused: false,
            preview: crate::ansi::parse("old preview line\n"),
            read_error: Some("pane_not_found (pane w66:p1 not found)".into()),
        };
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_tile(&card, false, frame.area(), frame.buffer_mut()))
            .unwrap();
        let rows = buffer_rows(terminal.backend());
        let joined = rows.join("\n");
        assert!(joined.contains("cicd-deploy"), "标题仍在");
        assert!(
            joined.contains("⚠ pane_not_found"),
            "信息条应显示 ⚠ 与错误码，实际: {joined:?}"
        );
    }

    #[test]
    fn plan_shrinks_then_scrolls() {
        use crate::state::Card;
        let mk_card = |tid: &str| Card {
            terminal_id: tid.into(),
            pane_id: tid.into(),
            title: tid.into(),
            agent_kind: None,
            status: Status::Idle,
            workspace_id: "w1".into(),
            workspace_label: "w".into(),
            workspace_number: 1,
            tab_id: "w1:t1".into(),
            tab_label: "1".into(),
            cwd_label: "-".into(),
            focused: false,
            preview: Vec::new(),
            read_error: None,
        };
        let section = Section {
            workspace_id: "w1".into(),
            label: "w".into(),
            number: 1,
            cards: (0..6).map(|i| mk_card(&format!("p{i}"))).collect(),
        };
        let body = Rect {
            x: 0,
            y: 0,
            width: 200,
            height: 30,
        };
        let plan = plan_rows(std::slice::from_ref(&section), body);
        // 200 宽 → 3 列 → 2 个卡片行 + 1 header。
        assert_eq!(plan.rows.len(), 3);
        assert!(!plan.scrolling);

        // 高度不够：进入滚动，卡片保持下限尺寸。
        let small = Rect {
            x: 0,
            y: 0,
            width: 200,
            height: 10,
        };
        let plan = plan_rows(std::slice::from_ref(&section), small);
        assert!(plan.scrolling);
        let tiles_height: u16 = plan
            .rows
            .iter()
            .filter(|r| matches!(r.kind, RowKind::Tiles { .. }))
            .map(|r| r.height)
            .sum();
        assert_eq!(tiles_height, MIN_TILE_H * 2);
    }
}
