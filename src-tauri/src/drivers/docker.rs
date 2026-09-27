use std::process::Command;

fn run(args: &[&str]) -> Result<String, String> {
    let out = Command::new("docker")
        .args(args)
        .output()
        .map_err(|e| format!("调用 docker 失败（Docker Desktop 是否在运行？）: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn start(container: &str) -> Result<(), String> {
    run(&["start", container]).map(|_| ())
}

pub fn stop(container: &str) -> Result<(), String> {
    run(&["stop", container]).map(|_| ())
}

/// 读取容器运行状态
pub fn status_running(container: &str) -> Option<bool> {
    let out = Command::new("docker")
        .args(["inspect", "-f", "{{.State.Running}}", container])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim() == "true")
}
