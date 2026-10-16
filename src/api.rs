//! herdr Unix socket API 客户端。
//!
//! 协议：newline-delimited JSON，服务器对非订阅连接“一条连接只处理一个请求”
//! （herdr src/api/server.rs），因此每次调用都新建连接；本机 Unix socket
//! connect 的开销在微秒级，4Hz × N 个 pane 完全可承受。
//!
//! 所有请求都带读写超时，单点故障不会卡住 UI 线程（网络只发生在后台线程）。

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

/// 写超时：请求行很小，1s 足够。
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);
/// 读超时：覆盖服务器响应；超过即视为本轮失败，由上层重试。
const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// API 层错误：保留服务器错误码（如 `popup_not_open`、`ui_busy`），供调用方分支处理。
#[derive(Debug, Clone)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl ApiError {
    pub fn transport(message: impl Into<String>) -> Self {
        Self {
            code: "transport".to_string(),
            message: message.into(),
        }
    }
}

#[derive(Deserialize)]
struct Envelope<T> {
    result: Option<T>,
    error: Option<EnvelopeError>,
}

#[derive(Deserialize)]
struct EnvelopeError {
    code: String,
    message: String,
}

#[derive(Deserialize)]
struct SnapshotResult {
    snapshot: Snapshot,
}

/// `session.snapshot` 响应。所有字段都容忍缺失/多余（serde 忽略未知字段）。
#[derive(Deserialize, Debug, Clone, Default)]
pub struct Snapshot {
    #[serde(default)]
    pub focused_workspace_id: Option<String>,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceInfo>,
    #[serde(default)]
    pub tabs: Vec<TabInfo>,
    #[serde(default)]
    pub panes: Vec<PaneInfo>,
    #[serde(default)]
    pub agents: Vec<AgentInfo>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    #[serde(default)]
    pub number: usize,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub focused: bool,
}

#[derive(Deserialize, Debug, Clone)]
pub struct TabInfo {
    pub tab_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub number: usize,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub focused: bool,
}

#[derive(Deserialize, Debug, Clone)]
pub struct PaneInfo {
    pub pane_id: String,
    #[serde(default)]
    pub terminal_id: String,
    pub tab_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub agent_status: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub display_agent: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    /// 原生 agent 会话引用；只用于判断“是否仍有 agent 身份”，不解析内容。
    #[serde(default)]
    pub agent_session: Option<Value>,
}

/// `agent.list` / `snapshot.agents` 记录。
#[derive(Deserialize, Debug, Clone)]
pub struct AgentInfo {
    pub pane_id: String,
    #[serde(default)]
    pub terminal_id: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub display_agent: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub agent_status: String,
    pub workspace_id: String,
    pub tab_id: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub agent_session: Option<Value>,
}

#[derive(Deserialize)]
struct AgentListResult {
    agents: Vec<AgentInfo>,
}

#[derive(Deserialize)]
struct ReadResultBody {
    read: ReadResult,
}

#[derive(Deserialize)]
struct ReadResult {
    text: String,
}

/// herdr socket 客户端。廉价克隆（只有一个路径字符串），可在多线程间共享。
#[derive(Debug, Clone)]
pub struct HerdrClient {
    socket: String,
}

impl HerdrClient {
    pub fn new(socket: impl Into<String>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// 从环境解析 socket 路径：优先 `HERDR_SOCKET_PATH`（herdr 注入），
    /// 其次 `HERDR_SESSION` 命名会话，最后默认会话路径。
    pub fn from_env() -> Result<Self, String> {
        if let Ok(path) = std::env::var("HERDR_SOCKET_PATH") {
            if !path.is_empty() {
                return Ok(Self::new(path));
            }
        }
        if let Ok(session) = std::env::var("HERDR_SESSION") {
            if !session.is_empty() {
                return Ok(Self::new(format!(
                    "{}/sessions/{}/herdr.sock",
                    default_config_dir(),
                    session
                )));
            }
        }
        Ok(Self::new(format!("{}/herdr.sock", default_config_dir())))
    }

    pub fn socket_path(&self) -> &str {
        &self.socket
    }

    /// 发送一个请求并读取响应。一次连接一个请求（与 herdr 服务器行为一致）。
    pub fn call<T: for<'de> Deserialize<'de>>(
        &self,
        id: &str,
        method: &str,
        params: &Value,
    ) -> Result<T, ApiError> {
        let mut stream = UnixStream::connect(&self.socket)
            .map_err(|err| ApiError::transport(format!("connect {}: {err}", self.socket)))?;
        let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
        let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
        let mut line = json!({"id": id, "method": method, "params": params}).to_string();
        line.push('\n');
        stream
            .write_all(line.as_bytes())
            .map_err(|err| ApiError::transport(format!("write: {err}")))?;
        let mut response = String::new();
        BufReader::new(stream)
            .read_line(&mut response)
            .map_err(|err| ApiError::transport(format!("read: {err}")))?;
        if response.trim().is_empty() {
            return Err(ApiError::transport("空响应（服务器断开连接）"));
        }
        let envelope: Envelope<T> = serde_json::from_str(&response)
            .map_err(|err| ApiError::transport(format!("bad response: {err}")))?;
        if let Some(error) = envelope.error {
            return Err(ApiError {
                code: error.code,
                message: error.message,
            });
        }
        envelope
            .result
            .ok_or_else(|| ApiError::transport("响应缺少 result 字段".to_string()))
    }

    /// 全会话快照（workspace/tab/pane/layout/agent 记录）。
    pub fn snapshot(&self) -> Result<Snapshot, ApiError> {
        self.call::<SnapshotResult>("ao:snapshot", "session.snapshot", &json!({}))
            .map(|r| r.snapshot)
    }

    /// 所有 Agent 记录（herdr 判定为 agent 终端的 pane，天然排除普通 shell）。
    pub fn agent_list(&self) -> Result<Vec<AgentInfo>, ApiError> {
        self.call::<AgentListResult>("ao:agents", "agent.list", &json!({}))
            .map(|r| r.agents)
    }

    /// 读取 pane 当前可视区（完整 viewport，不用 recent* 源，避免顶部内容读成空白）。
    pub fn pane_read(&self, pane_id: &str) -> Result<String, ApiError> {
        self.call::<ReadResultBody>(
            "ao:read",
            "pane.read",
            &json!({
                "pane_id": pane_id,
                "source": "visible",
                "format": "ansi",
            }),
        )
        .map(|r| r.read.text)
    }

    /// 聚焦某个 pane（唯一的“写”操作，仅在用户明确 Enter/双击后调用）。
    pub fn pane_focus(&self, pane_id: &str) -> Result<(), ApiError> {
        self.call::<Value>("ao:focus", "pane.focus", &json!({ "pane_id": pane_id }))
            .map(|_| ())
    }

    /// 打开本插件的 popup entrypoint（placement/尺寸来自 manifest）。
    pub fn plugin_pane_open(&self, plugin_id: &str, entrypoint: &str) -> Result<Value, ApiError> {
        self.call::<Value>(
            "ao:open",
            "plugin.pane.open",
            &json!({
                "plugin_id": plugin_id,
                "entrypoint": entrypoint,
                "focus": true,
            }),
        )
    }

    /// 关闭当前会话的 popup（调用方必须已确认 popup 属于本插件，见 toggle.rs）。
    pub fn popup_close(&self) -> Result<Value, ApiError> {
        self.call::<Value>("ao:popup-close", "popup.close", &json!({}))
    }
}

fn default_config_dir() -> String {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        if !dir.is_empty() {
            return format!("{dir}/herdr");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    format!("{home}/.config/herdr")
}
