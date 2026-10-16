//! 后台刷新：元数据 + 预览读取 + 事件加速。
//!
//! 设计要点：
//! - UI 线程绝不碰网络；这里通过 `Arc<Mutex<Shared>>` 交换数据。
//! - 默认 4Hz 全量刷新：`session.snapshot` + `agent.list`（元数据）+
//!   每 pane 一条 `pane.read`（ANSI 可视区）。单条连接一次请求（herdr 语义）。
//! - 读取并发受限（默认 4 worker），单 pane 失败只影响自己的卡片。
//! - `events.subscribe` 作为加速器：事件到达即提前触发下一轮刷新（节流）；
//!   订阅失败/丢失（events_lost）/断线时自动退避重订，轮询始终兜底。
//! - socket 断开：保留最后一次数据，UI 显示错误横幅，每 tick 自动重试恢复。

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::ansi;
use crate::api::{ApiError, HerdrClient};
use crate::discovery;
use crate::state::Shared;

/// 元数据 + 预览刷新节奏（4Hz）。
pub const REFRESH_INTERVAL: Duration = Duration::from_millis(250);
/// pane.read 并发上限。
pub const READ_WORKERS: usize = 4;
/// 事件触发的提前刷新最小间隔（防止事件风暴打满 socket）。
const EVENT_REFRESH_MIN_INTERVAL: Duration = Duration::from_millis(120);
/// 等待本轮读取结果的最长时间；超时结果留到下一轮套用。
const READ_COLLECT_TIMEOUT: Duration = Duration::from_millis(1500);

/// 唤醒刷新线程的信号（事件到达 / 关闭）。
#[derive(Default)]
pub struct Wakeup {
    mutex: Mutex<bool>,
    cond: Condvar,
}

impl Wakeup {
    pub fn notify(&self) {
        let mut pending = self.mutex.lock().unwrap();
        *pending = true;
        self.cond.notify_one();
    }

    /// 等待最多 `timeout`；返回 true 表示标志位被置位（提前唤醒）。
    fn wait_timeout(&self, timeout: Duration) -> bool {
        let mut pending = self.mutex.lock().unwrap();
        let (guard, _) = self
            .cond
            .wait_timeout_while(pending, timeout, |flag: &mut bool| !*flag)
            .unwrap();
        pending = guard;
        let woken = *pending;
        *pending = false;
        woken
    }
}

// ---------- 读取 worker 池 ----------

type ReadJob = String;
type ReadOutcome = (String, Result<String, ApiError>);

/// 固定大小的 pane.read 并发池。drop 时（进程退出）worker 一并退出。
struct ReadPool {
    job_tx: mpsc::Sender<ReadJob>,
    out_rx: mpsc::Receiver<ReadOutcome>,
}

impl ReadPool {
    fn new(client: Arc<HerdrClient>, size: usize) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<ReadJob>();
        let (out_tx, out_rx) = mpsc::channel::<ReadOutcome>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        for worker in 0..size {
            let client = client.clone();
            let job_rx = job_rx.clone();
            let out_tx = out_tx.clone();
            std::thread::Builder::new()
                .name(format!("ao-read-{worker}"))
                .spawn(move || loop {
                    let pane_id = {
                        let rx = job_rx.lock().unwrap();
                        match rx.recv() {
                            Ok(pane_id) => pane_id,
                            Err(_) => break, // 所有发送端已关闭：进程退出
                        }
                    };
                    let outcome = client.pane_read(&pane_id);
                    if out_tx.send((pane_id, outcome)).is_err() {
                        break;
                    }
                })
                .expect("spawn read worker");
        }
        Self { job_tx, out_rx }
    }

    fn dispatch(&self, pane_id: &str) {
        if let Err(err) = self.job_tx.send(pane_id.to_string()) {
            log(&format!("[refresh] 读取任务派发失败（池已关闭）: {err}"));
        }
    }

    /// 非阻塞收割所有已完成的读取结果。
    fn drain(&self) -> Vec<ReadOutcome> {
        let mut drained = Vec::new();
        while let Ok(outcome) = self.out_rx.try_recv() {
            drained.push(outcome);
        }
        drained
    }

    /// 最多等到 `deadline`，逐条收割结果；收满 `expected` 条即提前返回。
    fn collect_until(&self, deadline: Instant, expected: usize) -> Vec<ReadOutcome> {
        let mut collected = Vec::new();
        loop {
            if collected.len() >= expected {
                return collected;
            }
            let now = Instant::now();
            if now >= deadline {
                return collected;
            }
            match self.out_rx.recv_timeout(deadline - now) {
                Ok(outcome) => collected.push(outcome),
                Err(mpsc::RecvTimeoutError::Timeout) => return collected,
                Err(mpsc::RecvTimeoutError::Disconnected) => return collected,
            }
        }
    }
}

