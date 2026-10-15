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

/// 一次 inspect 同时拿运行状态与进程号（拆成两次就白起一个 docker 子进程）
fn inspect_state(container: &str) -> Option<(bool, u32)> {
    let mut cmd = Command::new("docker");
    cmd.args(["inspect", "-f", "{{.State.Running}} {{.State.Pid}}", container]);
    crate::drivers::command::hide_window(&mut cmd);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let (running, pid) = text.split_once(' ')?;
    Some((running == "true", pid.trim().parse().unwrap_or(0)))
}

/// 读取容器运行状态
pub fn status_running(container: &str) -> Option<bool> {
    inspect_state(container).map(|(running, _)| running)
}

/// `docker stats --no-stream` 的 CPU 与内存用量。
///
/// 容器指标只能问 Docker 自己：macOS 上容器进程跑在 Docker VM 里，宿主 sysinfo 按
/// `inspect` 回来的 PID 找过去必然查无此进程（那是 VM 内的编号），采样只会得到 0。
fn stats(container: &str) -> Option<(f32, f32)> {
    let out = run(&[
        "stats",
        "--no-stream",
        "--format",
        "{{.CPUPerc}}\t{{.MemUsage}}",
        container,
    ])
    .ok()?;
    let (cpu, mem) = out.split_once('\t')?;
    let cpu: f32 = cpu.trim().trim_end_matches('%').trim().parse().ok()?;
    // MemUsage 形如 "12.34MiB / 974.5MiB"，取使用量那一半
    let used = mem.split('/').next()?;
    Some((cpu, parse_size_mb(used)?))
}

/// Docker 打印的容量字符串折算成 MB（1024 进制，与进程采样的口径一致）。
/// 旧版 CLI 写 "kB/MB/GB"、新版写 "KiB/MiB/GiB"，两种都要认，
/// 否则 1.5GiB 会被当成 1.5MB。
fn parse_size_mb(s: &str) -> Option<f32> {
    let s = s.trim();
    let cut = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());
    let value: f32 = s[..cut].trim().parse().ok()?;
    let unit = s[cut..].trim().to_lowercase();
    let bytes = match unit.as_str() {
        "" | "b" => 1.0,
        "kb" | "kib" => 1024.0,
        "mb" | "mib" => 1024.0 * 1024.0,
        "gb" | "gib" => 1024.0f32.powi(3),
        "tb" | "tib" => 1024.0f32.powi(4),
        _ => return None,
    };
    Some(value * bytes / (1024.0 * 1024.0))
}

pub fn status_full(container: &str) -> crate::drivers::ExtStatus {
    let (running, pid) = match inspect_state(container) {
        Some(v) => v,
        None => return crate::drivers::ExtStatus::default(),
    };
    if !running {
        return crate::drivers::ExtStatus {
            running: Some(false),
            pid: None,
            metrics: None,
        };
    }
    crate::drivers::ExtStatus {
        running: Some(true),
        // 仅用于展示：跨平台采样一律走下面的 metrics，不拿这个 PID 去问 sysinfo
        pid: (pid > 0).then_some(pid),
        metrics: stats(container),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_size_mb;

    const MIB: f32 = 1024.0 * 1024.0;

    #[test]
    fn size_units_from_both_cli_generations() {
        assert_eq!(parse_size_mb("0B"), Some(0.0));
        assert_eq!(parse_size_mb("512B"), Some(512.0 / MIB));
        assert_eq!(parse_size_mb("1.5KiB"), Some(1.5 / 1024.0));
        assert_eq!(parse_size_mb("12.34MiB"), Some(12.34));
        assert_eq!(parse_size_mb("1.5GiB"), Some(1536.0));
        // 旧版 CLI 的 kB/MB/GB 同样是 1024 进制
        assert_eq!(parse_size_mb("200MB"), Some(200.0));
        assert_eq!(parse_size_mb("2GB"), Some(2048.0));
        assert_eq!(parse_size_mb("  3.25 GiB "), Some(3328.0));
    }

    #[test]
    fn size_garbage_is_none_not_zero() {
        assert_eq!(parse_size_mb("--"), None);
        assert_eq!(parse_size_mb(""), None);
        assert_eq!(parse_size_mb("12PiB"), None);
        assert_eq!(parse_size_mb("MiB"), None);
    }
}
