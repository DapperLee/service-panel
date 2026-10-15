use crate::logs::{LogEmitter, Sink};
use crate::types::ServiceSpec;
use std::process::{Child, Command, Stdio};

#[cfg(windows)]
use crate::winjob::Job;

/// 对外暴露 Job 句柄类型（供 state.rs 的 LiveProc 使用；仅 Windows 路径引用）
#[cfg(windows)]
pub use crate::winjob::Job as JobHandle;

/// Windows：让 sc/docker/cmd 这类辅助命令不弹控制台窗口。
/// 打包版是 GUI 进程、自己没有控制台，spawn 子进程时默认会各开一个新控制台窗口
/// 再随退出销毁——监督线程每轮都查询，用户看到的就是屏幕不停闪黑框。
pub fn hide_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// unix：向子进程所在**进程组**发信号（负 pid），回收它派生的整棵进程树。
/// 面板 spawn 时用 process_group(0) 让 pgid == 子进程 pid，故 kill(-pid) 命中整组。
/// force=false 发 SIGTERM（优雅），true 发 SIGKILL（强杀）。用 /bin/kill 避免引入 libc 依赖。
#[cfg(unix)]
pub fn kill_group(pid: u32, force: bool) {
    let sig = if force { "-KILL" } else { "-TERM" };
    let _ = Command::new("kill").args([sig, &format!("-{pid}")]).status();
}

/// 已启动的托管进程
pub struct SpawnedProc {
    pub child: Child,
    pub pid: u32,
    #[cfg(windows)]
    pub job: Option<Job>,
    /// unix 看门狗（自身 exe 的 --guard 副本），随 LiveProc 存亡：
    /// 面板正常停止/退出/重启该服务前必须先杀掉它，再动进程组。
    #[cfg(unix)]
    pub guard: Option<Child>,
}

