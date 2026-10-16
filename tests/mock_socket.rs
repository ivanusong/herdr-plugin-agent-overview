//! 集成测试：mock herdr Unix socket 服务器。
//!
//! 覆盖场景（对应验收要求）：
//! 1. agent.list + snapshot 正确映射、过滤普通 shell / 已退出 agent；
//! 2. 动态增减 agent 时卡片集合更新、选中按 terminal id 保持；
//! 3. 状态变化只允许官方五态；
//! 4. 单个 pane 读取失败不影响整体；
//! 5. socket 断线：保留数据、显示错误、自动恢复；
//! 6. 20 个 agent 全部被读取（并发受限 4）。

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use herdr_agent_overview::api::HerdrClient;
use herdr_agent_overview::refresh;
use herdr_agent_overview::state::{Shared, Status};
use herdr_agent_overview::ui::App;

// ---------- mock 服务器 ----------

#[derive(Default)]
struct MockState {
    workspaces: Vec<Value>,
    tabs: Vec<Value>,
    panes: Vec<Value>,
    agents: Vec<Value>,
    texts: HashMap<String, String>,
    fail_panes: HashSet<String>,
    agent_list_error: Option<(String, String)>,
    down: bool,
    focused: Mutex<Vec<String>>,
    pane_reads: Mutex<HashMap<String, usize>>,
    snapshots: Mutex<usize>,
}

struct MockServer {
    path: PathBuf,
    state: Arc<Mutex<MockState>>,
    shutdown: Arc<AtomicBool>,
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_file(&self.path);
    }
}

impl MockServer {
    fn start(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ao-mock-{}-{}.sock",
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind mock socket");
        let state = Arc::new(Mutex::new(MockState::default()));
        let shutdown = Arc::new(AtomicBool::new(false));
        {
            let state = state.clone();
            let shutdown = shutdown.clone();
            std::thread::Builder::new()
                .name(format!("mock-{name}"))
                .spawn(move || {
                    // 测试结束后不再 accept（listener 随线程结束丢弃）。
                    for stream in listener.incoming() {
                        if shutdown.load(Ordering::Relaxed) {
                            return;
                        }
                        let Ok(stream) = stream else { continue };
                        let down = state.lock().unwrap().down;
                        if down {
                            // 模拟断线：接受连接后立即关闭（客户端读到 EOF）。
                            drop(stream);
                            continue;
                        }
                        handle_connection(stream, &state);
                    }
                })
                .expect("spawn mock server");
        }
        MockServer {
            path,
            state,
            shutdown,
        }
    }

    fn client(&self) -> HerdrClient {
        HerdrClient::new(self.path.to_string_lossy().to_string())
    }

    fn shared(&self) -> Arc<Mutex<Shared>> {
        Arc::new(Mutex::new(Shared::default()))
    }

    fn spawn_refresh(&self, shared: Arc<Mutex<Shared>>) -> (Arc<AtomicBool>, Arc<refresh::Wakeup>) {
        let stop = Arc::new(AtomicBool::new(false));
        let wakeup = refresh::spawn_all(Arc::new(self.client()), shared, stop.clone());
        (stop, wakeup)
    }
}

