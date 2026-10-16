//! Agent 发现与映射：`agent.list` + `session.snapshot` → 卡片/分区。
//!
//! 规则（只用官方字段，不做文本猜测）：
//! 1. 以 `agent.list` 为准（herdr 只把 agent 终端列进去，普通 shell 天然不在）。
//! 2. 若 `agent.list` 失败，回落 `snapshot.agents`（同一数据源）。
//! 3. **按当前筛选档位展示**（默认 Working，`f` 循环）：只构建与档位匹配的
//!    官方状态卡片。过滤发生在卡片构建层，状态变化时卡片随下一次刷新
//!    自动增删，非当前档的 pane 不会被 pane.read（不浪费读取资源）。
//!    blocked / unknown 状态本版不展示。
//! 4. pane 已不在 snapshot 中 → 丢弃（pane 已关闭）。
//! 5. “agent 已退出但 pane 尚存”：若 AgentInfo 与 PaneInfo 的全部 agent 身份
//!    字段（agent / display_agent / name / agent_session / title）都为空且
//!    状态为 unknown → 视为无主终端，过滤。
//! 6. 排序：workspace.number → tab.number → snapshot 中 pane 顺序；分区按 workspace。

use crate::api::{AgentInfo, PaneInfo, Snapshot};
use crate::state::{cwd_label, Card, FilterMode, Section, Status};

/// 可见性规则：只按当前筛选档位接受官方状态。需求变更若需调整展示范围，
/// 只改这一处（含测试），不要散落到渲染层。
pub fn is_visible(agent_status: &str, filter: FilterMode) -> bool {
    filter.accepts(agent_status)
}

/// 从 snapshot + agent 列表构建当前筛选档位下的分区卡片。
pub fn build_sections(
    snapshot: &Snapshot,
    agents: &[AgentInfo],
    filter: FilterMode,
) -> Vec<Section> {
    let panes: std::collections::HashMap<&str, &PaneInfo> = snapshot
        .panes
        .iter()
        .map(|p| (p.pane_id.as_str(), p))
        .collect();
    let tabs: std::collections::HashMap<&str, &crate::api::TabInfo> = snapshot
        .tabs
        .iter()
        .map(|t| (t.tab_id.as_str(), t))
        .collect();
    let workspaces: std::collections::HashMap<&str, &crate::api::WorkspaceInfo> = snapshot
        .workspaces
        .iter()
        .map(|w| (w.workspace_id.as_str(), w))
        .collect();

    // pane 顺序：snapshot.panes 的数组顺序即 tab 内布局顺序。
    let pane_order: std::collections::HashMap<&str, usize> = snapshot
        .panes
        .iter()
        .enumerate()
        .map(|(i, p)| (p.pane_id.as_str(), i))
        .collect();

    let mut cards: Vec<Card> = agents
        .iter()
        .filter_map(|agent| {
            // 只保留当前档位的 Agent（其它状态不展示、也不读 pane）。
            if !is_visible(&agent.agent_status, filter) {
                return None;
            }
            let pane = panes.get(agent.pane_id.as_str())?;
            if has_agent_identity(agent) || has_pane_identity(pane) {
                Some(build_card(agent, pane, &tabs, &workspaces))
            } else {
                None
            }
        })
        .collect();

    cards.sort_by_key(|card| {
        let tab = tabs.get(card.tab_id.as_str());
        let ws_number = workspaces
            .get(card.workspace_id.as_str())
            .map(|w| w.number)
            .unwrap_or(usize::MAX);
        let tab_number = tab.map(|t| t.number).unwrap_or(usize::MAX);
        let order = pane_order
            .get(card.pane_id.as_str())
            .copied()
            .unwrap_or(usize::MAX);
        (ws_number, tab_number, order)
    });

    let mut sections: Vec<Section> = Vec::new();
    for card in cards {
        match sections
            .iter_mut()
            .find(|s| s.workspace_id == card.workspace_id)
        {
            Some(section) => section.cards.push(card),
            None => sections.push(Section {
                workspace_id: card.workspace_id.clone(),
                label: workspaces
                    .get(card.workspace_id.as_str())
                    .map(|w| w.label.clone())
                    .unwrap_or_default(),
                number: workspaces
                    .get(card.workspace_id.as_str())
                    .map(|w| w.number)
                    .unwrap_or(0),
                cards: vec![card],
            }),
        }
    }
    sections
}

