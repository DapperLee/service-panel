use crate::health;
use crate::manager;
use crate::state::{AppState, StateEvent};
use crate::types::{Desired, DriverKind, HealthCheck, RestartMode, State};
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use sysinfo::{Pid, System};

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 单批并发探测数上限（外部状态探测与健康探测各自独立分块）
const PROBE_CONCURRENCY: usize = 8;

/// 启动后台监督线程
pub fn spawn_supervisor(state: Arc<AppState>) {
    thread::spawn(move || {
        // 面板启动后先做一次“接管式”初始化 + 自启
        takeover_and_autostart(&state);

        let mut sys = System::new_all();
        loop {
            sys.refresh_processes();
            tick(&state, &sys);
            thread::sleep(Duration::from_secs(2));
        }
    });
}

/// 应用重启后不盲目再 start：外部驱动查真实状态；Command 驱动若无法接管则置 Stopped
fn takeover_and_autostart(state: &Arc<AppState>) {
    // 同步外部驱动真实状态：探测同样分块并发，服务多时启动不再被 N 次串行 sc query 拖慢
    let probes: Vec<(String, DriverKind, String)> = {
        let rts = state.runtimes.lock().unwrap();
        rts.iter()
            .filter_map(|(id, r)| match r.spec.driver {
                DriverKind::WindowsService => Some((
                    id.clone(),
                    DriverKind::WindowsService,
                    r.spec.service_name.clone(),
                )),
                DriverKind::Docker => {
                    Some((id.clone(), DriverKind::Docker, r.spec.container.clone()))
                }
                _ => None,
            })
            .collect()
    };

    let mut takeover_results: Vec<(String, Option<bool>)> = Vec::new();
    for chunk in probes.chunks(PROBE_CONCURRENCY) {
        let part: Vec<(String, Option<bool>)> = thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|(id, driver, target)| {
                    s.spawn(move || {
                        let running = match driver {
                            DriverKind::WindowsService => {
                                crate::drivers::winservice::status_running(target)
                            }
                            DriverKind::Docker => crate::drivers::docker::status_running(target),
                            _ => None,
                        };
                        (id.clone(), running)
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        takeover_results.extend(part);
    }

    for (id, running) in takeover_results {
        let Some(running) = running else { continue };
        let st = if running {
            State::Running
        } else {
            State::Stopped
        };
        {
            let mut rts = state.runtimes.lock().unwrap();
            if let Some(rt) = rts.get_mut(&id) {
                rt.state = st;
                // 接管时若已在运行，期望态也置 Running，避免被当成用户已停止
                if running {
                    rt.desired = Desired::Running;
                }
            }
        }
        if running {
            crate::logsrc::attach(state, &id);
        }
    }

    // 拉起 enabled 且当前为 Stopped 的服务
    let autostart: Vec<String> = {
        let rts = state.runtimes.lock().unwrap();
        rts.iter()
            .filter(|(_, r)| r.state == State::Stopped)
            .map(|(id, _)| id.clone())
            .collect()
    };
    // 只有 enabled=true 的才自启（enabled 存在 DB，这里读一次）
    let enabled_ids = enabled_ids(state);
    for id in autostart {
        if enabled_ids.contains(&id) {
            let _ = manager::start(state, &id);
            thread::sleep(Duration::from_millis(300)); // 顺序错开，避免端口抢占
        }
    }
}

fn enabled_ids(state: &Arc<AppState>) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    if let Ok(rows) = state.db.lock() {
        let mut stmt = rows.prepare("SELECT id FROM services WHERE enabled=1").ok();
        if let Some(stmt) = stmt.as_mut() {
            if let Ok(iter) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                for r in iter.flatten() {
                    set.insert(r);
                }
            }
        }
    }
    set
}

fn tick(state: &Arc<AppState>, sys: &System) {
    let mut to_restart: Vec<String> = Vec::new();
    let mut to_detach: Vec<String> = Vec::new();
    let mut events: Vec<StateEvent> = Vec::new();

    // 需要在锁外做的阻塞探测任务
    let mut external_jobs: Vec<(String, DriverKind, String)> = Vec::new();
    let mut health_jobs: Vec<(String, HealthCheck)> = Vec::new();

    // 每 tick 建一次 parent -> children 映射，供进程树 BFS 采样（含孙进程）
    let children_map = build_children_map(sys);

    // —— Phase A：锁内只做廉价操作（资源采样、Command try_wait 非阻塞），采集探测任务 ——
    {
        let mut rts = state.runtimes.lock().unwrap();
        let now = now_ms();
        for (id, rt) in rts.iter_mut() {
            // 1) 资源采样（sys 已刷新，无 I/O）
            if let Some(live) = &rt.live {
                let (cpu, mem_mb) = sample_tree(sys, &children_map, live.pid);
                rt.cpu = cpu;
                rt.mem_mb = mem_mb;
            } else {
                rt.cpu = 0.0;
                rt.mem_mb = 0.0;
            }

            // 2) 崩溃检测：托管进程意外退出（try_wait 非阻塞，可在锁内）
            if rt.spec.driver == DriverKind::Command {
                let mut exited: Option<i32> = None;
                if let Some(live) = rt.live.as_mut() {
                    if let Ok(Some(status)) = live.child.try_wait() {
                        exited = Some(status.code().unwrap_or(-1));
                    }
                }
                if let Some(code) = exited {
                    let prev = rt.state;
                    rt.live = None; // drop 触发 Job 回收
                    if rt.desired == Desired::Running {
                        rt.state = State::Crashed;
                        rt.last_error = Some(format!("进程意外退出 code={code}"));
                        events.push(StateEvent {
                            id: id.clone(),
                            from: Some(prev),
                            state: State::Crashed,
                            pid: None,
                            error: rt.last_error.clone(),
                            exit_code: Some(code),
                        });
                        maybe_schedule_restart(rt, now);
                    } else {
                        rt.state = State::Stopped;
                        events.push(StateEvent {
                            id: id.clone(),
                            from: Some(prev),
                            state: State::Stopped,
                            pid: None,
                            error: None,
                            exit_code: None,
                        });
                    }
                    continue;
                }
            }

            // 3) 采集外部状态探测任务（阻塞 I/O，锁外并行做）
            if rt.desired == Desired::Running && rt.spec.driver != DriverKind::Command {
                let target = match rt.spec.driver {
                    DriverKind::WindowsService => rt.spec.service_name.clone(),
                    DriverKind::Docker => rt.spec.container.clone(),
                    _ => String::new(),
                };
                if !target.is_empty() {
                    external_jobs.push((id.clone(), rt.spec.driver, target));
                }
            }

            // 4) 采集健康探测任务（阻塞 I/O，锁外并行做）
            if rt.state == State::Running
                && rt.spec.healthcheck.kind != crate::types::HealthKind::None
            {
                health_jobs.push((id.clone(), rt.spec.healthcheck.clone()));
            }
        }
    }

    // —— Phase B：锁外探测，绝不持锁做网络/子进程 I/O ——
    // 分块并发：单批最多 PROBE_CONCURRENCY 个线程。每个探测都要起子进程（sc / docker）
    // 或做网络握手，服务多时若一 tick 全量并发，线程与子进程能瞬间堆出上百个。
    // 探测线程 panic 时该 id 直接缺席：外部状态视为未知（不误判崩溃）、
    // 健康结果视为未探测（不累加 unhealthy_streak）。
    let mut external_results: Vec<(String, Option<bool>)> = Vec::new();
    for chunk in external_jobs.chunks(PROBE_CONCURRENCY) {
        let part: Vec<(String, Option<bool>)> = thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|(id, driver, target)| {
                    s.spawn(move || {
                        let running = match driver {
                            DriverKind::WindowsService => {
                                crate::drivers::winservice::status_running(target)
                            }
                            DriverKind::Docker => crate::drivers::docker::status_running(target),
                            _ => None,
                        };
                        (id.clone(), running)
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        external_results.extend(part);
    }

    let mut health_results: Vec<(String, Result<(), String>)> = Vec::new();
    for chunk in health_jobs.chunks(PROBE_CONCURRENCY) {
        let part: Vec<(String, Result<(), String>)> = thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|(id, hc)| s.spawn(move || (id.clone(), health::check(hc))))
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        health_results.extend(part);
    }

    let ext_map: HashMap<String, Option<bool>> = external_results.into_iter().collect();
    let health_map: HashMap<String, Result<(), String>> = health_results.into_iter().collect();

    // —— Phase C：回锁应用结果 + 排期重启（探测期间状态可能被用户改动，需重新校验）——
    {
        let mut rts = state.runtimes.lock().unwrap();
        let now = now_ms();
        for (id, rt) in rts.iter_mut() {
            // 稳定运行超过 30s：清零重试计数，避免偶发崩溃累积到上限后不再自愈
            if rt.state == State::Running
                && rt.running_since != 0
                && now - rt.running_since > 30_000
                && rt.fail_streak != 0
            {
                rt.fail_streak = 0;
            }
            // 外部服务未运行 -> 视为崩溃
            if rt.desired == Desired::Running
                && rt.spec.driver != DriverKind::Command
                && matches!(ext_map.get(id), Some(Some(false)))
            {
                let prev = rt.state;
                rt.state = State::Crashed;
                events.push(StateEvent {
                    id: id.clone(),
                    from: Some(prev),
                    state: State::Crashed,
                    pid: None,
                    error: Some("外部服务未在运行".to_string()),
                    exit_code: None,
                });
                to_detach.push(id.clone());
                maybe_schedule_restart(rt, now);
                continue;
            }

            // 健康检查结果（仅对仍是 Running 的服务应用，避免探测期间被停止后误判）
            if rt.state == State::Running {
                if let Some(res) = health_map.get(id) {
                    let threshold = rt.spec.healthcheck.unhealthy_threshold;
                    match res {
                        Ok(()) => rt.unhealthy_streak = 0,
                        Err(_) => {
                            rt.unhealthy_streak += 1;
                            if rt.unhealthy_streak >= threshold {
                                let prev = rt.state;
                                rt.state = State::Unhealthy;
                                events.push(StateEvent {
                                    id: id.clone(),
                                    from: Some(prev),
                                    state: State::Unhealthy,
                                    pid: rt.live.as_ref().map(|l| l.pid),
                                    error: Some("健康检查连续失败".to_string()),
                                    exit_code: None,
                                });
                                if rt.spec.restart_policy.mode != RestartMode::Never {
                                    maybe_schedule_restart(rt, now);
                                }
                            }
                        }
                    }
                }
            }

            // 到达退避时间 -> 执行重启
            if matches!(rt.state, State::Crashed | State::Restarting)
                && rt.desired == Desired::Running
                && rt.next_attempt_at != 0
                && now >= rt.next_attempt_at
            {
                to_restart.push(id.clone());
                rt.next_attempt_at = 0;
                rt.state = State::Restarting;
            }
        }
    }

    // 锁外广播状态事件
    for ev in events {
        state.push_state(ev);
    }
    // 锁外卸载已停外部服务的日志源
    for id in to_detach {
        crate::logsrc::detach(state, &id);
    }
    // 锁外执行重启
    for id in to_restart {
        if let Err(e) = manager::start(state, &id) {
            // 启动失败（多为“该服务正在执行其它操作”锁占用）：重新排期，别卡死不再拉
            let now = now_ms();
            let mut rts = state.runtimes.lock().unwrap();
            if let Some(rt) = rts.get_mut(&id) {
                rt.state = State::Crashed;
                rt.next_attempt_at = now + 3000;
                rt.last_error = Some(e);
            }
        }
    }
}

/// 依据重启策略决定是否/何时再拉起，设置 next_attempt_at（指数退避）
fn maybe_schedule_restart(rt: &mut crate::state::Runtime, now: i64) {
    // 先把策略字段拷出来，避免 policy 的不可变借用与 rt 的可变借用冲突
    let (mode, max_retries, base) = {
        let p = &rt.spec.restart_policy;
        (p.mode, p.max_retries, p.backoff_base_secs as i64)
    };
    match mode {
        RestartMode::Never => {}
        RestartMode::OnFailure | RestartMode::Always => {
            rt.fail_streak += 1;
            if rt.fail_streak > max_retries {
                // 超过快速重试上限：进入长冷却后再试，而不是永久放弃
                // （否则 Docker/服务偶发崩溃累计到上限后，即使故障恢复也不再自愈）
                rt.last_error = Some(format!(
                    "已达最大重试次数 {max_retries}，进入 5 分钟冷却后重试"
                ));
                rt.fail_streak = 0;
                rt.next_attempt_at = now + 300_000;
                return;
            }
            let shift = (rt.fail_streak - 1).min(5);
            let backoff = (base * (1i64 << shift)).min(60);
            rt.next_attempt_at = now + backoff * 1000;
            // 不立即重启，等下一个 tick 到点再启，保证退避生效
        }
    }
}

/// 每 tick 建一次 parent -> children 映射（一次遍历全部进程，O(n)）
fn build_children_map(sys: &System) -> HashMap<Pid, Vec<Pid>> {
    let mut map: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (pid, p) in sys.processes() {
        if let Some(parent) = p.parent() {
            map.entry(parent).or_default().push(*pid);
        }
    }
    map
}

/// 采样根进程及其**整棵进程树**（BFS 含孙进程）的 CPU/内存之和。
/// Node/pm2 的 worker 常是孙进程，只数直接子进程会明显少报。
fn sample_tree(sys: &System, children_map: &HashMap<Pid, Vec<Pid>>, root_pid: u32) -> (f32, f32) {
    let root = Pid::from_u32(root_pid);
    let mut cpu = 0.0f32;
    let mut mem_bytes = 0u64;
    let mut stack = vec![root];
    let mut seen: std::collections::HashSet<Pid> = std::collections::HashSet::new();
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue; // 防御环/重复
        }
        if let Some(p) = sys.process(pid) {
            cpu += p.cpu_usage();
            mem_bytes += p.memory();
        }
        if let Some(kids) = children_map.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }
    (cpu, mem_bytes as f32 / (1024.0 * 1024.0))
}