/// 处理一条连接：读一行请求，回一行响应，关闭（与 herdr 单请求连接语义一致）。
fn handle_connection(stream: UnixStream, state: &Arc<Mutex<MockState>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
        return;
    }
    let Ok(request) = serde_json::from_str::<Value>(&line) else {
        return;
    };
    let id = request["id"].as_str().unwrap_or("").to_string();
    let method = request["method"].as_str().unwrap_or("").to_string();
    let response = match method.as_str() {
        "ping" => json!({"id": id, "result": {"type": "pong", "version": "mock", "protocol": 22}}),
        "session.snapshot" => {
            let st = state.lock().unwrap();
            *st.snapshots.lock().unwrap() += 1;
            json!({
                "id": id,
                "result": {"type": "session_snapshot", "snapshot": {
                    "focused_workspace_id": st.workspaces.first().and_then(|w| w["workspace_id"].as_str()),
                    "workspaces": st.workspaces,
                    "tabs": st.tabs,
                    "panes": st.panes,
                    "layouts": [],
                    "agents": st.agents,
                    "version": "mock",
                    "protocol": 22,
                }}
            })
        }
        "agent.list" => {
            let st = state.lock().unwrap();
            match &st.agent_list_error {
                Some((code, message)) => json!({
                    "id": id,
                    "error": {"code": code, "message": message}
                }),
                None => json!({"id": id, "result": {"type": "agent_list", "agents": st.agents}}),
            }
        }
        "pane.read" => {
            let pane_id = request["params"]["pane_id"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let st = state.lock().unwrap();
            *st.pane_reads
                .lock()
                .unwrap()
                .entry(pane_id.clone())
                .or_insert(0) += 1;
            if st.fail_panes.contains(&pane_id) {
                json!({"id": id, "error": {"code": "pane_not_found", "message": format!("pane {pane_id} not found")}})
            } else {
                let text = st.texts.get(&pane_id).cloned().unwrap_or_else(|| {
                    format!("output of {pane_id}\nsecond line\r\n\x1b[31;1mstyled tail\x1b[0m")
                });
                json!({"id": id, "result": {"type": "pane_read", "read": {
                    "pane_id": pane_id, "workspace_id": "", "tab_id": "",
                    "source": "visible", "format": "ansi", "text": text,
                    "revision": 0, "truncated": false
                }}})
            }
        }
        "pane.focus" => {
            let pane_id = request["params"]["pane_id"]
                .as_str()
                .unwrap_or("")
                .to_string();
            state.lock().unwrap().focused.lock().unwrap().push(pane_id);
            json!({"id": id, "result": {"type": "pane_focus", "changed": true}})
        }
        "events.subscribe" => json!({
            "id": id,
            "error": {"code": "not_implemented", "message": "mock 不支持订阅（轮询兜底）"}
        }),
        _ => json!({"id": id, "error": {"code": "not_implemented", "message": method}}),
    };
    let mut stream = stream;
    let _ = stream.write_all(format!("{response}\n").as_bytes());
}

// ---------- fixture 构造 ----------

fn workspace(id: &str, number: usize, label: &str) -> Value {
    json!({"workspace_id": id, "number": number, "label": label, "focused": false,
           "active_tab_id": format!("{id}:t1"), "pane_count": 0, "tab_count": 1,
           "agent_status": "unknown"})
}

fn tab(id: &str, workspace_id: &str, number: usize, label: &str) -> Value {
    json!({"tab_id": id, "workspace_id": workspace_id, "number": number,
           "label": label, "focused": false, "pane_count": 1, "agent_status": "unknown"})
}

fn pane(pane_id: &str, terminal_id: &str, workspace_id: &str, tab_id: &str, status: &str) -> Value {
    json!({
        "pane_id": pane_id, "terminal_id": terminal_id, "workspace_id": workspace_id,
        "tab_id": tab_id, "focused": false, "agent_status": status, "revision": 1
    })
}

fn agent(
    pane_id: &str,
    terminal_id: &str,
    kind: &str,
    name: &str,
    status: &str,
    ws: &str,
    tab: &str,
) -> Value {
    json!({
        "pane_id": pane_id, "terminal_id": terminal_id, "agent": kind, "name": name,
        "agent_status": status, "workspace_id": ws, "tab_id": tab, "focused": false,
        "cwd": format!("/Users/ivan/proj/{ws}"), "foreground_cwd": format!("/Users/ivan/proj/{ws}")
    })
}

/// 等待条件成立（默认 5s），超时 panic。
fn wait_for(what: &str, cond: impl Fn() -> bool, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("等待超时: {what}");
}

fn total_cards(shared: &Arc<Mutex<Shared>>) -> usize {
    shared.lock().unwrap().total_cards()
}