/// 拉起看护进程：面板死亡（getppid 变化）后由它 kill(-pgid) 兜底。
/// mac 没有 PR_SET_PDEATHSIG，只能靠这个独立进程；Linux 上也顺带覆盖孙进程。
#[cfg(unix)]
fn spawn_guard(pgid: u32) -> Result<Child, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe)
        .args([
            "--guard",
            &std::process::id().to_string(),
            &pgid.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn 看门狗失败: {e}"))
}

/// 启动前的命令校验。空命令必须直接失败：否则会落到下面 `cmd /C` 那条回退分支，
/// 而 `cmd /C ""` 能成功 spawn 并立刻退出，被当成「启动成功」之后就陷入
/// 假启动 -> 崩溃检测 -> 指数退避 -> 长冷却 -> 再假启动 的死循环。
pub fn validate(spec: &ServiceSpec) -> Result<(), String> {
    if spec.command.trim().is_empty() {
        return Err("命令为空，无法启动：请填写可执行文件或脚本路径".to_string());
    }
    Ok(())
}

/// 启动命令型服务：spawn + 立即挂到 Job Object + 起日志读取线程
pub fn spawn(
    spec: &ServiceSpec,
    service_id: &str,
    sink: Sink,
    emitter: LogEmitter,
) -> Result<SpawnedProc, String> {
    validate(spec)?;
    // 清洗：用户常从别处复制粘贴，带前导/尾随空格会让 CreateProcess 报
    // “目录名称无效 (os error 267)” 或找不到程序。这里统一 trim。
    let command = spec.command.trim().to_string();
    let cwd = spec.cwd.trim().to_string();
    let args: Vec<String> = spec
        .args
        .iter()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty())
        .map(|a| a.to_string())
        .collect();

    let mut cmd = Command::new(&command);
    cmd.args(&args);
    if !cwd.is_empty() {
        cmd.current_dir(&cwd);
    }
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    // unix：把子进程放进独立进程组（pgid == 子进程 pid），停止时用 kill(-pgid) 回收整棵树。
    // Linux 额外设 PDEATHSIG，面板崩溃时内核会给子进程发 SIGTERM（macOS 无此机制，见 README 说明）。
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        #[cfg(target_os = "linux")]
        unsafe {
            cmd.pre_exec(|| {
                const PR_SET_PDEATHSIG: i32 = 1;
                const SIGTERM: i32 = 15;
                // prctl 来自 libc，通过 extern 声明避免额外依赖
                extern "C" {
                    fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32;
                }
                let _ = prctl(PR_SET_PDEATHSIG, SIGTERM as u64, 0, 0, 0);
                Ok(())
            });
        }
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        #[cfg(windows)]
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // 回退：很多命令在 Windows 上是 .cmd/.bat（npm、yarn、pnpm、conda…），
            // 或用户把「命令 + 参数」整体填进了命令框。用 cmd /C 走一遍 PATH 查找。
            let joined = if args.is_empty() {
                command.clone()
            } else {
                format!("{} {}", command, args.join(" "))
            };
            let mut c2 = Command::new("cmd");
            c2.args(["/C", &joined]);
            if !cwd.is_empty() {
                c2.current_dir(&cwd);
            }
            for (k, v) in &spec.env {
                c2.env(k, v);
            }
            c2.stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .stdin(Stdio::null());
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            c2.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
            c2.spawn()
                .map_err(|e2| format!("启动失败（直接执行与 cmd 回退均失败）: {e} / {e2}"))?
        }
        Err(e) => return Err(format!("启动失败: {e}")),
    };
    let pid = child.id();

    #[cfg(windows)]
    let job = {
        let j = Job::new_kill_on_close().map_err(|e| format!("Job Object 创建失败: {e}"))?;
        j.assign(&child)
            .map_err(|e| format!("Job Object 绑定失败: {e}"))?;
        Some(j)
    };

    // unix：立刻为这个进程组拉起看门狗。spawn 失败不阻断启动，但要把降级
    // 结果说清楚（面板崩溃时该服务不会被自动回收）。
    #[cfg(unix)]
    let guard = spawn_guard(pid);

    // 起两个后台线程读 stdout/stderr（管道缓冲区写满会阻塞子进程，必须持续消费）
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法获取 stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "无法获取 stderr".to_string())?;

    crate::logs::system_message(
        &format!("[panel] 进程已启动 pid={pid}"),
        service_id,
        &emitter,
        &sink,
    );

    #[cfg(unix)]
    let guard = match guard {
        Ok(c) => Some(c),
        Err(e) => {
            crate::logs::system_message(
                &format!("[panel] 看门狗启动失败: {e}（面板崩溃时本服务不会被自动回收）"),
                service_id,
                &emitter,
                &sink,
            );
            None
        }
    };

    crate::logs::spawn_reader(
        stdout,
        spec.encoding.clone(),
        "stdout",
        service_id.to_string(),
        emitter.clone(),
        sink.clone(),
    );
    crate::logs::spawn_reader(
        stderr,
        spec.encoding.clone(),
        "stderr",
        service_id.to_string(),
        emitter,
        sink.clone(),
    );

    Ok(SpawnedProc {
        child,
        pid,
        #[cfg(windows)]
        job,
        #[cfg(unix)]
        guard,
    })
}

#[cfg(test)]
mod tests {
    use super::validate;
    use crate::types::ServiceSpec;

    fn spec_with_command(cmd: &str) -> ServiceSpec {
        ServiceSpec {
            command: cmd.to_string(),
            ..Default::default()
        }
    }

    /// 空命令若在 spawn 里放过，就会落到 `cmd /C ""` 回退：那条命令能成功 spawn
    /// 并立刻退出，于是被当成启动成功，陷入假启动崩溃循环。
    #[test]
    fn empty_command_is_rejected() {
        let err = validate(&spec_with_command("")).err().unwrap();
        assert!(err.contains("命令为空"), "实际错误: {err}");
    }

    #[test]
    fn whitespace_only_command_is_rejected() {
        let err = validate(&spec_with_command("   ")).err().unwrap();
        assert!(err.contains("命令为空"), "实际错误: {err}");
    }

    #[test]
    fn real_command_passes_validation() {
        assert!(validate(&spec_with_command("  python  ")).is_ok());
    }
}
