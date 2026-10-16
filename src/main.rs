//! herdr-agent-overview 入口。
//!
//! 用法：
//! - `herdr-agent-overview`          运行全屏 popup TUI（manifest `[[panes]]` entrypoint）
//! - `herdr-agent-overview toggle`   action：切换本插件 popup（重复触发关闭自己的）
//! - `herdr-agent-overview --check [--socket PATH]`  headless 诊断：读取真实
//!   snapshot / agent.list / 一条 pane 预览并打印，便于验收
//! - `herdr-agent-overview --version`

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseButton, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::Position;

use herdr_agent_overview::api::HerdrClient;
use herdr_agent_overview::input;
use herdr_agent_overview::refresh;
use herdr_agent_overview::state::Shared;
use herdr_agent_overview::toggle;
use herdr_agent_overview::ui::{self, App};

/// 滚轮一格对应的虚拟行数。
const SCROLL_STEP: u16 = 3;

fn main() -> ExitCode {
    // 恢复 Unix 默认 SIGPIPE 行为：`--check | head` 等管道提前关闭时安静退出
    // （Rust 默认忽略 SIGPIPE 并在 println! 处 panic）。
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    // 日志 sink：默认静默（TUI 与终端共享 PTY，日志会污染画面）；
    // 仅 AGENT_OVERVIEW_DEBUG=1 时写入 state 目录日志文件。
    refresh::init_debug_log_from_env();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => run_tui(),
        Some("toggle") => run_toggle(),
        Some("--check") => run_check(),
        Some("--version") | Some("-V") => {
            println!("herdr-agent-overview {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("未知参数: {other}\n用法: herdr-agent-overview [toggle|--check [--socket PATH]|--version]");
            ExitCode::FAILURE
        }
    }
}

fn plugin_id() -> String {
    std::env::var("HERDR_PLUGIN_ID").unwrap_or_else(|_| "agent-overview".to_string())
}

fn run_toggle() -> ExitCode {
    let client = match HerdrClient::from_env() {
        Ok(c) => c,
        Err(err) => {
            eprintln!("herdr-agent-overview: {err}");
            return ExitCode::FAILURE;
        }
    };
    let code = toggle::run_toggle(&client, &plugin_id(), "overview");
    ExitCode::from(code as u8)
}

// ---------- headless 诊断 ----------