// ---------- 测试 ----------

#[test]
fn discovery_mapping_and_filtering_with_previews() {
    let server = MockServer::start("mapping");
    {
        let mut st = server.state.lock().unwrap();
        st.workspaces = vec![workspace("w1", 1, "Alpha"), workspace("w2", 2, "Beta")];
        st.tabs = vec![tab("w1:t1", "w1", 1, "build"), tab("w2:t1", "w2", 1, "1")];
        st.panes = vec![
            pane("w1:p1", "term_1", "w1", "w1:t1", "working"),
            pane("w1:p2", "term_2", "w1", "w1:t1", "working"),
            // 普通 shell：不在 agent.list，必须被过滤。
            pane("w1:p9", "term_9", "w1", "w1:t1", "unknown"),
            // 已退出 agent：pane 尚存、身份全空、状态 unknown，必须被过滤。
            pane("w1:p8", "term_8", "w1", "w1:t1", "unknown"),
            // 非 working（idle）：不展示、也不读取。
            pane("w2:p1", "term_3", "w2", "w2:t1", "idle"),
        ];
        st.agents = vec![
            agent("w1:p1", "term_1", "pi", "coder", "working", "w1", "w1:t1"),
            agent("w1:p2", "term_2", "claude", "", "working", "w1", "w1:t1"),
            // 已退出：全部身份字段为 null。
            json!({"pane_id": "w1:p8", "terminal_id": "term_8", "agent": null, "name": null,
                    "agent_status": "unknown", "workspace_id": "w1", "tab_id": "w1:t1", "focused": false}),
            agent("w2:p1", "term_3", "pi", "", "idle", "w2", "w2:t1"),
        ];
    }
    let shared = server.shared();
    let (stop, _wakeup) = server.spawn_refresh(shared.clone());

    wait_for(
        "2 张 working 卡片且预览就绪",
        || {
            let guard = shared.lock().unwrap();
            guard.total_cards() == 2
                && guard
                    .find("term_1")
                    .is_some_and(|(si, ci)| !guard.sections[si].cards[ci].preview.is_empty())
        },
        Duration::from_secs(5),
    );
    {
        let shared = shared.lock().unwrap();
        // 只有 Alpha 分区（Beta 的 idle agent 被过滤）。
        assert_eq!(shared.sections.len(), 1, "非 working 分区不出现");
        let w1 = &shared.sections[0];
        assert_eq!(w1.label, "Alpha");
        assert_eq!(w1.cards.len(), 2);

        let coder = &w1.cards[0];
        assert_eq!(coder.terminal_id, "term_1");
        assert_eq!(coder.title, "coder");
        assert_eq!(coder.agent_kind.as_deref(), Some("pi"));
        assert_eq!(coder.status, Status::Working);
        assert_eq!(coder.status.label(), "working");
        assert_eq!(coder.cwd_label, "w1");
        assert_eq!(coder.tab_label, "build");
        // 预览来自 mock 文本，ANSI 解析后含样式尾巴。
        assert!(coder
            .preview
            .iter()
            .any(|l| { l.spans.iter().any(|s| s.content.contains("styled tail")) }));

        let claude = &w1.cards[1];
        assert_eq!(claude.title, "claude"); // name 为空回落 agent 类型
        assert_eq!(claude.status, Status::Working);
    }

    // 读取计数：只有 working 的 agent pane 被读取；其余一律不读（包括
    // 非 working 的 idle agent —— 读取资源不花在隐藏卡上）。
    {
        let st = server.state.lock().unwrap();
        let reads = st.pane_reads.lock().unwrap();
        assert!(reads.get("w1:p1").copied().unwrap_or(0) >= 1);
        assert!(reads.get("w1:p2").copied().unwrap_or(0) >= 1);
        assert!(reads.get("w2:p1").is_none(), "非 working agent 不应被读取");
        assert!(reads.get("w1:p9").is_none(), "普通 shell 不应被读取");
        assert!(reads.get("w1:p8").is_none(), "已退出 pane 不应被读取");
    }
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn dynamic_add_remove_keeps_stable_ids_and_selection() {
    let server = MockServer::start("dynamic");
    {
        let mut st = server.state.lock().unwrap();
        st.workspaces = vec![workspace("w1", 1, "Alpha")];
        st.tabs = vec![tab("w1:t1", "w1", 1, "1")];
        st.panes = vec![
            pane("w1:p1", "term_1", "w1", "w1:t1", "working"),
            pane("w1:p2", "term_2", "w1", "w1:t1", "working"),
        ];
        st.agents = vec![
            agent("w1:p1", "term_1", "pi", "a", "working", "w1", "w1:t1"),
            agent("w1:p2", "term_2", "pi", "b", "working", "w1", "w1:t1"),
        ];
    }
    let shared = server.shared();
    let (stop, _wakeup) = server.spawn_refresh(shared.clone());

    wait_for(
        "初始 2 卡",
        || total_cards(&shared) == 2,
        Duration::from_secs(5),
    );

    // UI 选中 term_2（卡片 2）。
    let mut app = App::default();
    {
        let shared = shared.lock().unwrap();
        app.sync_selection(&shared);
        // 默认选第一张；把选中切到 term_2。
        if let Some(pos) = shared.find("term_2") {
            app.select(&shared, pos);
        }
    }
    assert_eq!(app.selected_terminal.as_deref(), Some("term_2"));

    // 动态变化：term_2 消失（agent+pane 同撤），新增 term_3。
    {
        let mut st = server.state.lock().unwrap();
        st.panes.retain(|p| p["pane_id"] != "w1:p2");
        st.agents.retain(|a| a["pane_id"] != "w1:p2");
        st.panes
            .push(pane("w1:p3", "term_3", "w1", "w1:t1", "working"));
        st.agents.push(agent(
            "w1:p3", "term_3", "pi", "c", "working", "w1", "w1:t1",
        ));
    }

    wait_for(
        "卡片集合变为 term_1 + term_3",
        || {
            let shared = shared.lock().unwrap();
            shared.find("term_1").is_some()
                && shared.find("term_3").is_some()
                && shared.find("term_2").is_none()
        },
        Duration::from_secs(5),
    );

    // 选中按 terminal_id 保持：term_2 消失后回退到合法卡片（不 panic、不越界）。
    {
        let shared = shared.lock().unwrap();
        app.sync_selection(&shared);
        assert!(app.selected.is_some());
        let selected_terminal = app.selected_terminal.clone().unwrap();
        assert!(selected_terminal == "term_1" || selected_terminal == "term_3");
    }

    // 再验证 Tab 环游在动态集合上不越界。
    {
        let shared = shared.lock().unwrap();
        app.move_flat(&shared, 1);
        app.move_flat(&shared, 1); // 环回
        app.move_flat(&shared, -1);
        assert!(app.selected.is_some());
    }
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn status_change_adds_and_removes_cards() {
    let server = MockServer::start("status");
    {
        let mut st = server.state.lock().unwrap();
        st.workspaces = vec![workspace("w1", 1, "Alpha")];
        st.tabs = vec![tab("w1:t1", "w1", 1, "1")];
        st.panes = vec![pane("w1:p1", "term_1", "w1", "w1:t1", "idle")];
        st.agents = vec![agent("w1:p1", "term_1", "pi", "a", "idle", "w1", "w1:t1")];
    }
    let shared = server.shared();
    let (stop, _wakeup) = server.spawn_refresh(shared.clone());

    // 初始 idle：不展示（0 卡）。
    wait_for(
        "初始无卡（idle 不展示）",
        || {
            let guard = shared.lock().unwrap();
            guard.connected && guard.total_cards() == 0
        },
        Duration::from_secs(5),
    );

    // idle → working：卡片自动出现。
    {
        let mut st = server.state.lock().unwrap();
        st.agents[0]["agent_status"] = json!("working");
        st.panes[0]["agent_status"] = json!("working");
    }
    wait_for(
        "working 出现 1 卡",
        || {
            let guard = shared.lock().unwrap();
            guard.total_cards() == 1 && guard.find("term_1").is_some()
        },
        Duration::from_secs(5),
    );
    {
        let guard = shared.lock().unwrap();
        let (si, ci) = guard.find("term_1").unwrap();
        assert_eq!(guard.sections[si].cards[ci].status, Status::Working);
    }

    // working → blocked：卡片自动移除。
    {
        let mut st = server.state.lock().unwrap();
        st.agents[0]["agent_status"] = json!("blocked");
    }
    wait_for(
        "blocked 后卡片移除",
        || {
            let guard = shared.lock().unwrap();
            guard.total_cards() == 0 && guard.find("term_1").is_none()
        },
        Duration::from_secs(5),
    );

    // blocked → working：再次出现（动态增删双向）。
    {
        let mut st = server.state.lock().unwrap();
        st.agents[0]["agent_status"] = json!("working");
    }
    wait_for(
        "working 复现",
        || {
            let guard = shared.lock().unwrap();
            guard.total_cards() == 1
        },
        Duration::from_secs(5),
    );
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn filter_switch_changes_cards_and_stops_reading_other_states() {
    use herdr_agent_overview::state::FilterMode;
    let server = MockServer::start("filter");
    {
        let mut st = server.state.lock().unwrap();
        st.workspaces = vec![workspace("w1", 1, "Alpha")];
        st.tabs = vec![tab("w1:t1", "w1", 1, "1")];
        st.panes = vec![
            pane("w1:p1", "term_1", "w1", "w1:t1", "working"),
            pane("w1:p2", "term_2", "w1", "w1:t1", "done"),
            pane("w1:p3", "term_3", "w1", "w1:t1", "idle"),
        ];
        st.agents = vec![
            agent("w1:p1", "term_1", "pi", "w", "working", "w1", "w1:t1"),
            agent("w1:p2", "term_2", "pi", "d", "done", "w1", "w1:t1"),
            agent("w1:p3", "term_3", "pi", "i", "idle", "w1", "w1:t1"),
        ];
    }
    let shared = server.shared();
    let (stop, _wakeup) = server.spawn_refresh(shared.clone());

    // 默认 Working：只有 working 卡，且只读 working pane。
    wait_for(
        "默认只展示 working",
        || {
            let guard = shared.lock().unwrap();
            guard.total_cards() == 1 && guard.find("term_1").is_some()
        },
        Duration::from_secs(5),
    );
    {
        let guard = shared.lock().unwrap();
        let (si, ci) = guard.find("term_1").unwrap();
        assert_eq!(guard.sections[si].cards[ci].status, Status::Working);
        assert_eq!(guard.filter, FilterMode::Working);
        let st = server.state.lock().unwrap();
        let reads = st.pane_reads.lock().unwrap();
        assert!(reads.get("w1:p1").copied().unwrap_or(0) >= 1);
        assert!(reads.get("w1:p2").is_none(), "done 不在当前档，不得读取");
        assert!(reads.get("w1:p3").is_none(), "idle 不在当前档，不得读取");
    }

    // 切换到 Done：done 卡出现；稍后 working pane 不再被读取。
    shared.lock().unwrap().filter = FilterMode::Done;
    wait_for(
        "切换到 Done",
        || {
            let guard = shared.lock().unwrap();
            guard.total_cards() == 1 && guard.find("term_2").is_some()
        },
        Duration::from_secs(5),
    );
    let working_reads_before = {
        let st = server.state.lock().unwrap();
        let reads = st.pane_reads.lock().unwrap();
        reads.get("w1:p1").copied().unwrap_or(0)
    };
    std::thread::sleep(Duration::from_millis(700));
    {
        let st = server.state.lock().unwrap();
        let reads = st.pane_reads.lock().unwrap();
        assert_eq!(
            reads.get("w1:p1").copied().unwrap_or(0),
            working_reads_before,
            "切换后 working pane 不得再被读取"
        );
        assert!(
            reads.get("w1:p2").copied().unwrap_or(0) >= 2,
            "done pane 应持续被读取"
        );
    }

    // 切换到 Idle：idle 卡出现。
    shared.lock().unwrap().filter = FilterMode::Idle;
    wait_for(
        "切换到 Idle",
        || {
            let guard = shared.lock().unwrap();
            guard.total_cards() == 1 && guard.find("term_3").is_some()
        },
        Duration::from_secs(5),
    );

    // 切回 Working：卡片自动回来。
    shared.lock().unwrap().filter = FilterMode::Working;
    wait_for(
        "切回 Working",
        || {
            let guard = shared.lock().unwrap();
            guard.total_cards() == 1 && guard.find("term_1").is_some()
        },
        Duration::from_secs(5),
    );
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn single_read_failure_is_isolated_and_recovers() {
    let server = MockServer::start("read-fail");
    {
        let mut st = server.state.lock().unwrap();
        st.workspaces = vec![workspace("w1", 1, "Alpha")];
        st.tabs = vec![tab("w1:t1", "w1", 1, "1")];
        st.panes = vec![
            pane("w1:p1", "term_1", "w1", "w1:t1", "working"),
            pane("w1:p2", "term_2", "w1", "w1:t1", "working"),
        ];
        st.agents = vec![
            agent("w1:p1", "term_1", "pi", "a", "working", "w1", "w1:t1"),
            agent("w1:p2", "term_2", "pi", "b", "working", "w1", "w1:t1"),
        ];
        st.fail_panes.insert("w1:p1".to_string());
    }
    let shared = server.shared();
    let (stop, _wakeup) = server.spawn_refresh(shared.clone());

    wait_for(
        "2 卡就绪且失败标记落地",
        || {
            let guard = shared.lock().unwrap();
            guard.total_cards() == 2
                && guard
                    .find("term_1")
                    .is_some_and(|(si, ci)| guard.sections[si].cards[ci].read_error.is_some())
        },
        Duration::from_secs(5),
    );
    {
        let shared = shared.lock().unwrap();
        assert!(shared.connected, "单个读取失败不影响整体连接状态");
        let (si, ci) = shared.find("term_1").unwrap();
        let failed = &shared.sections[si].cards[ci];
        assert!(failed.read_error.is_some(), "失败卡片带错误标记");
        assert!(failed.preview.is_empty() || failed.read_error.is_some());
        let (si, ci) = shared.find("term_2").unwrap();
        let ok = &shared.sections[si].cards[ci];
        assert!(ok.read_error.is_none(), "其它卡片不受影响");
        assert!(!ok.preview.is_empty(), "其它卡片有预览");
    }

    // 恢复：失败 pane 恢复后下一轮拿到预览、错误清除。
    server.state.lock().unwrap().fail_panes.clear();
    wait_for(
        "失败卡片恢复预览",
        || {
            let shared = shared.lock().unwrap();
            let (si, ci) = shared.find("term_1").unwrap();
            shared.sections[si].cards[ci].read_error.is_none()
                && !shared.sections[si].cards[ci].preview.is_empty()
        },
        Duration::from_secs(5),
    );
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn socket_outage_shows_error_and_recovers() {
    let server = MockServer::start("outage");
    {
        let mut st = server.state.lock().unwrap();
        st.workspaces = vec![workspace("w1", 1, "Alpha")];
        st.tabs = vec![tab("w1:t1", "w1", 1, "1")];
        st.panes = vec![pane("w1:p1", "term_1", "w1", "w1:t1", "working")];
        st.agents = vec![agent(
            "w1:p1", "term_1", "pi", "a", "working", "w1", "w1:t1",
        )];
    }
    let shared = server.shared();
    let (stop, _wakeup) = server.spawn_refresh(shared.clone());
    wait_for(
        "初始连接且有 working 卡",
        || {
            let guard = shared.lock().unwrap();
            guard.connected && guard.total_cards() == 1
        },
        Duration::from_secs(5),
    );

    // 断线：mock 拒绝服务（接受连接后立即关闭）。
    server.state.lock().unwrap().down = true;
    wait_for(
        "断线被报告",
        || {
            let shared = shared.lock().unwrap();
            !shared.connected && shared.last_error.is_some()
        },
        Duration::from_secs(10),
    );
    {
        // 断线期间保留最后一次数据，UI 不闪空。
        let guard = shared.lock().unwrap();
        assert_eq!(guard.total_cards(), 1, "断线时保留旧卡片");
    }

    // 断线期间状态从 working 变为 idle；恢复后重建应移除该卡片（数据一致）。
    {
        let mut st = server.state.lock().unwrap();
        st.agents[0]["agent_status"] = json!("idle");
        st.down = false;
    }
    wait_for(
        "自动恢复且卡片随状态移除",
        || {
            let shared = shared.lock().unwrap();
            shared.connected && shared.last_error.is_none() && shared.total_cards() == 0
        },
        Duration::from_secs(10),
    );
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn agent_list_error_falls_back_to_snapshot_agents() {
    let server = MockServer::start("agent-list-error");
    {
        let mut st = server.state.lock().unwrap();
        st.workspaces = vec![workspace("w1", 1, "Alpha")];
        st.tabs = vec![tab("w1:t1", "w1", 1, "1")];
        st.panes = vec![pane("w1:p1", "term_1", "w1", "w1:t1", "working")];
        st.agents = vec![agent(
            "w1:p1", "term_1", "pi", "a", "working", "w1", "w1:t1",
        )];
        st.agent_list_error = Some(("boom".into(), "agent.list 挂了".into()));
    }
    let shared = server.shared();
    let (stop, _wakeup) = server.spawn_refresh(shared.clone());
    wait_for(
        "回落 snapshot.agents 仍有 1 卡",
        || total_cards(&shared) == 1,
        Duration::from_secs(5),
    );
    {
        let shared = shared.lock().unwrap();
        assert!(shared.connected);
        let (si, ci) = shared.find("term_1").unwrap();
        assert_eq!(shared.sections[si].cards[ci].status, Status::Working);
    }
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn twenty_agents_all_read_with_limited_concurrency() {
    let server = MockServer::start("twenty");
    {
        let mut st = server.state.lock().unwrap();
        for w in 1..=4 {
            let ws_id = format!("w{w}");
            st.workspaces.push(workspace(&ws_id, w, &format!("ws-{w}")));
            for t in 1..=5 {
                let tab_id = format!("{ws_id}:t{t}");
                st.tabs.push(tab(&tab_id, &ws_id, t, &format!("t{t}")));
                let pane_id = format!("{ws_id}:p{t}");
                let term_id = format!("term_{w}_{t}");
                st.panes
                    .push(pane(&pane_id, &term_id, &ws_id, &tab_id, "working"));
                st.agents.push(agent(
                    &pane_id,
                    &term_id,
                    "pi",
                    &format!("agent-{w}-{t}"),
                    "working",
                    &ws_id,
                    &tab_id,
                ));
            }
        }
    }
    let shared = server.shared();
    let (stop, _wakeup) = server.spawn_refresh(shared.clone());

    wait_for(
        "20 张卡片且全部预览就绪",
        || {
            let guard = shared.lock().unwrap();
            guard.total_cards() == 20
                && guard.sections.iter().all(|s| {
                    s.cards
                        .iter()
                        .all(|c| !c.preview.is_empty() && c.read_error.is_none())
                })
        },
        Duration::from_secs(10),
    );
    {
        let shared = shared.lock().unwrap();
        assert_eq!(shared.sections.len(), 4);
        // 全部卡片都有预览（读取并发 4，但最终每张都读到）。
        for section in &shared.sections {
            for card in &section.cards {
                assert!(!card.preview.is_empty(), "卡片 {} 缺预览", card.pane_id);
                assert!(card.read_error.is_none());
            }
        }
        // 排序：第一个分区是 w1，第一张卡是 t1 的 agent。
        assert_eq!(shared.sections[0].label, "ws-1");
        assert_eq!(shared.sections[0].cards[0].title, "agent-1-1");
        assert_eq!(shared.sections[3].cards[4].title, "agent-4-5");
    }
    {
        let st = server.state.lock().unwrap();
        let reads = st.pane_reads.lock().unwrap();
        assert_eq!(reads.len(), 20, "20 个 pane 全部被读取");
        for (pane_id, count) in reads.iter() {
            assert!(*count >= 1, "{pane_id} 读取次数 {count}");
        }
    }
    stop.store(true, Ordering::Relaxed);
}

#[test]
fn client_pane_focus_roundtrip() {
    let server = MockServer::start("focus");
    let client = server.client();
    client.pane_focus("w1:p1").expect("pane.focus 成功");
    {
        let st = server.state.lock().unwrap();
        let focused = st.focused.lock().unwrap();
        assert_eq!(focused.last().map(String::as_str), Some("w1:p1"));
    }
}

#[test]
fn grid_navigation_moves_within_and_across_rows() {
    // 纯 UI 层测试：不依赖 socket，构造 sections 后验证网格导航语义。
    use herdr_agent_overview::state::Card;
    let mk = |tid: &str| Card {
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
    let shared = Arc::new(Mutex::new(Shared {
        sections: vec![herdr_agent_overview::state::Section {
            workspace_id: "w1".into(),
            label: "w".into(),
            number: 1,
            cards: (0..5).map(|i| mk(&format!("c{i}"))).collect(),
        }],
        connected: true,
        last_error: None,
        last_refresh: None,
        filter: herdr_agent_overview::state::FilterMode::default(),
    }));
    let mut app = App::default();
    {
        let shared = shared.lock().unwrap();
        app.sync_selection(&shared);
        assert_eq!(app.selected_terminal.as_deref(), Some("c0"));
    }
    // nav_rows 为空时（尚未 draw），导航安全无操作。
    {
        let shared = shared.lock().unwrap();
        app.move_horizontal(&shared, 1);
        app.move_vertical(&shared, 1);
        assert_eq!(app.selected_terminal.as_deref(), Some("c0"));
        // 扁平移动仍可用。
        app.move_flat(&shared, 1);
        assert_eq!(app.selected_terminal.as_deref(), Some("c1"));
    }
    // 模拟 3 列布局的 nav 结构：行 = [c0,c1,c2], [c3,c4]。
    app.nav_rows = vec![vec![(0, 0), (0, 1), (0, 2)], vec![(0, 3), (0, 4)]];
    app.nav_pos = [
        ((0, 0), (0, 0)),
        ((0, 1), (0, 1)),
        ((0, 2), (0, 2)),
        ((0, 3), (1, 0)),
        ((0, 4), (1, 1)),
    ]
    .into_iter()
    .collect();
    {
        let shared = shared.lock().unwrap();
        // c1 右移 → c2。
        app.select(&shared, (0, 1));
        app.move_horizontal(&shared, 1);
        assert_eq!(app.selected_terminal.as_deref(), Some("c2"));
        // c2 右移循环 → c0。
        app.move_horizontal(&shared, 1);
        assert_eq!(app.selected_terminal.as_deref(), Some("c0"));
        // c0 下移（第二行列数不足，取行尾）→ c3。
        app.move_vertical(&shared, 1);
        assert_eq!(app.selected_terminal.as_deref(), Some("c3"));
        // c3 下移到底不越界。
        app.move_vertical(&shared, 1);
        assert_eq!(app.selected_terminal.as_deref(), Some("c3"));
        // c3 上移回 c0。
        app.move_vertical(&shared, -1);
        assert_eq!(app.selected_terminal.as_deref(), Some("c0"));
    }
}
