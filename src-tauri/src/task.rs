//! 一次性任务：串行队列 worker。同一时刻只跑一个构建，其余排队。
//! 复用 CommandDriver 的思路（spawn + 日志管线 + Job Object/进程组），
//! 但生命周期是"跑到退出即结束"，每次运行记一条 task_runs。
use crate::db;
use crate::logs::{self, LogSink};
use crate::state::AppState;
use crate::types::{State, TaskRunRequest, TaskRunStatus};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(windows)]
use crate::winjob::Job;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(windows)]
fn shell_prog() -> &'static str {
    "cmd"
}
#[cfg(windows)]
fn shell_flag() -> &'static str {
    "/C"
}
#[cfg(not(windows))]
fn shell_prog() -> &'static str {
    "sh"
}
#[cfg(not(windows))]
fn shell_flag() -> &'static str {
    "-c"
}

/// 启动任务队列 worker：可配并发数（task_max_parallel，1=串行）。
/// 调度线程从 channel 取请求，占一个并发槽后 spawn 一个运行线程；槽满则等待。
pub fn spawn_worker(state: Arc<AppState>) {
    let (tx, rx) = std::sync::mpsc::channel::<TaskRunRequest>();
    *state.task_tx.lock().unwrap() = Some(tx);
    std::thread::spawn(move || {
        // 计数信号量：活跃运行数 + 条件变量
        let slots = Arc::new((std::sync::Mutex::new(0usize), std::sync::Condvar::new()));
        for req in rx {
            let run_id = req.run_id.clone();
            // 等待空闲槽（每次重新读 max_parallel，支持运行时调大/调小）
            {
                let (m, cv) = &*slots;
                let mut g = m.lock().unwrap();
                loop {
                    let max = state.task_max_parallel.load(Ordering::SeqCst).max(1);
                    if *g < max {
                        *g += 1;
                        break;
                    }
                    g = cv.wait(g).unwrap();
                }
            }

            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            state
                .task_cancel_flags
                .lock()
                .unwrap()
                .insert(run_id.clone(), cancel.clone());
            state.task_active.lock().unwrap().insert(run_id.clone());

            let st = state.clone();
            let slots2 = slots.clone();
            let rid = run_id.clone();
            std::thread::spawn(move || {
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    execute(&st, req, cancel)
                }));
                if res.is_err() {
                    let conn = st.db.lock().unwrap();
                    db::finish_task_run(
                        &conn,
                        &rid,
                        TaskRunStatus::Failed,
                        None,
                        now_ms() / 1000,
                        0,
                    );
                }
                st.task_active.lock().unwrap().remove(&rid);
                st.task_cancel_flags.lock().unwrap().remove(&rid);
                let (m, cv) = &*slots2;
                let mut g = m.lock().unwrap();
                *g = g.saturating_sub(1);
                cv.notify_all();
            });
        }
    });
}

