//! 外部服务（Docker / Windows 服务）的日志接入。
//! - Docker：spawn 一个 `docker logs -f` 子进程，复用现有日志管线读取；挂 Job Object。
//! - Windows 服务 / 命令型（可选）：尾随 spec.log_path 指向的日志文件，增量读取。
use crate::logs::{self, LogFile};
use crate::state::{AppState, LogTail};
use crate::types::DriverKind;
use std::io::{Read, Seek, SeekFrom};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 为服务挂载日志源（幂等：先卸再挂）
pub fn attach(state: &Arc<AppState>, id: &str) {
    let spec = match state.runtimes.lock().unwrap().get(id) {
        Some(r) => r.spec.clone(),
        None => return,
    };
    detach(state, id);
    match spec.driver {
        DriverKind::Docker => {
            if spec.log_path.trim().is_empty() {
                start_docker_tail(state, id, &spec);
            } else {
                start_file_tail(state, id, &spec);
            }
        }
        DriverKind::WindowsService => {
            if !spec.log_path.trim().is_empty() {
                start_file_tail(state, id, &spec);
            }
        }
        DriverKind::Command => {
            // 命令型主输出已走 stdout/stderr 管道；额外配了 log_path 才尾随文件
            if !spec.log_path.trim().is_empty() {
                start_file_tail(state, id, &spec);
            }
        }
    }
}

/// 卸载日志源：停文件尾随线程 + 杀 docker 尾随子进程
pub fn detach(state: &Arc<AppState>, id: &str) {
    let (tail, file_stop) = {
        let mut rts = state.runtimes.lock().unwrap();
        match rts.get_mut(id) {
            Some(r) => (r.tail.take(), r.file_stop.take()),
            None => (None, None),
        }
    };
    if let Some(stop) = file_stop {
        stop.store(false, Ordering::SeqCst);
    }
    if let Some(mut t) = tail {
        let _ = t.child.kill();
        let _ = t.child.wait();
        // drop(t.job) -> Job 句柄关闭 -> docker logs 子进程被系统回收
    }
}

fn start_docker_tail(state: &Arc<AppState>, id: &str, spec: &crate::types::ServiceSpec) {
    let emitter = match state.current_emitter() {
        Some(e) => e,
        None => return,
    };
    let sink = match LogFile::create(&state.log_dir(id)) {
        Ok(l) => l.writer(),
        Err(_) => return,
    };

    let mut c = Command::new("docker");
    c.args(["logs", "-f", "--tail", "200", "--timestamps", &spec.container]);
    c.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => {
            logs::emit_line(
                &emitter,
                &sink,
                id,
                "system",
                &format!("[panel] 无法启动 docker logs（Docker 是否在运行？）: {e}"),
            );
            return;
        }
    };

    #[cfg(windows)]
    let job = {
        match crate::winjob::Job::new_kill_on_close() {
            Ok(j) => {
                let _ = j.assign(&child);
                Some(j)
            }
            Err(_) => None,
        }
    };

    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => return,
    };
    let stderr = match child.stderr.take() {
        Some(s) => s,
        None => return,
    };
    logs::spawn_reader(
        stdout,
        spec.encoding.clone(),
        "stdout",
        id.to_string(),
        emitter.clone(),
        sink.clone(),
    );
    logs::spawn_reader(
        stderr,
        spec.encoding.clone(),
        "stderr",
        id.to_string(),
        emitter,
        sink,
    );

    let mut rts = state.runtimes.lock().unwrap();
    if let Some(r) = rts.get_mut(id) {
        r.tail = Some(LogTail {
            child,
            #[cfg(windows)]
            job,
        });
    }
}

fn start_file_tail(state: &Arc<AppState>, id: &str, spec: &crate::types::ServiceSpec) {
    let emitter = match state.current_emitter() {
        Some(e) => e,
        None => return,
    };
    let sink = match LogFile::create(&state.log_dir(id)) {
        Ok(l) => l.writer(),
        Err(_) => return,
    };
    let path = spec.log_path.trim().to_string();
    let stop = Arc::new(AtomicBool::new(true));
    {
        let mut rts = state.runtimes.lock().unwrap();
        if let Some(r) = rts.get_mut(id) {
            r.file_stop = Some(stop.clone());
        }
    }
    let enc = spec.encoding.clone();
    let sid = id.to_string();

    std::thread::spawn(move || {
        let mut offset: u64 = 0;
        let mut primed = false;
        while stop.load(Ordering::SeqCst) {
            match std::fs::metadata(&path) {
                Ok(meta) => {
                    let size = meta.len();
                    let mut drop_partial = false;
                    if !primed {
                        // 首次只回看最后 ~64KB，避免把历史全量刷出来
                        offset = size.saturating_sub(64 * 1024);
                        primed = true;
                        drop_partial = offset > 0;
                    }
                    if size < offset {
                        offset = 0; // 轮转/截断
                    }
                    if size > offset {
                        if let Ok(mut f) = std::fs::File::open(&path) {
                            if f.seek(SeekFrom::Start(offset)).is_ok() {
                                let mut buf = Vec::new();
                                if f.read_to_end(&mut buf).is_ok() {
                                    offset = size;
                                    let mut lines: Vec<&[u8]> =
                                        buf.split(|&b| b == b'\n').collect();
                                    if drop_partial {
                                        // 丢掉从 64KB 边界切进来的半行
                                        if !lines.is_empty() {
                                            lines.remove(0);
                                        }
                                    }
                                    for line in lines {
                                        if line.is_empty() {
                                            continue;
                                        }
                                        let text = logs::decode_text(line, &enc);
                                        logs::emit_line(&emitter, &sink, &sid, "stdout", &text);
                                    }
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    // 文件还没生成（服务尚未写日志），继续等
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1000));
        }
    });
}
