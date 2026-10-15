use std::process::{Command, Stdio};

fn run(args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("docker");
    cmd.args(args);
    crate::drivers::command::hide_window(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| format!("调用 docker 失败（Docker Desktop 是否在运行？）: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// 前置探测：Docker 引擎（守护进程）是否可达。
/// 用 `docker version` 探测——daemon 没起时它退出码非 0，且连不存在的命名管道会快速失败不卡住。
pub fn engine_up() -> bool {
    let mut cmd = Command::new("docker");
    cmd.args(["version", "--format", "{{.Server.Version}}"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::drivers::command::hide_window(&mut cmd);
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

pub fn start(container: &str) -> Result<(), String> {
    if !engine_up() {
        return Err("Docker 引擎未运行，请先启动 Docker Desktop 并等待其就绪后再启动容器".to_string());
    }
    run(&["start", container]).map(|_| ())
}

pub fn stop(container: &str) -> Result<(), String> {
    // 引擎没起时容器本就不在跑，视为已停止，避免抛 npipe 报错
    if !engine_up() {
        return Ok(());
    }
    run(&["stop", container]).map(|_| ())
}

/// 读取容器运行状态
pub fn status_running(container: &str) -> Option<bool> {
    let mut cmd = Command::new("docker");
    cmd.args(["inspect", "-f", "{{.State.Running}}", container]);
    crate::drivers::command::hide_window(&mut cmd);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim() == "true")
}