/// 启动全部后台任务：4Hz 刷新线程 + 读取 worker 池 + 事件订阅线程。
pub fn spawn_all(
    client: Arc<HerdrClient>,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
) -> Arc<Wakeup> {
    let wakeup = Arc::new(Wakeup::default());
    let refresh_wakeup = wakeup.clone();
    let pool = ReadPool::new(client.clone(), READ_WORKERS);
    let events_client = client.clone();
    let events_shared = shared.clone();
    let events_stop = stop.clone();
    std::thread::Builder::new()
        .name("ao-refresh".into())
        .spawn({
            let wakeup = refresh_wakeup;
            move || refresh_loop(client, shared, pool, wakeup, stop)
        })
        .expect("spawn refresh thread");
    std::thread::Builder::new()
        .name("ao-events".into())
        .spawn({
            let wakeup = wakeup.clone();
            move || events_loop(events_client, events_shared, wakeup, events_stop)
        })
        .expect("spawn events thread");
    wakeup
}

// ---------- 刷新主循环 ----------

fn refresh_loop(
    client: Arc<HerdrClient>,
    shared: Arc<Mutex<Shared>>,
    pool: ReadPool,
    wakeup: Arc<Wakeup>,
    stop: Arc<AtomicBool>,
) {
    let mut last_refresh: Option<Instant> = None;
    let mut last_event_refresh: Option<Instant> = None;
    let mut backoff = ReadBackoff::default();

    while !stop.load(Ordering::Relaxed) {
        let since_last = last_refresh.map_or(REFRESH_INTERVAL, |t| t.elapsed());
        if since_last >= REFRESH_INTERVAL {
            run_cycle(&client, &shared, &pool, &mut backoff);
            last_refresh = Some(Instant::now());
            last_event_refresh = Some(Instant::now());
            continue;
        }

        let wait = REFRESH_INTERVAL - since_last;
        let woken = wakeup.wait_timeout(wait);
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // 事件触发的提前刷新：节流到 EVENT_REFRESH_MIN_INTERVAL。
        if woken && last_event_refresh.is_none_or(|t| t.elapsed() >= EVENT_REFRESH_MIN_INTERVAL) {
            last_event_refresh = Some(Instant::now());
            run_cycle(&client, &shared, &pool, &mut backoff);
            last_refresh = Some(Instant::now());
        }
    }
    log("[refresh] 线程退出");
}

