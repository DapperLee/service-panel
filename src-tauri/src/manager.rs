use crate::drivers::{command, docker, winservice};
use crate::logs;
use crate::state::{AppState, LiveProc, Runtime, StateEvent};
use crate::types::{Desired, DriverKind, ServiceSpec, State};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn set_state(state: &Arc<AppState>, id: &str, new: State, err: Option<String>) {
    let mut pid = None;
    let mut from = None;
    {
        let mut rts = state.runtimes.lock().unwrap();
        if let Some(rt) = rts.get_mut(id) {
            from = Some(rt.state);
            rt.state = new;
            rt.last_error = err.clone();
            if let Some(live) = &rt.live {
                pid = Some(live.pid);
            }
        }
    }
    state.push_state(StateEvent {
        id: id.to_string(),
        from,
        state: new,
        pid,
        error: err,
        exit_code: None,
    });
}

/// 保证服务在内存注册表中有对应 Runtime（应用启动、增删改后调用）。
/// config_ok=false 表示 spec 解密/解析失败：把告警同时写进 last_error，让卡片上
/// 看得见；用户重新保存出好配置后，这条陈旧告警会被自动清掉。
pub fn ensure_runtime(state: &Arc<AppState>, id: &str, spec: ServiceSpec, config_ok: bool) {
    let broken_msg = || crate::db::CONFIG_BROKEN_MSG.to_string();
    let mut rts = state.runtimes.lock().unwrap();
    match rts.get_mut(id) {
        Some(rt) => {
            // 已存在则刷新 spec/config_ok（编辑配置后）
            let was_broken = !rt.config_ok;
            let stale_err = rt.last_error.as_deref() == Some(crate::db::CONFIG_BROKEN_MSG);
            rt.spec = spec;
            rt.config_ok = config_ok;
            if config_ok {
                if was_broken && stale_err {
                    rt.last_error = None;
                }
            } else {
                rt.last_error = Some(broken_msg());
            }
        }
        None => {
            let mut rt = Runtime::new(spec);
            rt.config_ok = config_ok;
            rt.last_error = (!config_ok).then(broken_msg);
            rts.insert(id.to_string(), rt);
        }
    }
}

fn get_spec(state: &Arc<AppState>, id: &str) -> Option<ServiceSpec> {
    state.runtimes.lock().unwrap().get(id).map(|r| r.spec.clone())
}

/// 取 spec 与「配置是否可用」标记（坏配置禁止启动，见 start_impl）
fn get_spec_and_ok(state: &Arc<AppState>, id: &str) -> Option<(ServiceSpec, bool)> {
    state
        .runtimes
        .lock()
        .unwrap()
        .get(id)
        .map(|r| (r.spec.clone(), r.config_ok))
}

fn set_desired(state: &Arc<AppState>, id: &str, desired: Desired) {
    if let Some(rt) = state.runtimes.lock().unwrap().get_mut(id) {
        rt.desired = desired;
    }
}

/// 生命周期操作互斥锁（RAII）：同一服务同一时刻只允许一个启/停/重启在跑，
/// 防止异步命令下用户双击导致重复 spawn 进程，也避免与监督线程的自动重启打架。
pub struct OpGuard {
    state: Arc<AppState>,
    id: String,
}
impl Drop for OpGuard {
    fn drop(&mut self) {
        self.state.ops.lock().unwrap().remove(&self.id);
    }
}

fn try_lock_op(state: &Arc<AppState>, id: &str) -> Option<OpGuard> {
    let mut ops = state.ops.lock().unwrap();
    if ops.contains(id) {
        return None;
    }
    ops.insert(id.to_string());
    Some(OpGuard {
        state: state.clone(),
        id: id.to_string(),
    })
}

pub fn start(state: &Arc<AppState>, id: &str) -> Result<(), String> {
    let _g = try_lock_op(state, id).ok_or("该服务正在执行其它操作，请稍候")?;
    start_impl(state, id)
}

pub fn stop(state: &Arc<AppState>, id: &str) -> Result<(), String> {
    let _g = try_lock_op(state, id).ok_or("该服务正在执行其它操作，请稍候")?;
    stop_impl(state, id)
}