fn has_agent_identity(agent: &AgentInfo) -> bool {
    non_empty(&agent.agent).is_some()
        || non_empty(&agent.display_agent).is_some()
        || non_empty(&agent.name).is_some()
        || non_empty(&agent.title).is_some()
        || agent.agent_session.is_some()
}

fn has_pane_identity(pane: &PaneInfo) -> bool {
    pane.agent.is_some()
        || pane.display_agent.is_some()
        || pane.agent_session.is_some()
        || (pane
            .terminal_title_stripped
            .as_deref()
            .is_some_and(|t| !t.is_empty())
            && pane.agent_status != "unknown")
}

fn non_empty(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

fn build_card(
    agent: &AgentInfo,
    pane: &PaneInfo,
    tabs: &std::collections::HashMap<&str, &crate::api::TabInfo>,
    workspaces: &std::collections::HashMap<&str, &crate::api::WorkspaceInfo>,
) -> Card {
    let title = non_empty(&agent.display_agent)
        .or_else(|| non_empty(&agent.name))
        .or_else(|| non_empty(&agent.agent))
        .or_else(|| non_empty(&pane.terminal_title_stripped))
        .unwrap_or_else(|| pane.pane_id.clone());
    let tab_label = tabs
        .get(agent.tab_id.as_str())
        .map(|t| {
            if t.label.is_empty() {
                format!("t{}", t.number)
            } else {
                t.label.clone()
            }
        })
        .unwrap_or_else(|| agent.tab_id.clone());
    let cwd = agent
        .foreground_cwd
        .as_deref()
        .filter(|c| !c.is_empty())
        .or(agent.cwd.as_deref())
        .or(pane.foreground_cwd.as_deref())
        .or(pane.cwd.as_deref())
        .unwrap_or("");
    Card {
        terminal_id: if agent.terminal_id.is_empty() {
            pane.terminal_id.clone()
        } else {
            agent.terminal_id.clone()
        },
        pane_id: agent.pane_id.clone(),
        title,
        agent_kind: agent.agent.clone(),
        status: Status::parse(&agent.agent_status),
        workspace_id: agent.workspace_id.clone(),
        workspace_label: workspaces
            .get(agent.workspace_id.as_str())
            .map(|w| w.label.clone())
            .unwrap_or_else(|| agent.workspace_id.clone()),
        workspace_number: workspaces
            .get(agent.workspace_id.as_str())
            .map(|w| w.number)
            .unwrap_or(0),
        tab_id: agent.tab_id.clone(),
        tab_label,
        cwd_label: if cwd.is_empty() {
            "-".to_string()
        } else {
            cwd_label(cwd)
        },
        focused: agent.focused || pane.focused,
        preview: Vec::new(),
        read_error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snap(v: serde_json::Value) -> Snapshot {
        serde_json::from_value(v).unwrap()
    }

    fn agents(v: serde_json::Value) -> Vec<AgentInfo> {
        serde_json::from_value(v).unwrap()
    }

    /// 两个 workspace、若干 agent / 普通 shell / 已退出 agent / 已关闭 pane。
    fn fixture() -> (Snapshot, Vec<AgentInfo>) {
        let snapshot = snap(json!({
            "focused_workspace_id": "w1",
            "workspaces": [
                {"workspace_id": "w1", "number": 1, "label": "Alpha", "focused": true},
                {"workspace_id": "w2", "number": 2, "label": "Beta", "focused": false}
            ],
            "tabs": [
                {"tab_id": "w1:t1", "workspace_id": "w1", "number": 1, "label": "1", "focused": true},
                {"tab_id": "w1:t2", "workspace_id": "w1", "number": 2, "label": "build", "focused": false},
                {"tab_id": "w2:t1", "workspace_id": "w2", "number": 1, "label": "1", "focused": false}
            ],
            "panes": [
                {"pane_id": "w1:p1", "terminal_id": "term_1", "tab_id": "w1:t1", "workspace_id": "w1",
                 "focused": true, "agent_status": "working", "agent": "pi",
                 "terminal_title_stripped": "π", "cwd": "/Users/ivan/proj/alpha"},
                {"pane_id": "w1:p2", "terminal_id": "term_2", "tab_id": "w1:t2", "workspace_id": "w1",
                 "focused": false, "agent_status": "unknown", "terminal_title_stripped": "~/shell"},
                {"pane_id": "w1:p3", "terminal_id": "term_3", "tab_id": "w1:t2", "workspace_id": "w1",
                 "focused": false, "agent_status": "unknown"},
                {"pane_id": "w2:p1", "terminal_id": "term_4", "tab_id": "w2:t1", "workspace_id": "w2",
                 "focused": false, "agent_status": "blocked", "agent": "claude",
                 "cwd": "/repo/beta"}
            ],
            "layouts": []
        }));
        let agents = agents(json!([
            // 正常 agent
            {"pane_id": "w1:p1", "terminal_id": "term_1", "agent": "pi", "name": "coder",
             "agent_status": "working", "workspace_id": "w1", "tab_id": "w1:t1",
             "focused": true, "cwd": "/Users/ivan/proj/alpha", "foreground_cwd": "/Users/ivan/proj/alpha/sub"},
            // 普通 shell：根本不出现在 agent.list
            // 已退出 agent：pane 尚存，身份字段全空
            {"pane_id": "w1:p3", "terminal_id": "term_3", "agent": null, "name": null,
             "agent_status": "unknown", "workspace_id": "w1", "tab_id": "w1:t2", "focused": false},
            // 另一个 workspace 的 blocked agent
            {"pane_id": "w2:p1", "terminal_id": "term_4", "agent": "claude",
             "agent_status": "blocked", "workspace_id": "w2", "tab_id": "w2:t1",
             "focused": false, "cwd": "/repo/beta"}
        ]));
        (snapshot, agents)
    }

    #[test]
    fn only_working_agents_are_visible() {
        let (snapshot, agents) = fixture();
        let sections = build_sections(&snapshot, &agents, FilterMode::Working);
        // 只有 w1:p1（working）显示；blocked/unknown 均被过滤，
        // 分区也只剩包含可见卡片的 workspace。
        let total: usize = sections.iter().map(|s| s.card_count()).sum();
        assert_eq!(total, 1);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].label, "Alpha");
        assert_eq!(sections[0].cards[0].pane_id, "w1:p1");
    }

    #[test]
    fn non_working_statuses_are_filtered_at_build_layer() {
        let (snapshot, agents) = fixture();
        for status in ["idle", "blocked", "done", "unknown"] {
            let mut agents = agents.clone();
            agents[0].agent_status = status.to_string();
            let sections = build_sections(&snapshot, &agents, FilterMode::Working);
            let total: usize = sections.iter().map(|s| s.card_count()).sum();
            // w2:p1 是 blocked，也不会显示；四种非 working 状态下总卡数为 0。
            assert_eq!(total, 0, "状态 {status} 不应展示卡片");
        }
    }

    #[test]
    fn status_change_adds_and_removes_cards() {
        let (snapshot, agents) = fixture();
        // working → idle：卡片移除。
        let mut idle = agents.clone();
        idle[0].agent_status = "idle".into();
        assert_eq!(
            build_sections(&snapshot, &idle, FilterMode::Working)
                .iter()
                .map(|s| s.card_count())
                .sum::<usize>(),
            0
        );
        // idle → working：卡片出现（同一个 pane）。
        let sections = build_sections(&snapshot, &agents, FilterMode::Working);
        assert_eq!(sections.iter().map(|s| s.card_count()).sum::<usize>(), 1);
        // blocked → working：另一个 pane 也能重新出现。
        let mut revived = agents.clone();
        revived[2].agent_status = "working".into();
        let sections = build_sections(&snapshot, &revived, FilterMode::Working);
        assert_eq!(sections.iter().map(|s| s.card_count()).sum::<usize>(), 2);
        assert!(sections.iter().any(|s| s.workspace_id == "w2"));
    }

    #[test]
    fn cards_map_fields_and_order_by_workspace_tab() {
        let (snapshot, agents) = fixture();
        let sections = build_sections(&snapshot, &agents, FilterMode::Working);
        let first = &sections[0].cards[0];
        assert_eq!(first.terminal_id, "term_1");
        assert_eq!(first.pane_id, "w1:p1");
        assert_eq!(first.title, "coder"); // name 优先于 agent 类型
        assert_eq!(first.agent_kind.as_deref(), Some("pi"));
        assert_eq!(first.status, Status::Working);
        assert_eq!(first.workspace_label, "Alpha");
        assert_eq!(first.cwd_label, "sub"); // foreground_cwd 优先
        assert_eq!(first.tab_label, "1"); // coder 在 t1
        assert!(first.focused);
    }

    #[test]
    fn pane_gone_from_snapshot_is_dropped() {
        let (snapshot, mut agents) = fixture();
        // working 卡的 pane 也不在 snapshot 中了 → 丢弃。
        agents[0].pane_id = "w9:p9".into();
        let sections = build_sections(&snapshot, &agents, FilterMode::Working);
        assert!(sections.is_empty());
    }

    #[test]
    fn exited_agent_with_session_ref_is_also_filtered() {
        // 非 working（含已退出但保留 agent_session 身份）一律不展示：
        // 过滤先于身份判定，避免“已退出但 pane 尚存”泄漏到网格。
        let (snapshot, mut agents) = fixture();
        agents[1].agent_session =
            Some(json!({"source": "herdr:pi", "agent": "pi", "kind": "id", "value": "s1"}));
        let sections = build_sections(&snapshot, &agents, FilterMode::Working);
        let total: usize = sections.iter().map(|s| s.card_count()).sum();
        assert_eq!(total, 1);
        assert_eq!(sections[0].cards[0].pane_id, "w1:p1");
    }

    #[test]
    fn is_visible_follows_filter_mode() {
        assert!(is_visible("working", FilterMode::Working));
        assert!(is_visible("done", FilterMode::Done));
        assert!(is_visible("idle", FilterMode::Idle));
        for status in ["idle", "blocked", "done", "unknown", "running", ""] {
            assert!(
                !is_visible(status, FilterMode::Working),
                "{status} 非 working 档不可见"
            );
        }
        assert!(!is_visible("working", FilterMode::Done));
        assert!(!is_visible("working", FilterMode::Idle));
    }

    #[test]
    fn filter_mode_selects_matching_cards() {
        let (snapshot, mut agents) = fixture();
        agents[0].agent_status = "done".into();
        agents[2].agent_status = "idle".into();
        // Done 档：只有 w1:p1。
        let done = build_sections(&snapshot, &agents, FilterMode::Done);
        let total: usize = done.iter().map(|s| s.card_count()).sum();
        assert_eq!(total, 1);
        assert_eq!(done[0].cards[0].pane_id, "w1:p1");
        assert_eq!(done[0].cards[0].status, Status::Done);
        // Idle 档：只有 w2:p1。
        let idle = build_sections(&snapshot, &agents, FilterMode::Idle);
        let total: usize = idle.iter().map(|s| s.card_count()).sum();
        assert_eq!(total, 1);
        assert_eq!(idle[0].cards[0].pane_id, "w2:p1");
        // Working 档：两张卡都不在 working，空。
        let working = build_sections(&snapshot, &agents, FilterMode::Working);
        assert_eq!(working.iter().map(|s| s.card_count()).sum::<usize>(), 0);
    }
}