/// 一轮完整刷新：收割遗留结果 → 元数据（snapshot+agent.list）→ 重建卡片
/// （按 terminal_id 保留旧预览）→ 并发派发读取 → 收割套用。
fn run_cycle(
    client: &HerdrClient,
    shared: &Arc<Mutex<Shared>>,
    pool: &ReadPool,
    backoff: &mut ReadBackoff,
) {
    let cycle_started = Instant::now();
    // 上一轮超时遗留的读取结果：先收割，最后套用到最新卡片。
    let stale = pool.drain();

    let snapshot = match client.snapshot() {
        Ok(s) => s,
        Err(err) => {
            let mut shared = shared.lock().unwrap();
            shared.connected = false;
            shared.last_error = Some(err.to_string());
            log(&format!("[refresh] snapshot 失败（保留旧数据）: {err}"));
            return;
        }
    };
    // agent.list 为主，失败回落 snapshot.agents（同一数据源，不影响正确性）。
    let agents = match client.agent_list() {
        Ok(a) => a,
        Err(err) => {
            log(&format!(
                "[refresh] agent.list 失败，回落 snapshot.agents: {err}"
            ));
            snapshot.agents.clone()
        }
    };

    // 当前筛选档位在构建前一刻读取：`f` 切换后下一轮（或事件唤醒
    // 的提前刷新）即生效，卡片随档位自动增删。
    let filter = shared.lock().unwrap().filter;
    let mut sections = discovery::build_sections(&snapshot, &agents, filter);
    let pane_ids: Vec<String> = sections
        .iter()
        .flat_map(|s| s.cards.iter().map(|c| c.pane_id.clone()))
        .collect();

    {
        let mut shared = shared.lock().unwrap();
        // 元数据更新后按 terminal_id 保留旧预览/旧错误，避免每 tick 闪空白。
        let mut previous: HashMap<String, CardPreview> = HashMap::new();
        for section in &shared.sections {
            for card in &section.cards {
                previous.insert(
                    card.terminal_id.clone(),
                    (card.preview.clone(), card.read_error.clone()),
                );
            }
        }
        for section in &mut sections {
            for card in &mut section.cards {
                if let Some((preview, error)) = previous.remove(&card.terminal_id) {
                    card.preview = preview;
                    card.read_error = error;
                }
            }
        }
        shared.sections = sections;
        shared.connected = true;
        shared.last_error = None;
        shared.last_refresh = Some(Instant::now());
    }

    // 并发读取（worker 池固定 4 并发），失败 pane 按退避跳过；
    // 结果收齐（或超时）后套用。expected 用实际派发数：退避跳过的 pane
    // 不会有结果，否则每轮都会白等满超时、拉低刷新频率。
    let now = Instant::now();
    let mut dispatched = 0usize;
    for pane_id in &pane_ids {
        if backoff.should_try(pane_id, now) {
            pool.dispatch(pane_id);
            dispatched += 1;
        }
    }
    let outcomes = pool.collect_until(Instant::now() + READ_COLLECT_TIMEOUT, dispatched);
    for (pane_id, result) in outcomes {
        match &result {
            Ok(_) => {
                if backoff.record_success(&pane_id) {
                    log(&format!("[refresh] pane {pane_id} 读取恢复"));
                }
            }
            Err(_) => {
                if backoff.record_failure(&pane_id, Instant::now()) {
                    log(&format!("[refresh] pane {pane_id} 读取失败，进入退避"));
                }
            }
        }
        apply_read(shared, &pane_id, result);
    }
    // 上一轮遗留的结果也套用（按 pane/terminal id 匹配，仍有效）。
    for (pane_id, result) in stale {
        apply_read(shared, &pane_id, result);
    }
    log(&format!(
        "[refresh] 一轮完成：{} panes，耗时 {}ms",
        pane_ids.len(),
        cycle_started.elapsed().as_millis()
    ));
}