fn start_impl(state: &Arc<AppState>, id: &str) -> Result<(), String> {
    let (spec, config_ok) = get_spec_and_ok(state, id).ok_or("服务不存在")?;

    // 坏配置（解密/解析失败）的 spec 是空壳：命令为空时 command 驱动会回退到
    // `cmd /C ""`，而它能成功 spawn 并立刻退出，于是被当成「启动成功」，紧接着
    // 崩溃检测 -> 指数退避 -> 打满 max_retries -> 长冷却 -> 再假启动，无限循环。
    // 这里在落期望态之前就挡住，免得把 desired 也污染成 Running。
    if !config_ok {
        return Err(crate::db::CONFIG_BROKEN_MSG.to_string());
    }

    // 空命令在创建日志目录之前就拒掉：启不动的服务不该留下任何副作用
    if spec.driver == DriverKind::Command {
        command::validate(&spec)?;
    }

    set_desired(state, id, Desired::Running);
    set_state(state, id, State::Starting, None);

    let result = match spec.driver {
        DriverKind::Command => {
            let emitter = state.current_emitter().ok_or("emitter 未就绪")?;
            // 日志写入端全局唯一（按字节滚动的前提）：取到即缓存进 Runtime
            let sink = state
                .log_sink(id)
                .ok_or_else(|| "无法准备日志文件".to_string())?;
            match command::spawn(&spec, id, sink, emitter) {
                Ok(spawned) => {
                    let live = LiveProc {
                        child: spawned.child,
                        pid: spawned.pid,
                        #[cfg(windows)]
                        job: spawned.job,
                    };
                    let mut rts = state.runtimes.lock().unwrap();
                    if let Some(rt) = rts.get_mut(id) {
                        rt.live = Some(live);
                    }
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        DriverKind::WindowsService => winservice::start(&spec.service_name),
        DriverKind::Docker => docker::start(&spec.container),
    };

    match result {
        Ok(()) => {
            // 启动成功：对所有驱动重置重试/健康计数并记录稳定运行起点
            {
                let mut rts = state.runtimes.lock().unwrap();
                if let Some(rt) = rts.get_mut(id) {
                    rt.unhealthy_streak = 0;
                    rt.fail_streak = 0;
                    rt.next_attempt_at = 0;
                    rt.running_since = chrono::Utc::now().timestamp_millis();
                }
            }
            set_state(state, id, State::Running, None);
            log_sys(state, id, "[panel] 启动成功");
            // 为外部服务（Docker / Windows 服务）挂载日志源
            crate::logsrc::attach(state, id);
            Ok(())
        }
        Err(e) => {
            set_state(state, id, State::Stopped, Some(e.clone()));
            log_sys(state, id, &format!("[panel] 启动失败: {e}"));
            Err(e)
        }
    }
}

fn log_sys(state: &Arc<AppState>, id: &str, msg: &str) {
    // 复用 Runtime 里缓存的唯一写入端：不再每条系统消息都 open 一次文件
    if let (Some(emitter), Some(sink)) = (state.current_emitter(), state.log_sink(id)) {
        logs::system_message(msg, id, &emitter, &sink);
    }
}

fn command_stop(state: &Arc<AppState>, id: &str, timeout_secs: u64) {
    let live = {
        let mut rts = state.runtimes.lock().unwrap();
        rts.get_mut(id).and_then(|r| r.live.take())
    };
    if let Some(mut live) = live {
        let deadline = Instant::now() + Duration::from_secs(timeout_secs.max(1));
        loop {
            match live.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = live.child.kill();
                        let _ = live.child.wait();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(_) => {
                    let _ = live.child.kill();
                    break;
                }
            }
        }
        // 离开作用域 -> drop(job) -> CloseHandle -> Job Object 连带回收整棵进程树
    }
}

fn stop_impl(state: &Arc<AppState>, id: &str) -> Result<(), String> {
    let spec = get_spec(state, id).ok_or("服务不存在")?;
    // 先落期望态，防止监督线程在停止过程中把它当崩溃重启
    set_desired(state, id, Desired::Stopped);
    set_state(state, id, State::Stopping, None);

    let result = match spec.driver {
        DriverKind::Command => {
            command_stop(state, id, spec.stop_timeout_secs);
            Ok(())
        }
        DriverKind::WindowsService => {
            // sc stop 异步：轮询到真正停止，避免立刻置 Stopped 后用户点启动撞 1056
            match winservice::stop_and_wait(
                &spec.service_name,
                Duration::from_secs(spec.stop_timeout_secs.max(30)),
            ) {
                Ok(true) => Ok(()),
                Ok(false) => Err("服务停止超时（仍在 STOP_PENDING）".to_string()),
                Err(e) => Err(e),
            }
        }
        DriverKind::Docker => docker::stop(&spec.container),
    };

    set_state(state, id, State::Stopped, result.as_ref().err().cloned());
    // 卸载日志源（停文件尾随线程 / 杀 docker logs 子进程）
    crate::logsrc::detach(state, id);
    log_sys(state, id, "[panel] 已停止");
    result
}

pub fn restart(state: &Arc<AppState>, id: &str) -> Result<(), String> {
    let _g = try_lock_op(state, id).ok_or("该服务正在执行其它操作，请稍候")?;
    let was_running = state
        .runtimes
        .lock()
        .unwrap()
        .get(id)
        .map(|r| r.desired == Desired::Running)
        .unwrap_or(false);
    if was_running {
        stop_impl(state, id)?;
    }
    start_impl(state, id)
}

/// 保证服务在内存注册表中有对应 Runtime（应用启动、增删改后调用）
pub fn remove(state: &Arc<AppState>, id: &str) {
    crate::logsrc::detach(state, id);
    let live = {
        let mut rts = state.runtimes.lock().unwrap();
        rts.remove(id).and_then(|r| r.live)
    };
    if let Some(mut live) = live {
        let _ = live.child.kill();
        let _ = live.child.wait();
    }
}
