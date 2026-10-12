//! 系统服务驱动，按平台分派：
//!   Windows -> sc (start/stop/query)
//!   Linux   -> systemctl (start/stop/is-active)
//!   macOS   -> launchctl (start/stop/list)
//! 对外统一暴露 start / stop / status_running / stop_and_wait。
//! 注意：Linux/macOS 分支无法在 Windows 上编译验证，且管理系统级服务通常需要 root/sudo，
//!       请在对应真机上验证后再用于生产。
use std::process::Command;
use std::time::{Duration, Instant};

pub use imp::*;

// ============================ Windows ============================
#[cfg(windows)]
mod imp {
    use super::*;

    const STATE_STOPPED: u32 = 1;
    const STATE_RUNNING: u32 = 4;

    fn run(args: &[&str]) -> Result<String, String> {
        let out = Command::new("sc")
            .args(args)
            .output()
            .map_err(|e| format!("调用 sc 失败: {e}"))?;
        // sc 在中文 Windows 上输出 OEM(936/GBK)
        let stdout = crate::logs::decode_text(&out.stdout, "gbk");
        let stderr = crate::logs::decode_text(&out.stderr, "gbk");
        if out.status.success() {
            Ok(stdout)
        } else {
            Err(format!("{}{}", stderr, stdout).trim().to_string())
        }
    }

    pub fn start(service_name: &str) -> Result<(), String> {
        match run(&["start", service_name]) {
            Ok(_) => Ok(()),
            Err(e) => {
                if e.contains("1056") || e.contains("already running") || e.contains("正在运行") {
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    pub fn stop(service_name: &str) -> Result<(), String> {
        match run(&["stop", service_name]) {
            Ok(_) => Ok(()),
            Err(e) => {
                if e.contains("1062") || e.contains("not running") || e.contains("未运行") {
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    fn query_state(service_name: &str) -> Option<u32> {
        let out = Command::new("sc")
            .args(["query", service_name])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        parse_state(&out.stdout)
    }

    /// 从 sc query 原始字节里抽 "STATE" 后的数字（ASCII，绕开代码页，且不被其它数字误命中）
    fn parse_state(bytes: &[u8]) -> Option<u32> {
        let keyword = b"STATE";
        let mut i = 0;
        while i + keyword.len() <= bytes.len() {
            if &bytes[i..i + keyword.len()] == keyword {
                let mut j = i + keyword.len();
                while j < bytes.len() && !bytes[j].is_ascii_digit() {
                    j += 1;
                }
                let start = j;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                if j > start {
                    return std::str::from_utf8(&bytes[start..j])
                        .ok()?
                        .parse::<u32>()
                        .ok();
                }
            }
            i += 1;
        }
        None
    }

    pub fn status_running(service_name: &str) -> Option<bool> {
        query_state(service_name).map(|st| st == STATE_RUNNING)
    }

    pub fn stop_and_wait(service_name: &str, timeout: Duration) -> Result<bool, String> {
        stop(service_name)?;
        let start = Instant::now();
        loop {
            match query_state(service_name) {
                None => return Ok(true),
                Some(STATE_STOPPED) => return Ok(true),
                Some(_) => {
                    if start.elapsed() > timeout {
                        return Ok(false);
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }
}

// ============================ Linux (systemctl) ============================
#[cfg(target_os = "linux")]
mod imp {
    use super::*;

    fn run(args: &[&str]) -> Result<String, String> {
        let out = Command::new("systemctl")
            .args(args)
            .output()
            .map_err(|e| format!("调用 systemctl 失败: {e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        // systemctl start 对已运行服务返回 0；is-active 对 inactive 返回非 0 但会打印状态，
        // 所以状态判定看 stdout 文本而非退出码。
        if out.status.success() {
            Ok(stdout)
        } else {
            Err(format!("{}{}", stderr, stdout).trim().to_string())
        }
    }

    pub fn start(service_name: &str) -> Result<(), String> {
        run(&["start", service_name]).map(|_| ())
    }

    pub fn stop(service_name: &str) -> Result<(), String> {
        run(&["stop", service_name]).map(|_| ())
    }

    pub fn status_running(service_name: &str) -> Option<bool> {
        let out = Command::new("systemctl")
            .args(["is-active", service_name])
            .output()
            .ok()?;
        match String::from_utf8_lossy(&out.stdout).trim() {
            "active" => Some(true),
            "inactive" | "failed" | "deactivating" | "activating" | "reloading" => Some(false),
            _ => None,
        }
    }

    pub fn stop_and_wait(service_name: &str, timeout: Duration) -> Result<bool, String> {
        stop(service_name)?;
        let start = Instant::now();
        loop {
            match status_running(service_name) {
                Some(false) | None => return Ok(true),
                Some(true) => {
                    if start.elapsed() > timeout {
                        return Ok(false);
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }
}

// ============================ macOS (launchctl) ============================
#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    fn run(args: &[&str]) -> Result<String, String> {
        let out = Command::new("launchctl")
            .args(args)
            .output()
            .map_err(|e| format!("调用 launchctl 失败: {e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        if out.status.success() {
            Ok(stdout)
        } else {
            Err(format!("{}{}", stderr, stdout).trim().to_string())
        }
    }

    // launchctl 的 label 即服务标识（plist 的 Label）。start/stop 用经典动词，best-effort。
    pub fn start(service_name: &str) -> Result<(), String> {
        run(&["start", service_name]).map(|_| ())
    }

    pub fn stop(service_name: &str) -> Result<(), String> {
        run(&["stop", service_name]).map(|_| ())
    }

    /// launchctl list <label> 退出码 0 表示该 job 已加载；再看 PID 是否非 0 判定运行中。
    pub fn status_running(service_name: &str) -> Option<bool> {
        let out = Command::new("launchctl")
            .args(["list", service_name])
            .output()
            .ok()?;
        if !out.status.success() {
            return Some(false); // 未加载
        }
        let s = String::from_utf8_lossy(&out.stdout);
        // 输出含 "PID" = 有进程号 -> 运行中
        Some(s.contains("PID"))
    }

    pub fn stop_and_wait(service_name: &str, timeout: Duration) -> Result<bool, String> {
        stop(service_name)?;
        let start = Instant::now();
        loop {
            match status_running(service_name) {
                Some(false) | None => return Ok(true),
                Some(true) => {
                    if start.elapsed() > timeout {
                        return Ok(false);
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }
}
