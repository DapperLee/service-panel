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
        let mut cmd = Command::new("sc");
        cmd.args(args);
        crate::drivers::command::hide_window(&mut cmd);
        let out = cmd.output().map_err(|e| format!("调用 sc 失败: {e}"))?;
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
                if e.contains("1056") || e.contains("already running") || e.contains("正在运行")
                {
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
        let mut cmd = Command::new("sc");
        cmd.args(["query", service_name]);
        crate::drivers::command::hide_window(&mut cmd);
        let out = cmd.output().ok()?;
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

    /// 服务状态与宿主进程号。
    ///
    /// 不走 `sc query`：那要起子进程，而面板每 2 秒就要问一轮，且 `sc query` 的输出里
    /// 根本没有 PID（这正是外部服务 CPU/内存恒为 0 的原因）。`QueryServiceStatusEx`
    /// 一次调用同时拿到状态与宿主进程号。
    mod ffi {
        use std::ffi::c_void;

        const SC_MANAGER_CONNECT: u32 = 0x0001;
        const SERVICE_QUERY_STATUS: u32 = 0x0004;
        /// QueryServiceStatusEx 唯一支持的信息级别，即取 SERVICE_STATUS_PROCESS
        const SC_STATUS_PROCESS_INFO: u32 = 0;

        #[repr(C)]
        #[allow(non_snake_case)]
        // 布局由内核定义，字段一个都不能省，哪怕只用其中两个
        #[allow(dead_code)]
        struct ServiceStatusProcess {
            dwServiceType: u32,
            dwCurrentState: u32,
            dwControlsAccepted: u32,
            dwWin32ExitCode: u32,
            dwServiceExitCode: u32,
            dwCheckPoint: u32,
            dwWaitHint: u32,
            dwProcessId: u32,
            dwServiceFlags: u32,
        }

        #[link(name = "advapi32")]
        extern "system" {
            fn OpenSCManagerW(
                machine: *const u16,
                database: *const u16,
                access: u32,
            ) -> *mut c_void;
            fn OpenServiceW(scm: *mut c_void, name: *const u16, access: u32) -> *mut c_void;
            fn QueryServiceStatusEx(
                service: *mut c_void,
                level: u32,
                buf: *mut u8,
                buf_size: u32,
                needed: *mut u32,
            ) -> i32;
            fn CloseHandle(obj: *mut c_void) -> i32;
        }

        fn wide(s: &str) -> Vec<u16> {
            s.encode_utf16().chain(std::iter::once(0)).collect()
        }

        /// 返回 (dwCurrentState, dwProcessId)。
        ///
        /// 没有顺带返回「是否共享宿主」：`dwServiceType` 的
        /// SERVICE_WIN32_SHARE_PROCESS 位在本机上实测不可信（MySQL84、Spooler 这类
        /// 独占进程服务也带该位，RpcSs 反而不带），拿它过滤会把正常服务误判成测不到。
        /// 是不是 svchost 承载，交给采样方按进程名判。
        pub fn query(service_name: &str) -> Option<(u32, u32)> {
            let name = wide(service_name);
            let mut st: ServiceStatusProcess = unsafe { std::mem::zeroed() };
            let mut needed: u32 = 0;
            unsafe {
                let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
                if scm.is_null() {
                    return None;
                }
                let svc = OpenServiceW(scm, name.as_ptr(), SERVICE_QUERY_STATUS);
                CloseHandle(scm);
                if svc.is_null() {
                    return None;
                }
                let ok = QueryServiceStatusEx(
                    svc,
                    SC_STATUS_PROCESS_INFO,
                    &mut st as *mut ServiceStatusProcess as *mut u8,
                    std::mem::size_of::<ServiceStatusProcess>() as u32,
                    &mut needed,
                );
                CloseHandle(svc);
                if ok == 0 {
                    return None;
                }
            }
            Some((st.dwCurrentState, st.dwProcessId))
        }
    }

    pub fn status_full(service_name: &str) -> crate::drivers::ExtStatus {
        let mut st = match ffi::query(service_name) {
            Some((state, pid)) => crate::drivers::ExtStatus {
                running: Some(state == STATE_RUNNING),
                // 过渡态（START_PENDING 等）的进程号还是 0，不给
                pid: if pid == 0 { None } else { Some(pid) },
                metrics: None,
            },
            // FFI 失败（权限受限、非标准服务名等）回落 sc query：状态判定不能因为
            // 顺带要 PID 就退化，那会让崩溃检测整条链路失效
            None => crate::drivers::ExtStatus {
                running: query_state(service_name).map(|st| st == STATE_RUNNING),
                pid: None,
                metrics: None,
            },
        };
        if st.running != Some(true) {
            st.pid = None;
        }
        st
    }

    pub fn status_running(service_name: &str) -> Option<bool> {
        status_full(service_name).running
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

    #[cfg(test)]
    mod tests {
        use super::status_full;

        /// 查无此服务必须报「未知」而不是「已停止」：后者会让面板把探测失败当成服务崩了，
        /// 进而对着一个可能根本存在的服务反复重启
        #[test]
        fn missing_service_is_unknown_not_stopped() {
            let st = status_full("sp-definitely-not-a-real-service");
            assert_eq!(st.running, None);
            assert_eq!(st.pid, None);
        }

        /// RpcSs 每台 Windows 都在跑，且由 svchost 承载：验证状态与宿主进程号都读得对。
        /// （共享宿主的过滤在采样侧，见 supervisor::is_shared_host）
        #[test]
        fn running_service_reports_host_pid() {
            let st = status_full("RpcSs");
            assert_eq!(st.running, Some(true), "FFI 读出的状态不对，调用链有问题");
            let pid = st.pid.expect("运行中的服务应给出宿主 PID");
            let name = process_name(pid);
            assert!(
                name.contains("svchost"),
                "RpcSs 的宿主进程应是 svchost，实际是 {name}（PID {pid}）"
            );
        }

        /// 独占进程型服务（Spooler = spoolsv.exe）必须给出真实存在的 PID，
        /// 否则外部服务的资源占用又会退回恒为 0。
        /// 该服务在部分 Windows 上被设为手动启动，未运行时只校验「状态查得通」。
        #[test]
        fn own_process_service_pid_resolves_to_that_process() {
            let st = status_full("Spooler");
            assert!(st.running.is_some(), "连状态都查不到，说明 FFI 调用失败");
            if st.running != Some(true) {
                eprintln!("Spooler 未运行（该机器上它是手动启动），跳过 PID 校验");
                return;
            }
            let pid = st.pid.expect("独占进程型服务应给出宿主 PID");
            let name = process_name(pid);
            assert!(name.contains("spoolsv"), "PID {pid} 指向了别的进程: {name}");
        }

        fn process_name(pid: u32) -> String {
            use sysinfo::{Pid, System};
            let mut sys = System::new_all();
            sys.refresh_processes();
            sys.process(Pid::from_u32(pid))
                .unwrap_or_else(|| panic!("PID {pid} 在进程表里找不到"))
                .name()
                .to_lowercase()
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
            cmd.args(["is-active", service_name]).output().ok()?
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

    /// 单元的主进程号。未运行或查不到时 systemd 返回 0，一律折算成 None。
    fn main_pid(user_scope: bool, service_name: &str) -> Option<u32> {
        let out = run_at(
            user_scope,
            &["show", "-p", "MainPID", "--value", service_name],
        )
        .ok()?;
        let pid: u32 = out.trim().parse().ok()?;
        (pid > 0).then_some(pid)
    }

    pub fn status_full(service_name: &str) -> crate::drivers::ExtStatus {
        let running = status_running(service_name);
        // 只在确实运行时才多问一次 PID（--value 需要 systemd 230+，发行版早已普及）
        let pid = if running == Some(true) {
            main_pid(false, service_name).or_else(|| main_pid(true, service_name))
        } else {
            None
        };
        crate::drivers::ExtStatus {
            running,
            pid,
            metrics: None,
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

    /// 同一个 `pid = N` 字段，未启动时是 0。running 判定早就在读它，只是没往外传，
    /// 所以 PID 不需要额外一次 launchctl 调用。
    fn parse_pid(s: &str) -> Option<u32> {
        s.lines()
            .map(str::trim)
            .find_map(|t| t.strip_prefix("pid = "))
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|p| *p > 0)
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

    pub fn status_full(service_name: &str) -> crate::drivers::ExtStatus {
        let target = format!("{}/{}", domain(), service_name);
        if let Ok(out) = run(&["print", &target]) {
            let pid = parse_pid(&out);
            return crate::drivers::ExtStatus {
                running: Some(parse_running(&out)),
                pid,
                metrics: None,
            };
        }
        // 系统域回落路径只报状态：`launchctl list` 的 PID 列对已加载未启动的 job 是 "-"，
        // 拿它当 PID 采样会读到别家的进程，不如留空
        crate::drivers::ExtStatus {
            running: status_running(service_name),
            pid: None,
            metrics: None,
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
