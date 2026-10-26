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
        let mut rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(rt) = rts.get_mut(id) {
            from = Some(rt.state);
            rt.state = new;
            rt.last_error = err.clone();
            pid = rt.observed_pid();
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
    let mut rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
    match rts.get_mut(id) {
        Some(rt) => {
            // 已存在则刷新 config_ok 与模板。运行中的服务**不覆盖活动 spec**
            // （保留已解析的端口/健康目标），新配置到下次重启才生效，避免编辑
            // 把正在跑的自动端口服务打回 {port} 导致健康检查失效。
            let was_broken = !rt.config_ok;
            let stale_err = rt.last_error.as_deref() == Some(crate::db::CONFIG_BROKEN_MSG);
            let running = matches!(
                rt.state,
                State::Running | State::Starting | State::Restarting | State::Unhealthy
            );
            rt.spec_template = spec.clone();
            if !running {
                rt.spec = spec;
            }
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
    state
        .runtimes
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(id)
        .map(|r| r.spec.clone())
}

/// 取原始 spec 模板（含 {port} 占位符）+ config_ok，用于每次启动重新解析端口
fn get_template_and_ok(state: &Arc<AppState>, id: &str) -> Option<(ServiceSpec, bool)> {
    state
        .runtimes
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(id)
        .map(|r| (r.spec_template.clone(), r.config_ok))
}

/// 把自动端口解析进 spec：替换 {port} 占位符并注入端口环境变量
fn resolve_spec_port(spec: &ServiceSpec, port: u16) -> ServiceSpec {
    use crate::netdiag;
    let ps = port.to_string();
    let mut s = spec.clone();
    s.command = netdiag::resolve_port_token(&s.command, port);
    s.args = s
        .args
        .iter()
        .map(|a| netdiag::resolve_port_token(a, port))
        .collect();
    let mut env = std::collections::HashMap::new();
    for (k, v) in s.env {
        env.insert(k, netdiag::resolve_port_token(&v, port));
    }
    s.env = env;
    s.healthcheck.target = netdiag::resolve_port_token(&s.healthcheck.target, port);
    if !s.port_env_key.trim().is_empty() {
        s.env.insert(s.port_env_key.trim().to_string(), ps);
    }
    s
}

fn set_desired(state: &Arc<AppState>, id: &str, desired: Desired) {
    if let Some(rt) = state
        .runtimes
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(id)
    {
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
        self.state
            .ops
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

fn try_lock_op(state: &Arc<AppState>, id: &str) -> Option<OpGuard> {
    let mut ops = state.ops.lock().unwrap_or_else(|e| e.into_inner());
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
    // 从模板读，保证每次启动都重新解析 {port}（不被上一次解析结果污染）
    let (mut spec, config_ok) = get_template_and_ok(state, id).ok_or("服务不存在")?;

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

    // 自动端口：默认端口空闲就用它，被占则往上找；解析出本次实际 spec
    if spec.driver == DriverKind::Command && spec.auto_port && spec.default_port > 0 {
        let chosen = if crate::netdiag::is_port_free(spec.default_port) {
            spec.default_port
        } else {
            crate::netdiag::find_free_port(spec.default_port.saturating_add(1), 50).ok_or_else(
                || {
                    format!(
                        "默认端口 {} 起连续 50 个端口都被占用，找不到空闲端口",
                        spec.default_port
                    )
                },
            )?
        };
        spec = resolve_spec_port(&spec, chosen);
        let mut rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(rt) = rts.get_mut(id) {
            rt.spec = spec.clone();
            rt.chosen_port = Some(chosen);
        }
    } else if let Some(rt) = state
        .runtimes
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(id)
    {
        // 非自动端口：用模板原样，清掉上次自动端口残留
        rt.spec = spec.clone();
        rt.chosen_port = None;
    }

    set_desired(state, id, Desired::Running);
    set_state(state, id, State::Starting, None);

    // 若该服务仍持有存活的托管/接管进程（如 Unhealthy 但进程没退、或重复点启动），
    // 先回收再启动，避免双起两个实例抢端口。command_stop 是内部函数，不重入 op 锁。
    let had_live = state
        .runtimes
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(id)
        .map(|r| r.live.is_some() || r.adopted.is_some())
        .unwrap_or(false);
    if had_live && spec.driver == DriverKind::Command {
        command_stop(state, id, spec.stop_timeout_secs);
    }

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
                        #[cfg(unix)]
                        guard: spawned.guard,
                    };
                    let mut rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
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
                let mut rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(rt) = rts.get_mut(id) {
                    rt.unhealthy_streak = 0;
                    rt.fail_streak = 0;
                    rt.next_attempt_at = 0;
                    // 排期也归零：刚起来的服务下一拍就该探一次，不该沿用停机前的时间点
                    rt.next_health_at = 0;
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

pub(crate) fn log_sys(state: &Arc<AppState>, id: &str, msg: &str) {
    // 复用 Runtime 里缓存的唯一写入端：不再每条系统消息都 open 一次文件
    if let (Some(emitter), Some(sink)) = (state.current_emitter(), state.log_sink(id)) {
        logs::system_message(msg, id, &emitter, &sink);
    }
}

/// 先停看门狗、再动进程组。顺序反了的话，看门狗可能在 pgid 已被系统复用之后
/// 才醒来补一刀 SIGKILL，误杀无关进程组。
#[cfg(unix)]
pub fn kill_guard(live: &mut LiveProc) {
    if let Some(mut g) = live.guard.take() {
        let _ = g.kill();
        let _ = g.wait();
    }
}

fn command_stop(state: &Arc<AppState>, id: &str, timeout_secs: u64) {
    let (live, adopted) = {
        let mut rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
        match rts.get_mut(id) {
            Some(r) => (r.live.take(), r.adopted.take()),
            None => (None, None),
        }
    };
    if let Some(mut live) = live {
        #[cfg(unix)]
        kill_guard(&mut live);
        #[cfg(unix)]
        let pid = live.child.id();
        let grace = timeout_secs.max(1);
        // 把宽限期写进服务自己的日志：mac/linux 上「停止没等够就死了/根本没等」这类
        // 反馈，靠这行和下面的耗时行就能判，不用用户自证
        log_sys(
            state,
            id,
            &format!("[panel] 优雅停止：请求进程退出，最多等待 {grace} 秒"),
        );
        let t0 = Instant::now();
        let deadline = t0 + Duration::from_secs(grace);

        // unix：先向整个进程组发 SIGTERM（Linux 上子进程也会因 PDEATHSIG 感知父退出）
        #[cfg(unix)]
        crate::drivers::command::kill_group(pid, false);

        let mut forced = false;
        loop {
            match live.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        #[cfg(unix)]
                        crate::drivers::command::kill_group(pid, true);
                        let _ = live.child.kill();
                        let _ = live.child.wait();
                        forced = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(_) => {
                    #[cfg(unix)]
                    crate::drivers::command::kill_group(pid, true);
                    let _ = live.child.kill();
                    forced = true;
                    break;
                }
            }
        }
        let took = t0.elapsed().as_secs_f32();
        if forced {
            log_sys(
                state,
                id,
                &format!("[panel] {grace} 秒内进程未退出，已强杀（实际用时 {took:.1} 秒）"),
            );
        } else {
            log_sys(state, id, &format!("[panel] 进程已退出，用时 {took:.1} 秒"));
        }
        // Windows: 离开作用域 drop(job) -> CloseHandle -> Job Object 连带回收整棵进程树
        // unix: 上面已 kill_group 回收；Child drop 不影响已被信号的进程
    }
    // 接管的进程：take() 出来即与 Runtime 脱离，drop 掉 Job 句柄就由操作系统结束它，
    // 然后等它真的从进程表里消失（拿不到 Child，只能轮询内核快照）
    if let Some(a) = adopted {
        let pid = a.pid;
        drop(a);
        let deadline = Instant::now() + Duration::from_secs(timeout_secs.max(1));
        while crate::netdiag::process_alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// 面板重启后尝试**接管**一个本服务对应、但由面板之外启动的进程。
///
/// 认领要极谨慎：一旦认领，用户点「停止」就意味着我们去结束这个进程。所以要求
/// 命令行候选唯一；若服务还声明了 TCP/HTTP 健康端口，则再用端口收窄一次。
/// 另外必须成功挂上 KILL_ON_JOB_CLOSE 的 Job——没有这层兜底就不接管。
pub fn try_adopt(state: &Arc<AppState>, id: &str) -> Option<u32> {
    let spec = get_spec(state, id)?;
    if spec.driver != DriverKind::Command {
        return None;
    }
    let command = spec.command.trim().to_string();
    if command.is_empty() {
        return None;
    }
    let mut cands = crate::netdiag::find_candidates(&command, &spec.args);
    // 只有 TCP/HTTP 型健康检查的 target 才能可靠地推出端口（命令型的 target 是 shell 串）
    let declared_port = match spec.healthcheck.kind {
        crate::types::HealthKind::Tcp | crate::types::HealthKind::Http => {
            crate::netdiag::port_of(&spec.healthcheck.target)
        }
        _ => None,
    };
    if let Some(port) = declared_port {
        // 端口没人监听 -> 服务其实没起来（或不是它占的端口），不接管
        let listener = crate::netdiag::listener_pid(port)?;
        cands.retain(|p| {
            *p == listener
                || crate::netdiag::is_descendant_of(listener, *p)
                || crate::netdiag::is_descendant_of(*p, listener)
        });
    }
    if cands.len() != 1 {
        return None;
    }
    let pid = cands[0];

    #[cfg(windows)]
    let job = {
        match crate::winjob::Job::new_kill_on_close() {
            Ok(j) => {
                if j.assign_pid(pid).is_err() {
                    return None; // 挂不上就没有兜底，宁可不接管
                }
                Some(j)
            }
            Err(_) => return None,
        }
    };

    {
        let mut rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
        let rt = rts.get_mut(id)?;
        if rt.live.is_some() {
            return None; // 已经有面板自己启动的实例，不重复认领
        }
        rt.adopted = Some(crate::state::Adopted {
            pid,
            #[cfg(windows)]
            job,
        });
        rt.state = State::Running;
        rt.desired = Desired::Running;
        rt.running_since = chrono::Utc::now().timestamp_millis();
    }
    log_sys(
        state,
        id,
        &format!("[panel] 接管已在运行的进程 pid={pid}（非面板启动；停止将结束该进程）"),
    );
    crate::logsrc::attach(state, id);
    Some(pid)
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
        .unwrap_or_else(|e| e.into_inner())
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
        let mut rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
        rts.remove(id).and_then(|r| r.live)
    };
    if let Some(mut live) = live {
        #[cfg(unix)]
        kill_guard(&mut live);
        let _ = live.child.kill();
        let _ = live.child.wait();
    }
}

/// 退出面板时的兜底回收。Windows 由 Job Object 的 KILL_ON_JOB_CLOSE 在进程结束时
/// 由操作系统连带回收，不需要走到这里；macOS/Linux 没有这个机制，`Child` 被 drop
/// 只是"脱离关系"，子进程会继续活着占端口。
///
/// 逻辑与平台无关（这样 Windows 也能编译校验它），只有发信号那一步分平台：
/// 先对整个进程组发 SIGTERM，宽限 1.5 秒，仍未退出的补 SIGKILL；
/// `docker logs -f` 这类外部日志尾随进程直接强杀（它没有优雅退出的必要）。
/// 只处理面板自己 spawn 的进程：`adopted`（接管的外部进程）与系统服务、
/// 容器本体一律不碰。
#[allow(dead_code)] // Windows 上没人调用（Job Object 已兜底），但代码要保持可编译
pub fn reap_children_on_exit(state: &Arc<AppState>) {
    let (mut procs, mut tails) = {
        let mut rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
        let mut procs = Vec::new();
        let mut tails = Vec::new();
        for rt in rts.values_mut() {
            if rt.spec.driver == DriverKind::Command {
                if let Some(live) = rt.live.take() {
                    procs.push(live);
                }
            }
            if let Some(tail) = rt.tail.take() {
                tails.push(tail);
            }
        }
        (procs, tails)
    };
    if procs.is_empty() && tails.is_empty() {
        return;
    }

    // 看门狗必须先于任何组级信号停掉（见 kill_guard 注释）
    #[cfg(unix)]
    for live in procs.iter_mut() {
        kill_guard(live);
    }

    for live in procs.iter() {
        signal_group(live, false);
    }
    for tail in tails.iter_mut() {
        let _ = tail.child.kill();
        let _ = tail.child.wait();
    }

    let deadline = Instant::now() + Duration::from_millis(1500);
    loop {
        let pending = procs
            .iter_mut()
            .any(|l| !matches!(l.child.try_wait(), Ok(Some(_))));
        if !pending || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    for live in procs.iter_mut() {
        let gone = matches!(live.child.try_wait(), Ok(Some(_)));
        if !gone {
            signal_group(live, true);
        }
        let _ = live.child.wait();
    }
}

/// 向托管进程发退出信号（组级）
fn signal_group(live: &LiveProc, force: bool) {
    #[cfg(unix)]
    {
        crate::drivers::command::kill_group(live.pid, force);
    }
    // Windows：句柄关闭即由操作系统回收整棵树，这里不做任何事。
    #[cfg(windows)]
    {
        let _ = (live, force);
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::drivers::command;
    use crate::logs::LogEmitter;
    use crate::state::LiveProc;

    fn read_logs(dir: &std::path::Path) -> String {
        let mut all = String::new();
        for day in std::fs::read_dir(dir).unwrap() {
            for shard in std::fs::read_dir(day.unwrap().path()).unwrap() {
                let p = shard.unwrap().path();
                if p.extension().and_then(|e| e.to_str()) == Some("log") {
                    all.push_str(&std::fs::read_to_string(&p).unwrap());
                }
            }
        }
        all
    }

    /// 「优雅停止超时」必须真的等满 spec 里的秒数再强杀，并把这两行写进服务日志。
    /// Windows 上 ping 不会响应退出请求，正好走满超时；用户在 Mac 上就是靠这两行
    /// 判「有没有等」的，所以这里把它钉住。
    #[test]
    fn command_stop_waits_the_configured_grace_period() {
        let dir = std::env::temp_dir().join(format!("sp-stop-{}", uuid::Uuid::new_v4()));
        let state = Arc::new(AppState::new(
            rusqlite::Connection::open_in_memory().unwrap(),
            dir.clone(),
        ));
        let spec = ServiceSpec {
            driver: DriverKind::Command,
            command: "cmd".to_string(),
            args: vec!["/C".to_string(), "ping -n 60 127.0.0.1 > nul".to_string()],
            stop_timeout_secs: 2,
            ..ServiceSpec::default()
        };
        state
            .runtimes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert("s".to_string(), Runtime::new(spec.clone()));

        let emitter: LogEmitter = Arc::new(|_| {});
        *state.emitter.lock().unwrap_or_else(|e| e.into_inner()) = Some(emitter.clone());
        let sink = state.log_sink("s").expect("日志写入端应可用");
        let spawned = command::spawn(&spec, "s", sink, emitter).unwrap();
        {
            let mut rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
            let rt = rts.get_mut("s").unwrap();
            rt.state = State::Running;
            rt.desired = Desired::Running;
            rt.live = Some(LiveProc {
                child: spawned.child,
                pid: spawned.pid,
                job: spawned.job,
            });
        }

        let t0 = Instant::now();
        stop(&state, "s").unwrap();
        let took = t0.elapsed();
        assert!(
            took >= Duration::from_secs(2) && took < Duration::from_secs(8),
            "没等满宽限期或等过头：实际 {took:?}"
        );

        let text = read_logs(&dir);
        assert!(
            text.contains("[panel] 优雅停止：请求进程退出，最多等待 2 秒"),
            "日志里没有等待声明：{text}"
        );
        assert!(
            text.contains("秒内进程未退出，已强杀"),
            "日志里没有强杀结论：{text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