type CardPreview = (Vec<ratatui::text::Line<'static>>, Option<String>);

/// 把一条读取结果套用到对应卡片（pane/terminal id 匹配；解析 ANSI）。
fn apply_read(shared: &Arc<Mutex<Shared>>, pane_id: &str, result: Result<String, ApiError>) {
    let mut shared = shared.lock().unwrap();
    for section in &mut shared.sections {
        for card in &mut section.cards {
            if card.pane_id == pane_id || card.terminal_id == pane_id {
                match result {
                    Ok(text) => {
                        card.preview = ansi::parse(&text);
                        card.read_error = None;
                    }
                    Err(err) => {
                        // 保留上一次成功预览，只记录错误（UI 信息条红色提示）。
                        // 逐次失败不写日志：退避层已降噪，这里不刷屏。
                        card.read_error = Some(err.to_string());
                    }
                }
                return;
            }
        }
    }
}

// ---------- pane 读取失败退避 ----------

/// 日志文件大小上限：超过后轮转为 `.log.1`（保留一代）。
const LOG_MAX_BYTES: u64 = 512 * 1024;

/// 退避序列（毫秒）：首次失败后 500ms 内不重试，逐步翻倍，封顶 5s。
const BACKOFF_STEPS_MS: &[u64] = &[500, 1_000, 2_000, 4_000, 5_000];

#[derive(Debug)]
struct BackoffEntry {
    failures: u32,
    last_failure: Instant,
}

/// 按 pane_id 记录读取失败退避；成功清零，pane 从拓扑移除后条目自然失效。
#[derive(Default)]
struct ReadBackoff {
    entries: HashMap<String, BackoffEntry>,
}

impl ReadBackoff {
    fn delay(failures: u32) -> Duration {
        let index = ((failures as usize).saturating_sub(1)).min(BACKOFF_STEPS_MS.len() - 1);
        Duration::from_millis(BACKOFF_STEPS_MS[index])
    }

    /// 距上次失败不足退避窗口时跳过本轮读取（卡片保留旧预览与错误提示）。
    fn should_try(&self, pane_id: &str, now: Instant) -> bool {
        match self.entries.get(pane_id) {
            None => true,
            Some(entry) => now.duration_since(entry.last_failure) >= Self::delay(entry.failures),
        }
    }

    /// 记录一次失败；返回 true 表示这是首次失败（可记一条日志）。
    fn record_failure(&mut self, pane_id: &str, now: Instant) -> bool {
        let entry = self
            .entries
            .entry(pane_id.to_string())
            .or_insert(BackoffEntry {
                failures: 0,
                last_failure: now,
            });
        entry.failures = entry.failures.saturating_add(1);
        entry.last_failure = now;
        entry.failures == 1
    }

    /// 记录成功并清零；返回 true 表示此前处于失败状态（可记一条恢复日志）。
    fn record_success(&mut self, pane_id: &str) -> bool {
        self.entries.remove(pane_id).is_some()
    }

    #[cfg(test)]
    fn failures(&self, pane_id: &str) -> u32 {
        self.entries.get(pane_id).map(|e| e.failures).unwrap_or(0)
    }
}

// ---------- events.subscribe 加速器 ----------

/// 订阅的 lifecycle 事件集合（全局型，无需 pane_id）。
const LIFECYCLE_EVENTS: &[&str] = &[
    "workspace.created",
    "workspace.closed",
    "workspace.focused",
    "workspace.renamed",
    "tab.created",
    "tab.closed",
    "tab.focused",
    "tab.renamed",
    "pane.created",
    "pane.closed",
    "pane.updated",
    "pane.focused",
    "pane.exited",
    "pane.agent_detected",
    "layout.updated",
];

fn events_loop(
    client: Arc<HerdrClient>,
    shared: Arc<Mutex<Shared>>,
    wakeup: Arc<Wakeup>,
    stop: Arc<AtomicBool>,
) {
    let mut backoff = Duration::from_millis(500);
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        // 订阅集合 = 全局 lifecycle + 当前已知 agent pane 的状态变化。
        // 本连接只在断开后重订时刷新 pane 集合（新 agent 的状态事件最多
        // 延迟一个重连周期，轮询兑底不影响正确性）。
        let panes: Vec<String> = {
            let shared = shared.lock().unwrap();
            shared
                .sections
                .iter()
                .flat_map(|s| s.cards.iter().map(|c| c.pane_id.clone()))
                .collect()
        };
        let known_panes = panes;

        let mut subs: Vec<serde_json::Value> = LIFECYCLE_EVENTS
            .iter()
            .map(|e| serde_json::json!({"type": e}))
            .collect();
        for pane_id in &known_panes {
            subs.push(serde_json::json!({"type": "pane.agent_status_changed", "pane_id": pane_id}));
        }

        match subscribe_and_read(&client, &subs, &wakeup, &stop) {
            EventOutcome::Stopped => return,
            EventOutcome::Lost(reason) => {
                log(&format!(
                    "[events] 订阅断开（{reason}），退避后重订；轮询仍在兜底"
                ));
                // 分片 sleep，便于快速响应 stop。
                let deadline = Instant::now() + backoff;
                while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(100));
                }
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        }
    }
}

