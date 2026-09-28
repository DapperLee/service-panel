//! 外部服务（Docker / Windows 服务）的日志接入。
//! - Docker：spawn 一个 `docker logs -f` 子进程，复用现有日志管线读取；挂 Job Object。
//! - Windows 服务 / 命令型（可选）：尾随 spec.log_path 指向的日志文件，增量读取。
use crate::logs;
use crate::state::{AppState, LogTail};
use crate::types::DriverKind;
use std::io::{Read, Seek, SeekFrom};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

/// 文件尾随的采样间隔。用 recv_timeout 而不是 sleep：detach 时线程能立刻醒来退出，
/// 不会多活一个间隔、与紧随其后的新尾随线程重叠而把同一行推两遍。
const TAIL_POLL: Duration = Duration::from_millis(500);

/// 首次回看的字节数：避免一挂上就把整个历史日志刷进 UI
const TAIL_PRIME_BYTES: u64 = 64 * 1024;

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

/// 卸载日志源：让文件尾随线程**真正退出**（send + join），并杀掉 docker 尾随子进程。
pub fn detach(state: &Arc<AppState>, id: &str) {
    let (tail, stop_tx, join) = {
        let mut rts = state.runtimes.lock().unwrap();
        match rts.get_mut(id) {
            Some(r) => (r.tail.take(), r.tail_stop.take(), r.tail_join.take()),
            None => (None, None, None),
        }
    };
    if let Some(tx) = stop_tx {
        let _ = tx.send(());
    }
    if let Some(h) = join {
        // 线程收到信号后最多再跑一次文件读取就退出，join 因此是有界的。
        // 注意不要在持有 runtimes 锁的地方 join，这里已经取出了句柄。
        let _ = h.join();
    }
    if let Some(mut t) = tail {
        let _ = t.child.kill();
        let _ = t.child.wait();
        // drop(t.job) -> Job 句柄关闭 -> docker logs 子进程被系统回收
    }
}

/// 取该服务的日志写入端与广播闭包；任一不可用就没法挂日志源
fn pipeline(state: &Arc<AppState>, id: &str) -> Option<(logs::LogEmitter, logs::Sink)> {
    let emitter = state.current_emitter()?;
    let sink = state.log_sink(id)?;
    Some((emitter, sink))
}

fn start_docker_tail(state: &Arc<AppState>, id: &str, spec: &crate::types::ServiceSpec) {
    let (emitter, sink) = match pipeline(state, id) {
        Some(p) => p,
        None => return,
    };

    let mut c = Command::new("docker");
    c.args([
        "logs",
        "-f",
        "--tail",
        "200",
        "--timestamps",
        &spec.container,
    ]);
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
    let (emitter, sink) = match pipeline(state, id) {
        Some(p) => p,
        None => return,
    };
    let path = spec.log_path.trim().to_string();
    let (tx, rx) = mpsc::channel::<()>();
    {
        let mut rts = state.runtimes.lock().unwrap();
        if let Some(r) = rts.get_mut(id) {
            r.tail_stop = Some(tx);
        }
    }
    let enc = spec.encoding.clone();
    let sid = id.to_string();

    let handle = std::thread::spawn(move || {
        let mut offset: u64 = 0;
        let mut primed = false;
        loop {
            // 收到 detach 的信号、或 Sender 被丢弃，都立即退出
            match rx.recv_timeout(TAIL_POLL) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let size = match std::fs::metadata(&path) {
                Ok(m) => m.len(),
                Err(_) => continue, // 文件还没生成（服务尚未写日志），继续等
            };
            let mut drop_partial = false;
            if !primed {
                // 首次只回看最后 ~64KB，避免把历史全量刷出来
                offset = size.saturating_sub(TAIL_PRIME_BYTES);
                primed = true;
                drop_partial = offset > 0;
            }
            if size < offset {
                offset = 0; // 轮转/截断
            }
            if size <= offset {
                continue; // 无新增内容
            }
            let Ok(mut f) = std::fs::File::open(&path) else {
                continue;
            };
            if f.seek(SeekFrom::Start(offset)).is_err() {
                continue;
            }
            let mut buf = Vec::new();
            if f.read_to_end(&mut buf).is_err() {
                continue;
            }
            // 按**实际读到的字节数**推进偏移。旧代码写的是 `offset = size`，而 size
            // 是先前 metadata 取到的值：读取期间文件又涨了的话，下一轮会从 size 重新
            // seek，把上一轮已经读过的尾部再读一遍，表现为重复日志行。
            offset += buf.len() as u64;

            let mut lines: Vec<&[u8]> = buf.split(|&b| b == b'\n').collect();
            if drop_partial && !lines.is_empty() {
                // 丢掉从 64KB 边界切进来的半行
                lines.remove(0);
            }
            for line in lines {
                if line.is_empty() {
                    continue;
                }
                let text = logs::decode_text(line, &enc);
                logs::emit_line(&emitter, &sink, &sid, "stdout", &text);
            }
        }
    });

    let mut rts = state.runtimes.lock().unwrap();
    if let Some(r) = rts.get_mut(id) {
        r.tail_join = Some(handle);
    }
}
