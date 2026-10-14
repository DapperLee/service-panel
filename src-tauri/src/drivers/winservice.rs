//! 系统服务驱动，按平台分派：
//!   Windows -> sc (start/stop/query)
//!   Linux   -> systemctl，系统级失败（权限/查无单元）自动回落 systemctl --user
//!   macOS   -> launchctl 新式 bootstrap/kickstart/bootout/print + gui/<uid> 目标
//! 对外统一暴露 start / stop / status_running / stop_and_wait。
//! 注意：Linux/macOS 分支无法在 Windows 上编译验证，请在对应真机上验证后再用于生产。
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

    fn run_at(user_scope: bool, args: &[&str]) -> Result<String, String> {
        let mut cmd = Command::new("systemctl");
        if user_scope {
            cmd.arg("--user");
        }
        let out = cmd
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

    /// 系统级报「没权限 / 单元不存在」时才回落去试用户级；其它错误原样返回。
    /// （用户级 session bus 不存在时 --user 自己会报错，两个信息都回给用户）
    fn worth_retry_user(e: &str) -> bool {
        let l = e.to_lowercase();
        l.contains("access denied")
            || l.contains("permission")
            || l.contains("authentication")
            || l.contains("not found")
    }

    fn action(user_scope: bool, verb: &str, service_name: &str) -> Result<(), String> {
        run_at(user_scope, &[verb, service_name]).map(|_| ())
    }

    fn dual(verb: &str, service_name: &str) -> Result<(), String> {
        action(false, verb, service_name).or_else(|e| {
            if worth_retry_user(&e) {
                action(true, verb, service_name)
                    .map_err(|e2| format!("系统级: {e}；用户级也失败: {e2}"))
            } else {
                Err(e)
            }
        })
    }

    pub fn start(service_name: &str) -> Result<(), String> {
        dual("start", service_name)
    }

    pub fn stop(service_name: &str) -> Result<(), String> {
        dual("stop", service_name)
    }

    fn probe(user_scope: bool, service_name: &str) -> Option<bool> {
        let out = {
            let mut cmd = Command::new("systemctl");
            if user_scope {
                cmd.arg("--user");
            }
            cmd.args(["is-active", service_name])
                .output()
                .ok()?
        };
        match String::from_utf8_lossy(&out.stdout).trim() {
            "active" => Some(true),
            "inactive" | "failed" | "deactivating" | "activating" | "reloading" => Some(false),
            _ => None,
        }
    }

    pub fn status_running(service_name: &str) -> Option<bool> {
        let system = probe(false, service_name);
        if system == Some(true) {
            return Some(true);
        }
        let user = probe(true, service_name);
        if user == Some(true) {
            return Some(true);
        }
        // 两边都没在跑：优先报系统级的结论；系统级查无此单元(None)才看用户级
        system.or(user)
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

    extern "C" {
        fn getuid() -> u32;
    }

    /// 只管用户自己的 gui 域（LaunchAgents）。系统域 LaunchDaemon 的增停需要 root，
    /// 经典动词（start/stop/list <label>）在新 macOS 上对 gui 会话经常静默失败，
    /// 因此统一走 bootstrap / kickstart / bootout / print + `gui/<uid>` 目标。
    fn domain() -> String {
        format!("gui/{}", unsafe { getuid() })
    }

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

    /// job 不存在的报错形态（kickstart/bootout 措辞不同，都算"没加载"）
    fn job_missing(e: &str) -> bool {
        e.contains("Could not find service") || e.contains("No such process")
    }

    /// 按 launchd 约定（plist 文件名 == Label）在用户可见的 LaunchAgents 目录找文件。
    /// 只搜 Agents 目录：Daemons 的 plist 拿到 gui 域也 bootstrap 不起来，不该假装能管。
    fn agent_plist(label: &str) -> Option<String> {
        let mut dirs: Vec<String> = Vec::new();
        if let Ok(home) = std::env::var("HOME") {
            dirs.push(format!("{home}/Library/LaunchAgents"));
        }
        dirs.push("/Library/LaunchAgents".to_string());
        let fname = format!("{label}.plist");
        dirs.iter()
            .map(|d| format!("{d}/{fname}"))
            .find(|p| std::path::Path::new(p).exists())
    }

    /// `launchctl print` 输出里判运行：pid 非 0，或 state = running（过渡态）
    fn parse_running(s: &str) -> bool {
        for line in s.lines() {
            let t = line.trim();
            if let Some(v) = t.strip_prefix("pid = ") {
                if v.trim() != "0" {
                    return true;
                }
            }
        }
        s.lines().any(|l| l.trim().starts_with("state = running"))
    }

    /// `launchctl print-disabled <dom>` 输出里查该 label 是否被禁用。
    /// disabled 服务的 bootstrap 报 `Bootstrap failed: 5: Input/output error`，
    /// 报错文本不含 "disabled" 字样，只能查这张表确认（E6 场景）。
    fn launchd_disabled(label: &str) -> bool {
        Command::new("launchctl")
            .args(["print-disabled", &domain()])
            .output()
            .map(|o| {
                let s = format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                );
                s.contains(&format!("\"{label}\" => disabled"))
            })
            .unwrap_or(false)
    }

    pub fn start(service_name: &str) -> Result<(), String> {
        let dom = domain();
        let target = format!("{dom}/{service_name}");
        match run(&["kickstart", &target]) {
            Ok(_) => Ok(()),
            Err(e) if e.contains("already running") => Ok(()),
            Err(e) if job_missing(&e) => {
                let plist = agent_plist(service_name).ok_or_else(|| {
                    format!(
                        "启动失败：LaunchAgents 目录里没找到 {service_name}.plist；\
                         若它是系统域 LaunchDaemon，本面板不支持免提权管理"
                    )
                })?;
                // bootstrap 失败必须接住：disabled 服务的真实死因在这里
                // （"Bootstrap failed: 5: Input/output error"，不含 "disabled" 字样），
                // 吞掉就只剩 kickstart 的 "Could not find service"，没法给用户提示
                let boot_err = run(&["bootstrap", &dom, &plist]).err();
                match run(&["kickstart", &target]) {
                    Ok(_) => Ok(()),
                    Err(e2) if e2.contains("disabled") => Err(format!(
                        "{e2}（该服务被禁用，先执行: launchctl enable {target}）"
                    )),
                    // kickstart 报错不带 "disabled"，但 launchd 禁用表里有它：E6 场景
                    Err(_) if launchd_disabled(service_name) => {
                        let why = boot_err.as_deref().unwrap_or("kickstart 被拒");
                        Err(format!(
                            "启动失败：{service_name} 已被 launchd 禁用（{why}）；\
                             先执行: launchctl enable {target} 再启动"
                        ))
                    }
                    // bootstrap 的报错（plist 路径/格式问题）比 kickstart 的
                    // "Could not find service" 更有定位价值
                    Err(e2) => Err(boot_err.unwrap_or(e2)),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// stop 用 bootout（卸载并终止）：对 KeepAlive 的 Agent，经典 stop 会被立即拉回，
    /// bootout 才是真的停。start 会负责重新 bootstrap。
    pub fn stop(service_name: &str) -> Result<(), String> {
        let target = format!("{}/{}", domain(), service_name);
        match run(&["bootout", &target]) {
            Ok(_) => Ok(()),
            Err(e) if job_missing(&e) => {
                // gui 域没有但系统域有：那不是"已停止"，是没权限停，必须说清楚
                if run(&["print", &format!("system/{service_name}")]).is_ok() {
                    Err(format!(
                        "{service_name} 是系统域 LaunchDaemon，停止它需要管理员权限，面板暂不支持"
                    ))
                } else {
                    Ok(()) // 本来就没在跑
                }
            }
            Err(e) => Err(e),
        }
    }

    pub fn status_running(service_name: &str) -> Option<bool> {
        let target = format!("{}/{}", domain(), service_name);
        if let Ok(out) = run(&["print", &target]) {
            return Some(parse_running(&out));
        }
        // 系统域 job：print 要权限，经典只读 list 对 LaunchDaemon 仍可用
        let out = Command::new("launchctl")
            .args(["list", service_name])
            .output()
            .ok()?;
        if !out.status.success() {
            return Some(false);
        }
        Some(String::from_utf8_lossy(&out.stdout).contains("PID"))
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