enum EventOutcome {
    Stopped,
    Lost(String),
}

/// 建立订阅并阻塞读取事件行；每行唤醒刷新线程。连接断开返回 Lost。
fn subscribe_and_read(
    client: &HerdrClient,
    subs: &[serde_json::Value],
    wakeup: &Wakeup,
    stop: &AtomicBool,
) -> EventOutcome {
    use std::io::{BufRead, Write};
    let stream = match std::os::unix::net::UnixStream::connect(client.socket_path()) {
        Ok(s) => s,
        Err(err) => return EventOutcome::Lost(format!("connect: {err}")),
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let mut request = serde_json::json!({
        "id": "ao:events",
        "method": "events.subscribe",
        "params": {"subscriptions": subs},
    })
    .to_string();
    request.push('\n');
    let mut writer = stream.try_clone().expect("clone stream");
    if let Err(err) = writer.write_all(request.as_bytes()) {
        return EventOutcome::Lost(format!("write: {err}"));
    }

    let reader = std::io::BufReader::new(stream);
    log(&format!("[events] 已订阅 {} 类事件", subs.len()));
    for line in reader.lines() {
        if stop.load(Ordering::Relaxed) {
            return EventOutcome::Stopped;
        }
        let line = match line {
            Ok(l) => l,
            Err(err) => return EventOutcome::Lost(format!("read: {err}")),
        };
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if value.get("error").is_some() {
            // events_lost 等订阅级错误：断开重订，靠 snapshot 对账。
            let code = value["error"]["code"].as_str().unwrap_or("unknown");
            return EventOutcome::Lost(code.to_string());
        }
        if value["result"]["type"] == "subscription_started" {
            continue; // 订阅确认
        }
        if value.get("event").is_some() {
            // 事件到达：立即唤醒刷新（事件只是信号，数据以 snapshot/reads 为准）。
            wakeup.notify();
        }
    }
    EventOutcome::Lost("连接关闭".to_string())
}

/// 进程级日志（stderr；herdr 会收进 plugin log）。
/// 依据环境启用调试日志。默认（未设 `AGENT_OVERVIEW_DEBUG=1`）完全静默：
/// TUI 与用户终端共享 stdout/stderr，任何常规日志都会污染 alternate screen
/// 并造成滚动。启用时写入插件 state 目录下的 `agent-overview.log`（轮转）。
/// 必须在启动时调用一次（TUI / toggle / check 各自入口）。
pub fn init_debug_log_from_env() {
    let enabled = std::env::var("AGENT_OVERVIEW_DEBUG").ok().as_deref() == Some("1");
    let sink = if enabled {
        LogSink::File(Mutex::new(log_file_path()))
    } else {
        LogSink::Disabled
    };
    let _ = LOG_SINK.set(sink);
}

fn log_file_path() -> PathBuf {
    let dir = std::env::var("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    dir.join("agent-overview.log")
}

enum LogSink {
    Disabled,
    File(Mutex<PathBuf>),
}

static LOG_SINK: OnceLock<LogSink> = OnceLock::new();

/// 写一条日志。默认 sink 为 Disabled：**不写 stdout/stderr**（TUI 安全），
/// 仅在 `AGENT_OVERVIEW_DEBUG=1` 时落盘。未初始化时同样静默。
pub fn log(message: &str) {
    if let Some(LogSink::File(path)) = LOG_SINK.get() {
        if let Err(err) = emit_to_file(path, message, LOG_MAX_BYTES) {
            // 日志失败绝不能反噬主流程，也不能写终端；只能放弃。
            let _ = err;
        }
    }
}

/// 追加一条日志到文件；超过 `max_bytes` 时先轮转为 `.log.1`（保留一代）。
fn emit_to_file(path: &Mutex<PathBuf>, message: &str, max_bytes: u64) -> std::io::Result<()> {
    let path = path.lock().unwrap();
    if let Ok(meta) = std::fs::metadata(&*path) {
        if meta.len() >= max_bytes {
            let mut rotated = path.clone();
            rotated.set_extension("log.1");
            let _ = std::fs::rename(&*path, rotated);
        }
    }
    let mut file: File = OpenOptions::new().create(true).append(true).open(&*path)?;
    writeln!(file, "[{}] [INFO] {}", log_ts(), message)
}

/// 供其他模块复用的时间戳：`2026-10-16 12:34:56Z`。
pub fn log_ts() -> String {
    utc_now()
}

/// UTC 时间戳，格式 `2026-10-16 12:34:56Z`（避免引入 chrono 依赖）。
fn utc_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02}Z")
}