fn run_check() -> ExitCode {
    let mut socket: Option<String> = None;
    let mut args = std::env::args().skip(2);
    while let Some(arg) = args.next() {
        if arg == "--socket" {
            socket = args.next();
        }
    }
    let client = match socket {
        Some(path) => HerdrClient::new(path),
        None => match HerdrClient::from_env() {
            Ok(c) => c,
            Err(err) => {
                eprintln!("--check: {err}");
                return ExitCode::FAILURE;
            }
        },
    };
    println!("socket      : {}", client.socket_path());

    let snapshot = match client.snapshot() {
        Ok(s) => s,
        Err(err) => {
            eprintln!("--check: session.snapshot 失败: {err}");
            return ExitCode::FAILURE;
        }
    };
    let agents = match client.agent_list() {
        Ok(a) => a,
        Err(err) => {
            eprintln!("--check: agent.list 失败: {err}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "session     : {} workspaces / {} tabs / {} panes / {} agent 记录(snapshot.agents={})",
        snapshot.workspaces.len(),
        snapshot.tabs.len(),
        snapshot.panes.len(),
        agents.len(),
        snapshot.agents.len()
    );

    // 诊断模式按默认档位（Working）展示，与 TUI 默认行为一致。
    let sections = herdr_agent_overview::discovery::build_sections(
        &snapshot,
        &agents,
        herdr_agent_overview::state::FilterMode::Working,
    );
    let total: usize = sections.iter().map(|s| s.card_count()).sum();
    if total == 0 {
        println!("发现 agent  : 0（No working agents；TUI 按 f 可切 Done/Idle）");
        return ExitCode::SUCCESS;
    }
    println!("发现 agent  : {total}（Filter: Working；TUI 按 f 可切 Done/Idle）");
    for section in &sections {
        println!(
            "  w{} · {} ({})",
            section.number,
            section.label,
            section.card_count()
        );
        for card in &section.cards {
            println!(
                "    [{}] {:<28} kind={:<10} {} · {} · {}",
                card.status.label(),
                card.title,
                card.agent_kind.as_deref().unwrap_or("-"),
                card.tab_label,
                card.cwd_label,
                card.pane_id,
            );
        }
    }

    // 抽查第一条卡片的可视区读取。
    let first = &sections[0].cards[0];
    match client.pane_read(&first.pane_id) {
        Ok(text) => {
            let plain: String = herdr_agent_overview::ansi::parse(&text)
                .iter()
                .take(3)
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join(" ⏎ ");
            println!(
                "pane.read   : {} 字节（ANSI），前几行: {}",
                text.len(),
                if plain.is_empty() {
                    "(空白屏)"
                } else {
                    &plain
                }
            );
        }
        Err(err) => {
            println!("pane.read   : 失败（{err}）");
        }
    }
    println!("--check 通过");
    ExitCode::SUCCESS
}

// ---------- popup TUI ----------

enum Outcome {
    Cancel,
    OpenPane { pane_id: String },
}

fn run_tui() -> ExitCode {
    let client = match HerdrClient::from_env() {
        Ok(c) => c,
        Err(err) => {
            eprintln!("herdr-agent-overview: {err}");
            return ExitCode::FAILURE;
        }
    };
    refresh::log(&format!(
        "[tui] 启动（pid={} socket={}）",
        std::process::id(),
        client.socket_path()
    ));

    // 状态文件先于一切 UI 写入，供 toggle 判定 popup 属主。
    toggle::write_state(client.socket_path());

    let shared = Arc::new(Mutex::new(Shared::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let wakeup = refresh::spawn_all(Arc::new(client.clone()), shared.clone(), stop.clone());

    let mut app = App::default();
    let outcome = match run_event_loop(&mut app, &shared, &wakeup) {
        Ok(outcome) => outcome,
        Err(err) => {
            refresh::log(&format!("[tui] 终端错误: {err}"));
            stop.store(true, Ordering::Relaxed);
            toggle::clear_state();
            eprintln!("herdr-agent-overview: 终端错误: {err}");
            return ExitCode::FAILURE;
        }
    };

    stop.store(true, Ordering::Relaxed);
    toggle::clear_state();
    refresh::log(&match &outcome {
        Outcome::Cancel => "[tui] 正常退出".to_string(),
        Outcome::OpenPane { pane_id } => format!("[tui] 跳转 pane {pane_id}"),
    });

    // 跳转：在 TUI 拆除后、进程退出前执行 pane.focus（herdr 0.9 已验证
    // popup 关闭不会覆盖这次 focus）。这是全插件唯一的写操作。
    if let Outcome::OpenPane { pane_id } = outcome {
        if let Err(err) = client.pane_focus(&pane_id) {
            refresh::log(&format!("[tui] pane.focus {pane_id} 失败: {err}"));
            eprintln!("herdr-agent-overview: focus {pane_id} 失败: {err}");
            return ExitCode::FAILURE;
        }
        refresh::log(&format!("[tui] pane.focus {pane_id} 完成"));
    }
    ExitCode::SUCCESS
}

fn run_event_loop(
    app: &mut App,
    shared: &Arc<Mutex<Shared>>,
    wakeup: &refresh::Wakeup,
) -> std::io::Result<Outcome> {
    let mut terminal = ratatui::try_init()?;
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let outcome = event_loop(&mut terminal, app, shared, wakeup);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    outcome
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    shared: &Arc<Mutex<Shared>>,
    wakeup: &refresh::Wakeup,
) -> std::io::Result<Outcome> {
    let mut clicks = input::ClickTracker::default();
    loop {
        let snapshot_guard = shared.lock().unwrap();
        terminal.draw(|frame| ui::draw(frame, app, &snapshot_guard))?;
        drop(snapshot_guard);

        // 键盘/鼠标事件驱动：poll 超时即下一帧（刷新线程持续更新 shared）。
        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let ev = event::read()?;
        let mut shared_now = shared.lock().unwrap();
        match ev {
            // 键盘决策在 input::handle_key（可测）；它在判断前先 sync_selection，
            // 避免首轮刷新后、下一帧绘制前的窗口内按键被吞。
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                match input::handle_key(app, &shared_now, key.code) {
                    input::Action::None => {}
                    input::Action::Cancel => {
                        drop(shared_now);
                        return Ok(Outcome::Cancel);
                    }
                    input::Action::OpenPane { pane_id } => {
                        drop(shared_now);
                        return Ok(Outcome::OpenPane { pane_id });
                    }
                    input::Action::CycleFilter => {
                        // 档位是共享状态（刷新线程据此构建卡片）；改完唤醒
                        // 刷新线程尽快重建，避免长时间显示旧档卡片。
                        shared_now.filter = shared_now.filter.next();
                        refresh::log(&format!("[tui] 筛选切换：{}", shared_now.filter.label()));
                        wakeup.notify();
                    }
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => {
                    app.follow_selection = false;
                    app.scroll = app.scroll.saturating_sub(SCROLL_STEP);
                }
                MouseEventKind::ScrollDown => {
                    app.follow_selection = false;
                    app.scroll = app.scroll.saturating_add(SCROLL_STEP);
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    // 启动窗口内 tile_rects 可能尚未由任何一帧填充（shared 已有
                    // 卡片）：先补一帧，保证命中测试有几何可用（与 handle_key
                    // 的 sync_selection 同理，避免首击丢失打断双击链）。
                    if app.tile_rects.is_empty() && shared_now.total_cards() > 0 {
                        terminal.draw(|frame| ui::draw(frame, app, &shared_now))?;
                    }
                    let position = Position::new(mouse.column, mouse.row);
                    let Some(&(_, tile)) = app
                        .tile_rects
                        .iter()
                        .find(|(rect, _)| rect.contains(position))
                    else {
                        continue;
                    };
                    // 单击选中；同一卡双击跳转（决策在 input::handle_click，可测）。
                    match input::handle_click(app, &shared_now, tile, &mut clicks, Instant::now()) {
                        input::Action::None => {}
                        // handle_click 只会返回 None / OpenPane。
                        input::Action::Cancel | input::Action::CycleFilter => {
                            unreachable!("handle_click 不返回 Cancel/CycleFilter")
                        }
                        input::Action::OpenPane { pane_id } => {
                            drop(shared_now);
                            return Ok(Outcome::OpenPane { pane_id });
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
        drop(shared_now);
    }
}
