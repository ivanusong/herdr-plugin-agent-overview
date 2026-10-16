//! 按键 → 动作的可测决策层。
//!
//! 关键约定：`handle_key` 在判断前先执行 `app.sync_selection(shared)`，
//! 把「选中映射」从渲染帧节奏中解耦——首轮刷新写入卡片后、下一帧绘制前
//! 的窗口内（约 ≤50ms）到达的按键，也能拿到正确的选中卡片（此前 Enter
//! 会因 `selected == None` 被静默吞掉，导航键同理）。

use ratatui::crossterm::event::KeyCode;

use crate::state::Shared;
use crate::ui::App;
use std::time::{Duration, Instant};

/// 双击判定窗口。
pub const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(500);

/// 按键处理结果：None = 按键已消费（导航/无操作），其余为退出/状态请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Cancel,
    OpenPane {
        pane_id: String,
    },
    /// `f`：循环状态筛选档位（调用方负责改 shared.filter 并唤醒刷新）。
    CycleFilter,
}

pub fn handle_key(app: &mut App, shared: &Shared, code: KeyCode) -> Action {
    // 任何按键处理前先把选中映射到最新卡片集合（按 terminal_id 保持，
    // 选中消失时就近回退）。这是渲染无关的正确性步骤。
    app.sync_selection(shared);
    match code {
        KeyCode::Esc | KeyCode::Char('q') => Action::Cancel,
        // 状态筛选：Working → Done → Idle → Working。
        KeyCode::Char('f') => Action::CycleFilter,
        KeyCode::Left | KeyCode::Char('h') => {
            app.move_horizontal(shared, -1);
            Action::None
        }
        KeyCode::Right | KeyCode::Char('l') => {
            app.move_horizontal(shared, 1);
            Action::None
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.move_vertical(shared, -1);
            Action::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.move_vertical(shared, 1);
            Action::None
        }
        KeyCode::Tab => {
            app.move_flat(shared, 1);
            Action::None
        }
        KeyCode::BackTab => {
            app.move_flat(shared, -1);
            Action::None
        }
        KeyCode::Enter => match app.selected_pane_id(shared) {
            Some(pane_id) => Action::OpenPane { pane_id },
            // 无卡片（No active agents）：Enter 不退出，q/Esc 仍是退出键。
            None => Action::None,
        },
        _ => Action::None,
    }
}

/// 单击/双击跟踪器。时间由调用方注入，便于测试。
#[derive(Default)]
pub struct ClickTracker {
    last: Option<(Instant, (usize, usize))>,
}

impl ClickTracker {
    /// 记录一次点击；若与上一次为同一卡片且在窗口内，返回 true（双击）。
    pub fn register(&mut self, now: Instant, tile: (usize, usize)) -> bool {
        let chained = self.last.is_some_and(|(at, target)| {
            target == tile && now.duration_since(at) <= DOUBLE_CLICK_INTERVAL
        });
        self.last = Some((now, tile));
        chained
    }
}

