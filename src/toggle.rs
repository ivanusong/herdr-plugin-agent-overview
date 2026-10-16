//! popup 打开/关闭切换。
//!
//! herdr 的 popup 是会话级单例（同时只能有一个），但 API 没有“查询当前
//! popup 属主”的方法：`plugin.pane.open` 在已有 popup 时返回 `ui_busy`，
//! `popup.close` 关闭的是当前活跃 popup——不区分属主。
//!
//! 为了“重复 open 只 toggle 本插件的 popup、绝不误关其它 popup”，本插件的
//! TUI 启动时在 `HERDR_PLUGIN_STATE_DIR` 写入 `{pid, socket_path}`，退出时
//! 删除；toggle 动作按以下规则决策：
//!
//! 1. 状态文件存在、socket 路径一致、进程存活 → 当前 popup 是本插件在本
//!    会话打开的 → 发 `popup.close`。
//! 2. 否则尝试 `plugin.pane.open`；若返回 `ui_busy`，说明被其它 popup 或
//!    模态（Settings/Copy mode）占用 → 报错退出，**不关闭**。
//! 3. 状态文件残留（TUI 被强杀）：进程已死 → 清除残留后正常打开。

use std::io::Write as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::api::HerdrClient;
use crate::refresh::log;

const STATE_FILE: &str = "popup.json";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PopupState {
    pub pid: u32,
    pub socket_path: String,
    pub started_at: String,
}

/// 状态文件路径：`HERDR_PLUGIN_STATE_DIR/popup.json`；env 缺失时回落临时目录。
pub fn state_file() -> PathBuf {
    std::env::var("HERDR_PLUGIN_STATE_DIR")
        .map(|dir| PathBuf::from(dir).join(STATE_FILE))
        .unwrap_or_else(|_| std::env::temp_dir().join(format!("herdr-agent-overview-{STATE_FILE}")))
}

/// TUI 启动时调用：写入自己的 pid + socket 路径。
pub fn write_state(socket_path: &str) {
    let state = PopupState {
        pid: std::process::id(),
        socket_path: socket_path.to_string(),
        started_at: crate::refresh::log_ts(),
    };
    let path = state_file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string(&state) {
        Ok(body) => {
            if let Err(err) =
                std::fs::File::create(&path).and_then(|mut f| f.write_all(body.as_bytes()))
            {
                log(&format!("[toggle] 写状态文件失败 {path:?}: {err}"));
            }
        }
        Err(err) => log(&format!("[toggle] 序列化状态失败: {err}")),
    }
}

/// TUI 退出时调用：删除状态文件（尽力而为；被强杀时靠 liveness 检查兜底）。
pub fn clear_state() {
    let _ = std::fs::remove_file(state_file());
}

/// 读取“仍然存活的、属于当前 socket 会话”的 popup 状态。
pub fn read_alive_state(socket_path: &str) -> Option<PopupState> {
    let body = std::fs::read_to_string(state_file()).ok()?;
    let state: PopupState = serde_json::from_str(&body).ok()?;
    if state.socket_path != socket_path {
        // 另一个 herdr 会话（不同 socket）的 popup：不算本会话的。
        return None;
    }
    if !pid_alive(state.pid) {
        // 残留状态（TUI 被强杀）：清掉，视为没有。
        log(&format!(
            "[toggle] 清除残留状态文件（pid {} 已退出）",
            state.pid
        ));
        clear_state();
        return None;
    }
    Some(state)
}

/// 进程存活检查：kill(pid, 0)。EPERM 也说明进程存在（属主不同）。
fn pid_alive(pid: u32) -> bool {
    // pid=0 是非法目标；i32 截断在 Unix pid 范围内安全。
    let rc = unsafe { libc::kill(pid as i32, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// 执行 toggle 动作。返回进程退出码。
pub fn run_toggle(client: &HerdrClient, plugin_id: &str, entrypoint: &str) -> i32 {
    let socket_path = client.socket_path().to_string();
    if read_alive_state(&socket_path).is_some() {
        // 情形 1：本插件的 popup 正在运行（popup 是会话单例，因此它就是
        // 当前活跃 popup），关闭它是安全的。
        log("[toggle] 检测到本插件 popup 正在运行，发送 popup.close");
        return match client.popup_close() {
            Ok(_) => {
                println!("agent-overview popup 已关闭");
                0
            }
            Err(err) => {
                // 可能刚好自己退出了（popup_not_open）：清理状态即可。
                log(&format!("[toggle] popup.close 失败: {err}"));
                clear_state();
                if err.code == "popup_not_open" {
                    println!("agent-overview popup 已退出");
                    0
                } else {
                    eprintln!("关闭 popup 失败: {err}");
                    1
                }
            }
        };
    }

    // 情形 2：打开。
    log(&format!(
        "[toggle] 打开 popup（plugin={plugin_id} entrypoint={entrypoint}）"
    ));
    match client.plugin_pane_open(plugin_id, entrypoint) {
        Ok(_) => {
            log("[toggle] popup 打开成功");
            0
        }
        Err(err) if err.code == "ui_busy" => {
            // 被其它 popup 或模态占用：绝不关闭非本插件的 popup。
            eprintln!("无法打开：herdr 当前有另一个 popup 或模态（Settings/Copy mode）打开中");
            eprintln!("（本插件只切换自己的 popup，不会关闭其它 popup）");
            1
        }
        Err(err) => {
            eprintln!("打开 agent-overview 失败: {err}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_file_lifecycle() {
        // 合并为单个测试：set_var 是进程全局的，避免并行测试竞争。
        let dir = std::env::temp_dir().join("ao-test-state");
        std::env::set_var("HERDR_PLUGIN_STATE_DIR", &dir);
        let file = state_file();
        let _ = std::fs::remove_file(&file);

        // 正常写入：同 socket 可读到，其他 socket 读不到。
        write_state("/tmp/sock1");
        assert!(read_alive_state("/tmp/sock1").is_some());
        assert!(read_alive_state("/tmp/other-sock").is_none());
        clear_state();
        assert!(read_alive_state("/tmp/sock1").is_none());

        // 残留状态（进程已死）：读不到且被清理。
        let stale = PopupState {
            pid: u32::MAX / 2,
            socket_path: "/tmp/sock-x".into(),
            started_at: "2026-01-01".into(),
        };
        std::fs::write(&file, serde_json::to_string(&stale).unwrap()).unwrap();
        assert!(read_alive_state("/tmp/sock-x").is_none());
        assert!(!file.exists());
        let _ = std::fs::remove_file(&file);
    }
}
