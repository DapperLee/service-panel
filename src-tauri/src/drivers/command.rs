use crate::logs::LogFile;
use crate::types::ServiceSpec;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::path::Path;

#[cfg(windows)]
use crate::winjob::Job;

/// 对外暴露 Job 句柄类型（供 state.rs 的 LiveProc 使用）
#[cfg(windows)]
pub use crate::winjob::Job as JobHandle;
#[cfg(not(windows))]
pub type JobHandle = ();

/// 已启动的托管进程
pub struct SpawnedProc {
    pub child: Child,
    pub pid: u32,
    #[cfg(windows)]
    pub job: Option<Job>,
}

/// 启动命令型服务：spawn + 立即挂到 Job Object + 起日志读取线程
pub fn spawn(
    spec: &ServiceSpec,
    service_id: &str,
    log_dir: &Path,
    emitter: Arc<dyn Fn(crate::logs::LogEvent) + Send + Sync>,
) -> Result<SpawnedProc, String> {
    let log = LogFile::create(log_dir).map_err(|e| format!("无法创建日志文件: {e}"))?;
    let sink = log.writer();

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
    })
}