/// 鼠标左键点击处理：单击选中；同一卡在窗口内再击视为双击 → 跳转。
pub fn handle_click(
    app: &mut App,
    shared: &Shared,
    tile: (usize, usize),
    tracker: &mut ClickTracker,
    now: Instant,
) -> Action {
    let chained = tracker.register(now, tile);
    app.select(shared, tile);
    if chained {
        match app.selected_pane_id(shared) {
            Some(pane_id) => Action::OpenPane { pane_id },
            None => Action::None,
        }
    } else {
        Action::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Card, Section, Shared, Status};

    fn mk_card(terminal_id: &str, pane_id: &str) -> Card {
        Card {
            terminal_id: terminal_id.into(),
            pane_id: pane_id.into(),
            title: terminal_id.into(),
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
        }
    }

    fn shared_with(cards: Vec<Card>) -> Shared {
        Shared {
            sections: if cards.is_empty() {
                Vec::new()
            } else {
                vec![Section {
                    workspace_id: "w1".into(),
                    label: "w".into(),
                    number: 1,
                    cards,
                }]
            },
            connected: true,
            last_error: None,
            last_refresh: None,
            filter: crate::state::FilterMode::default(),
        }
    }

    #[test]
    fn enter_after_startup_race_opens_first_card() {
        // 直击本次缺陷：App 尚未渲染过任何一帧（selected 未初始化），
        // 首轮刷新已写入卡片，此时按 Enter 必须跳到默认第一张卡。
        let mut app = App::default();
        assert_eq!(app.selected, None);
        let shared = shared_with(vec![
            mk_card("term_1", "w5H:p2"),
            mk_card("term_2", "w5H:p3"),
        ]);
        let action = handle_key(&mut app, &shared, KeyCode::Enter);
        assert_eq!(
            action,
            Action::OpenPane {
                pane_id: "w5H:p2".into()
            }
        );
    }

    #[test]
    fn enter_with_no_cards_does_not_exit() {
        let mut app = App::default();
        let shared = Shared::default();
        assert_eq!(handle_key(&mut app, &shared, KeyCode::Enter), Action::None);
        // 无卡时 Esc 仍是退出键。
        assert_eq!(handle_key(&mut app, &shared, KeyCode::Esc), Action::Cancel);
    }

    #[test]
    fn f_cycles_filter_action() {
        let mut app = App::default();
        let shared = shared_with(vec![mk_card("t1", "w1:p1")]);
        assert_eq!(
            handle_key(&mut app, &shared, KeyCode::Char('f')),
            Action::CycleFilter
        );
        // 不退出、不改选中。
        assert!(app.selected.is_some() || app.selected_terminal.is_some());
        assert_eq!(
            handle_key(&mut app, &shared, KeyCode::Char('f')),
            Action::CycleFilter
        );
    }

    #[test]
    fn q_and_esc_cancel() {
        let mut app = App::default();
        let shared = shared_with(vec![mk_card("t1", "w1:p1")]);
        assert_eq!(
            handle_key(&mut app, &shared, KeyCode::Char('q')),
            Action::Cancel
        );
        assert_eq!(handle_key(&mut app, &shared, KeyCode::Esc), Action::Cancel);
    }

    #[test]
    fn enter_uses_current_selection_after_navigation() {
        let mut app = App::default();
        let shared = shared_with(vec![mk_card("term_1", "w1:p1"), mk_card("term_2", "w1:p2")]);
        // 先 Tab 到第二张，再 Enter：应跳第二张对应的 pane。
        assert_eq!(handle_key(&mut app, &shared, KeyCode::Tab), Action::None);
        assert_eq!(
            handle_key(&mut app, &shared, KeyCode::Enter),
            Action::OpenPane {
                pane_id: "w1:p2".into()
            }
        );
    }

    #[test]
    fn selection_keeps_terminal_id_across_rebuild() {
        // 选中 term_2 后卡片集合重建（term_2 消失）：Enter 回退到合法卡片，
        // 不吞键、不越界。
        let mut app = App::default();
        let shared = shared_with(vec![mk_card("term_1", "w1:p1"), mk_card("term_2", "w1:p2")]);
        handle_key(&mut app, &shared, KeyCode::Tab);
        assert_eq!(app.selected_terminal.as_deref(), Some("term_2"));

        let rebuilt = shared_with(vec![mk_card("term_1", "w1:p1")]);
        let action = handle_key(&mut app, &rebuilt, KeyCode::Enter);
        assert_eq!(
            action,
            Action::OpenPane {
                pane_id: "w1:p1".into()
            }
        );
    }

    #[test]
    fn click_tracker_double_click_same_tile_within_window() {
        let mut tracker = ClickTracker::default();
        let t0 = Instant::now();
        assert!(!tracker.register(t0, (0, 0)));
        // 窗口边界内（含恰好等于窗口）再击同一张：双击成立。
        assert!(tracker.register(t0 + DOUBLE_CLICK_INTERVAL, (0, 0)));
    }

    #[test]
    fn click_tracker_rejects_other_tile_and_stale_click() {
        let mut tracker = ClickTracker::default();
        let t0 = Instant::now();
        assert!(!tracker.register(t0, (0, 0)));
        // 换卡片：不算双击。
        assert!(!tracker.register(t0 + Duration::from_millis(50), (0, 1)));
        // 超出窗口：不算双击。
        let late =
            t0 + Duration::from_millis(50) + DOUBLE_CLICK_INTERVAL + Duration::from_millis(1);
        assert!(!tracker.register(late, (0, 1)));
    }

    #[test]
    fn handle_click_single_selects_double_opens() {
        let mut app = App::default();
        let shared = shared_with(vec![mk_card("term_1", "w1:p1"), mk_card("term_2", "w1:p2")]);
        let mut tracker = ClickTracker::default();
        let t0 = Instant::now();

        // 单击第一张：只选中，不退出。
        assert_eq!(
            handle_click(&mut app, &shared, (0, 0), &mut tracker, t0),
            Action::None
        );
        assert_eq!(app.selected_terminal.as_deref(), Some("term_1"));

        // 窗口内再击同一张：双击跳转。
        assert_eq!(
            handle_click(
                &mut app,
                &shared,
                (0, 0),
                &mut tracker,
                t0 + Duration::from_millis(100)
            ),
            Action::OpenPane {
                pane_id: "w1:p1".into()
            }
        );

        // 双击另一张的第一击：仍只是选中。
        assert_eq!(
            handle_click(
                &mut app,
                &shared,
                (0, 1),
                &mut tracker,
                t0 + Duration::from_millis(200)
            ),
            Action::None
        );
        assert_eq!(app.selected_terminal.as_deref(), Some("term_2"));
    }
}