fn execute(state: &Arc<AppState>, req: TaskRunRequest, cancel: Arc<std::sync::atomic::AtomicBool>) {
    let _ = &cancel; // 取消标志在等待循环里读取

    let emitter = match state.current_emitter() {
        Some(e) => e,
        None => return,
    };
    let cfg = req.config.clone();
    let run_dir = state.log_root.join("_tasks").join(&req.run_id);
    let sink = match LogSink::open(&run_dir) {
        Ok(s) => s,
        Err(e) => {
            let conn = state.db.lock().unwrap();
            db::finish_task_run(
                &conn,
                &req.run_id,
                TaskRunStatus::Failed,
                None,
                now_ms() / 1000,
                0,
            );
            let _ = e;
            return;
        }
    };

    let command = cfg.command.trim().to_string();
    if command.is_empty() {
        logs::system_message("[task] 命令为空", &req.run_id, &emitter, &sink);
        let conn = state.db.lock().unwrap();
        db::finish_task_run(
            &conn,
            &req.run_id,
            TaskRunStatus::Failed,
            None,
            now_ms() / 1000,
            0,
        );
        return;
    }

    // 依赖服务检查：requires 里的服务必须都在 Running
    if !cfg.requires.is_empty() {
        let missing: Vec<String> = {
            let rts = state.runtimes.lock().unwrap();
            cfg.requires
                .iter()
                .filter(|sid| {
                    rts.get(*sid).map(|r| r.state != State::Running).unwrap_or(true)
                })
                .cloned()
                .collect()
        };
        if !missing.is_empty() {
            logs::system_message(
                &format!("[task] 依赖服务未运行：{}，已中止", missing.join("、")),
                &req.run_id,
                &emitter,
                &sink,
            );
            let conn = state.db.lock().unwrap();
            db::finish_task_run(
                &conn,
                &req.run_id,
                TaskRunStatus::Failed,
                None,
                now_ms() / 1000,
                0,
            );
            return;
        }
    }

    // 有效参数：变体参数（或默认参数）+ 运行时追加参数
    let mut all_args: Vec<String> = match req.variant_index {
        Some(i) if i < cfg.variants.len() => cfg.variants[i].args.clone(),
        _ => cfg.args.clone(),
    };
    all_args.extend(req.extra_args.iter().cloned());

    {
        let conn = state.db.lock().unwrap();
        db::set_task_run_running(&conn, &req.run_id, now_ms() / 1000, &run_dir.to_string_lossy());
    }
    logs::system_message(
        &format!("[task] 运行：{} {}", command, all_args.join(" ")),
        &req.run_id,
        &emitter,
        &sink,
    );

    // 组装进程
    let spawn = |use_shell: bool| -> Result<_, String> {
        let mut c = if use_shell {
            let mut c = Command::new(shell_prog());
            let joined = if all_args.is_empty() {
                command.clone()
            } else {
                format!("{} {}", command, all_args.join(" "))
            };
            c.args([shell_flag(), &joined]);
            c
        } else {
            let mut c = Command::new(&command);
            c.args(&all_args);
            c
        };
        let cwd = cfg.cwd.trim();
        if !cwd.is_empty() {
            c.current_dir(cwd);
        }
        for (k, v) in &cfg.env {
            c.env(k, v);
        }
        c.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0000_0200 | 0x0800_0000); // NEW_PROCESS_GROUP | NO_WINDOW
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            c.process_group(0);
            #[cfg(target_os = "linux")]
            unsafe {
                c.pre_exec(|| {
                    extern "C" {
                        fn prctl(o: i32, a: u64, b: u64, d: u64, e: u64) -> i32;
                    }
                    let _ = prctl(1, 15, 0, 0, 0); // PR_SET_PDEATHSIG, SIGTERM
                    Ok(())
                });
            }
        }
        c.spawn().map_err(|e| e.to_string())
    };

    // 先按配置方式起；非 shell 且 Windows 下找不到程序，回退 shell 起（npm/java 等 .cmd）
    let mut child = match spawn(cfg.use_shell) {
        Ok(ch) => ch,
        Err(e) => {
            if !cfg.use_shell {
                match spawn(true) {
                    Ok(ch) => ch,
                    Err(e2) => {
                        logs::system_message(
                            &format!("[task] 启动失败: {e} / {e2}"),
                            &req.run_id,
                            &emitter,
                            &sink,
                        );
                        let conn = state.db.lock().unwrap();
                        db::finish_task_run(
                            &conn,
                            &req.run_id,
                            TaskRunStatus::Failed,
                            None,
                            now_ms() / 1000,
                            0,
                        );
                        return;
                    }
                }
            } else {
                logs::system_message(
                    &format!("[task] 启动失败: {e}"),
                    &req.run_id,
                    &emitter,
                    &sink,
                );
                let conn = state.db.lock().unwrap();
                db::finish_task_run(
                    &conn,
                    &req.run_id,
                    TaskRunStatus::Failed,
                    None,
                    now_ms() / 1000,
                    0,
                );
                return;
            }
        }
    };

    #[cfg(unix)]
    let pid = child.id();
    #[cfg(windows)]
    let job = Job::new_kill_on_close().ok().inspect(|j| {
        let _ = j.assign(&child);
    });

    // 起日志读取线程（不 join，随进程/进程组退出自然 EOF）
    if let Some(o) = child.stdout.take() {
        logs::spawn_reader(
            o,
            cfg.encoding.clone(),
            "stdout",
            req.run_id.clone(),
            emitter.clone(),
            sink.clone(),
        );
    }
    if let Some(er) = child.stderr.take() {
        logs::spawn_reader(
            er,
            cfg.encoding.clone(),
            "stderr",
            req.run_id.clone(),
            emitter.clone(),
            sink.clone(),
        );
    }

    // 等待退出或取消（构建可能很久，不设超时）
    let start = Instant::now();
    let mut cancelled = false;
    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if cancel.load(Ordering::SeqCst) {
                    cancelled = true;
                    #[cfg(unix)]
                    crate::drivers::command::kill_group(pid, false);
                    let _ = child.kill();
                    let _ = child.wait();
                    #[cfg(unix)]
                    {
                        std::thread::sleep(Duration::from_millis(500));
                        crate::drivers::command::kill_group(pid, true);
                    }
                    break None;
                }
                std::thread::sleep(Duration::from_millis(300));
            }
            Err(_) => {
                let _ = child.kill();
                break None;
            }
        }
    };
    // drop(child) + drop(job) -> Windows Job 回收整棵进程树
    #[cfg(windows)]
    let _ = job;

    let (status, exit_code) = if cancelled {
        (TaskRunStatus::Cancelled, None)
    } else {
        match exit {
            Some(s) => {
                let code = s.code();
                (
                    if code == Some(0) {
                        TaskRunStatus::Success
                    } else {
                        TaskRunStatus::Failed
                    },
                    code,
                )
            }
            None => (TaskRunStatus::Failed, None),
        }
    };
    let dur = start.elapsed().as_millis() as i64;
    logs::system_message(
        &format!("[task] 结束：{:?} exit={:?}", status, exit_code),
        &req.run_id,
        &emitter,
        &sink,
    );
    let conn = state.db.lock().unwrap();
    db::finish_task_run(&conn, &req.run_id, status, exit_code, now_ms() / 1000, dur);
}
