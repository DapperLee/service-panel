use std::process::Command;
use std::time::{Duration, Instant};

// Windows 服务控制状态码（SERVICE_*），数字是 ASCII，与系统语言无关
const STATE_STOPPED: u32 = 1;
#[allow(dead_code)]
const STATE_START_PENDING: u32 = 2;
#[allow(dead_code)]
const STATE_STOP_PENDING: u32 = 3;
const STATE_RUNNING: u32 = 4;

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("调用 {program} 失败: {e}"))?;
    // sc 在中文 Windows 上输出 OEM(936/GBK)，用 GBK 解码避免中文错误信息乱码
    let stdout = crate::logs::decode_text(&out.stdout, "gbk");
    let stderr = crate::logs::decode_text(&out.stderr, "gbk");
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(format!("{}{}", stderr, stdout).trim().to_string())
    }
}

pub fn start(service_name: &str) -> Result<(), String> {
    match run("sc", &["start", service_name]) {
        Ok(_) => Ok(()),
        Err(e) => {
            // 1056 = 服务已在运行
            if e.contains("1056") || e.contains("already running") || e.contains("正在运行") {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

/// 查询服务当前 STATE 数字码；None 表示服务不存在/无权限/查询失败。
pub fn query_state(service_name: &str) -> Option<u32> {
    let out = Command::new("sc")
        .args(["query", service_name])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_state(&out.stdout)
}

/// 从 sc query 的原始字节里抽 "STATE" 后紧跟的数字。
/// 数字是 ASCII，绕开控制台代码页问题；比 contains("4") 可靠（不会被 exit code 等其它数字误命中）。
fn parse_state(bytes: &[u8]) -> Option<u32> {
    let keyword = b"STATE";
    let mut i = 0;
    while i + keyword.len() <= bytes.len() {
        if &bytes[i..i + keyword.len()] == keyword {
            let mut j = i + keyword.len();
            // 跳到第一个数字
            while j < bytes.len() && !bytes[j].is_ascii_digit() {
                j += 1;
            }
            // 收集连续数字
            let start = j;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > start {
                let s = std::str::from_utf8(&bytes[start..j]).ok()?;
                return s.parse::<u32>().ok();
            }
        }
        i += 1;
    }
    None
}

/// 是否正在运行（严格：STATE == 4）
pub fn status_running(service_name: &str) -> Option<bool> {
    query_state(service_name).map(|st| st == STATE_RUNNING)
}

pub fn stop(service_name: &str) -> Result<(), String> {
    match run("sc", &["stop", service_name]) {
        Ok(_) => Ok(()),
        Err(e) => {
            // 1062 = 未在运行
            if e.contains("1062") || e.contains("not running") || e.contains("未运行") {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

/// 发送停止并**轮询到真正停止**（sc stop 是异步的，服务可能 STOP_PENDING 十几秒，如 MySQL）。
/// 返回 Ok(true)=已停 / Ok(false)=超时仍在停 / Err=命令失败。
pub fn stop_and_wait(service_name: &str, timeout: Duration) -> Result<bool, String> {
    stop(service_name)?;
    let start = Instant::now();
    loop {
        match query_state(service_name) {
            None => return Ok(true), // 服务已消失，视为已停
            Some(STATE_STOPPED) => return Ok(true),
            Some(other) => {
                if start.elapsed() > timeout {
                    return Ok(false); // 超时仍在 STOP_PENDING(3)/RUNNING(4)
                }
                let _ = other;
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}