/// Howard Hinnant 的 civil_from_days（天数 → 公历年月日）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_matches_known_dates() {
        // 2026-10-16 是 epoch 后 20742 天；2026-01-01 是 20454 天。
        assert_eq!(civil_from_days(20742), (2026, 10, 16));
        assert_eq!(civil_from_days(20454), (2026, 1, 1));
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19723), (2024, 1, 1));
    }

    // ---------- 退避 ----------

    #[test]
    fn backoff_first_failure_blocks_next_tick_then_recovers() {
        let mut backoff = ReadBackoff::default();
        let t0 = Instant::now();
        // 从未失败：允许读取。
        assert!(backoff.should_try("w66:p1", t0));
        // 首次失败：500ms 内禁止重试（避免 4Hz 连打），之后恢复。
        assert!(backoff.record_failure("w66:p1", t0), "首次失败应报告");
        assert!(!backoff.should_try("w66:p1", t0 + Duration::from_millis(250)));
        assert!(backoff.should_try("w66:p1", t0 + Duration::from_millis(500)));
        // 二次失败：退避翻倍到 1s。
        assert!(!backoff.record_failure("w66:p1", t0 + Duration::from_millis(500)));
        assert!(!backoff.should_try("w66:p1", t0 + Duration::from_millis(1_200)));
        assert!(backoff.should_try("w66:p1", t0 + Duration::from_millis(1_500)));
        assert_eq!(backoff.failures("w66:p1"), 2);
    }

    #[test]
    fn backoff_success_resets_and_delay_is_capped() {
        let mut backoff = ReadBackoff::default();
        let t0 = Instant::now();
        // 连续失败封顶在 5s。
        for i in 0..10u32 {
            backoff.record_failure("p", t0 + Duration::from_millis(i as u64 * 10));
        }
        assert_eq!(
            ReadBackoff::delay(10),
            Duration::from_millis(*BACKOFF_STEPS_MS.last().unwrap())
        );
        // 成功即清零：立即可读，无残留窗口。
        assert!(backoff.record_success("p"));
        assert!(backoff.should_try("p", t0));
        assert!(!backoff.record_success("p"), "无失败记录时恢复不报告");
    }

    // ---------- 日志 ----------

    #[test]
    fn log_without_init_is_silent_and_writes_nothing() {
        // 全局未 init（测试进程从不调用 init_debug_log_from_env）时
        // log() 必须静默：不写终端、不产生文件、不 panic。
        let dir = std::env::temp_dir().join(format!(
            "ao-silent-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        log("这条日志必须无处可去");
        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert!(entries.is_empty(), "未启用 debug 时不得产生日志文件");
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn log_file_writes_and_rotates() {
        let dir = std::env::temp_dir().join(format!(
            "ao-log-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = Mutex::new(dir.join("agent-overview.log"));
        let small_limit: u64 = 256;
        let line = "x".repeat(64);
        for _ in 0..10 {
            emit_to_file(&path, &line, small_limit).unwrap();
        }
        let main_path = path.lock().unwrap().clone();
        let rotated = dir.join("agent-overview.log.1");
        assert!(rotated.exists(), "超过上限后应轮转出 .log.1");
        assert!(
            std::fs::metadata(&main_path).unwrap().len() < small_limit + 128,
            "轮转后主文件应重新从 0 计数"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
