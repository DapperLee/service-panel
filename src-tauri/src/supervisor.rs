use crate::drivers::ExtStatus;
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
            // 单轮 tick 若 panic，记录后继续，绝不让监督线程死掉导致服务失去监控
            let st = state.clone();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tick(&st, &sys)));
            thread::sleep(Duration::from_secs(2));
        }
    });
}

/// 应用重启后不盲目再 start：外部驱动查真实状态；Command 驱动若无法接管则置 Stopped
fn takeover_and_autostart(state: &Arc<AppState>) {
    // 同步外部驱动真实状态：探测同样分块并发，服务多时启动不再被 N 次串行 sc query 拖慢
    let probes: Vec<(String, DriverKind, String)> = {
        let rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
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
            let mut rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
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

    // Command 驱动没有外部注册表可查状态：按「命令行唯一 + 声明端口对得上」认领
    // 已在跑的进程。必须排在自启之前，否则会对同一个服务再启一份去撞端口。
    let command_ids: Vec<String> = {
        let rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
        rts.iter()
            .filter(|(_, r)| r.spec.driver == DriverKind::Command && r.live.is_none())
            .map(|(id, _)| id.clone())
            .collect()
    };
    for id in command_ids {
        manager::try_adopt(state, &id);
    }

    // 拉起 enabled 且当前为 Stopped 的服务
    let autostart: Vec<String> = {
        let rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
        rts.iter()
            .filter(|(_, r)| r.state == State::Stopped)
            .map(|(id, _)| id.clone())
            .collect()
    };
    // 只有 enabled=true 的才自启（enabled 存在 DB，这里读一次）
    let enabled_ids = enabled_ids(state);
    for id in autostart {
        if enabled_ids.contains(&id) {
            // 只在真的拉起来之后署名「自启」：提前写日志会替这个服务建出日志目录，
            // 而启不动的服务（空命令/坏配置）不该留下任何副作用
            if manager::start(state, &id).is_ok() {
                manager::log_sys(state, &id, "[panel] 面板启动自启");
            }
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
        let mut rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_ms();
        for (id, rt) in rts.iter_mut() {
            // 1) 资源采样（sys 已刷新，无 I/O）。
            //    外部驱动的宿主 PID / 自报指标由上一拍 Phase C 的探测填进来。
            if rt.desired != Desired::Running {
                rt.ext_pid = None;
                rt.ext_metrics = None;
            }
            let sampled = if let Some(live) = &rt.live {
                sample_tree(sys, &children_map, live.pid)
            } else if let Some(a) = &rt.adopted {
                sample_tree(sys, &children_map, a.pid)
            } else if let Some(m) = rt.ext_metrics {
                Some(m)
            } else if let Some(pid) = rt.ext_pid {
                // 系统服务的宿主进程可能同时承载别的服务（svchost），按它采样会把
                // 整组的占用算到本服务头上，不如报「测不到」
                if is_shared_host(sys, pid) {
                    None
                } else {
                    sample_tree(sys, &children_map, pid)
                }
            } else {
                None
            };
            match sampled {
                Some((cpu, mem_mb)) => {
                    rt.cpu = Some(cpu);
                    rt.mem_mb = Some(mem_mb);
                }
                None => {
                    rt.cpu = None;
                    rt.mem_mb = None;
                }
            }

            // 2) 崩溃检测：托管进程意外退出（try_wait 非阻塞，可在锁内）
            if rt.spec.driver == DriverKind::Command {
                let mut exited: Option<i32> = None;
                if let Some(live) = rt.live.as_mut() {
                    if let Ok(Some(status)) = live.child.try_wait() {
                        exited = Some(status.code().unwrap_or(-1));
                    }
                } else if let Some(pid) = rt.adopted.as_ref().map(|a| a.pid) {
                    // 接管的进程没有 Child 句柄，用本轮已刷新的内核快照判存活
                    if sys.process(Pid::from_u32(pid)).is_none() {
                        exited = Some(-1);
                    }
                }
                if let Some(code) = exited {
                    let prev = rt.state;
                    #[cfg(unix)]
                    if let Some(live) = rt.live.as_mut() {
                        crate::manager::kill_guard(live);
                        // 与 Windows 的 KILL_ON_JOB_CLOSE 对齐：直接子进程已死时，
                        // 同组残留（如 npm 下面的 vite）也一并清掉，不等看门狗择机补刀
                        crate::drivers::command::kill_group(live.pid, true);
                    }
                    rt.live = None; // drop 触发 Job 回收
                    rt.adopted = None;
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
            //    Running 和 Unhealthy 都探测：Unhealthy 需要继续探测以便恢复回 Running，
            //    否则一旦进入 Unhealthy 就再也不探、永远无法自愈。
            if matches!(rt.state, State::Running | State::Unhealthy)
                && rt.spec.healthcheck.kind != crate::types::HealthKind::None
                && now >= rt.next_health_at
            {
                health_jobs.push((id.clone(), rt.spec.healthcheck.clone()));
                // 夹到合理上限并 saturating，避免用户填超大间隔在 debug 下 *1000 溢出 panic
                let every_ms = (rt.spec.healthcheck.interval_secs.clamp(1, 86_400) as i64)
                    .saturating_mul(1000);
                rt.next_health_at = now.saturating_add(every_ms);
            }
        }
    }

    // —— Phase B：锁外探测，绝不持锁做网络/子进程 I/O ——
    // 分块并发：单批最多 PROBE_CONCURRENCY 个线程。每个探测都要起子进程（sc / docker）
    // 或做网络握手，服务多时若一 tick 全量并发，线程与子进程能瞬间堆出上百个。
    // 探测线程 panic 时该 id 直接缺席：外部状态视为未知（不误判崩溃）、
    // 健康结果视为未探测（不累加 unhealthy_streak）。
    let mut external_results: Vec<(String, ExtStatus)> = Vec::new();
    for chunk in external_jobs.chunks(PROBE_CONCURRENCY) {
        let part: Vec<(String, ExtStatus)> = thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|(id, driver, target)| {
                    s.spawn(move || {
                        let st = match driver {
                            DriverKind::WindowsService => {
                                crate::drivers::winservice::status_full(target)
                            }
                            DriverKind::Docker => crate::drivers::docker::status_full(target),
                            _ => ExtStatus::default(),
                        };
                        (id.clone(), st)
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

    let ext_map: HashMap<String, ExtStatus> = external_results.into_iter().collect();
    let health_map: HashMap<String, Result<(), String>> = health_results.into_iter().collect();

    // —— Phase C：回锁应用结果 + 排期重启（探测期间状态可能被用户改动，需重新校验）——
    {
        let mut rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
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
                && ext_map.get(id).and_then(|s| s.running) == Some(false)
            {
                let prev = rt.state;
                rt.state = State::Crashed;
                rt.ext_pid = None;
                rt.ext_metrics = None;
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

            // 探测回来的宿主进程号/指标交给下一拍 Phase A 采样（这里不做任何阻塞 I/O）
            if let Some(st) = ext_map.get(id) {
                let alive = st.running == Some(true);
                rt.ext_pid = alive.then_some(st.pid).flatten();
                rt.ext_metrics = alive.then_some(st.metrics).flatten();
            }

            // 健康检查结果（Running 或 Unhealthy 都应用；Unhealthy 恢复则回到 Running）
            if matches!(rt.state, State::Running | State::Unhealthy) {
                if let Some(res) = health_map.get(id) {
                    let threshold = rt.spec.healthcheck.unhealthy_threshold;
                    match res {
                        Ok(()) => {
                            rt.unhealthy_streak = 0;
                            if rt.state == State::Unhealthy {
                                let prev = rt.state;
                                rt.state = State::Running;
                                rt.next_attempt_at = 0; // 撤销可能排上的重启
                                events.push(StateEvent {
                                    id: id.clone(),
                                    from: Some(prev),
                                    state: State::Running,
                                    pid: rt.observed_pid(),
                                    error: None,
                                    exit_code: None,
                                });
                            }
                        }
                        Err(_) => {
                            rt.unhealthy_streak += 1;
                            if rt.unhealthy_streak >= threshold && rt.state != State::Unhealthy {
                                let prev = rt.state;
                                rt.state = State::Unhealthy;
                                events.push(StateEvent {
                                    id: id.clone(),
                                    from: Some(prev),
                                    state: State::Unhealthy,
                                    pid: rt.observed_pid(),
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

            // 到达退避时间 -> 执行重启（含 Unhealthy：健康检查失败触发的自愈）
            if matches!(
                rt.state,
                State::Crashed | State::Restarting | State::Unhealthy
            ) && rt.desired == Desired::Running
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
        // 「谁把它拉起来的」必须落到服务自己的日志里：mac 上反馈「停止后自己又起来了」，
        // 光看状态变 Running 分不清是自动重启、启动自启还是用户点的
        let why = {
            let rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
            rts.get(&id).map(|r| {
                format!(
                    "[panel] 自动重启：退避到期（第 {} 次尝试，上次原因：{}）",
                    r.fail_streak,
                    r.last_error.as_deref().unwrap_or("未记录")
                )
            })
        };
        if let Some(why) = why {
            manager::log_sys(state, &id, &why);
        }
        if let Err(e) = manager::start(state, &id) {
            // 启动失败（多为“该服务正在执行其它操作”锁占用）：重新排期，别卡死不再拉
            let now = now_ms();
            let mut rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
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
            let backoff = base.saturating_mul(1i64 << shift).min(60);
            rt.next_attempt_at = now.saturating_add(backoff * 1000);
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

/// 「一个进程承载多个服务/程序」的宿主名，按它们的占用记账会把别人算进来，宁可不报：
/// - `svchost.exe` / `services.exe`：一组系统服务共用一个进程；
/// - `conhost.exe`：Windows 给控制台进程配的窗口宿主，可被多个进程共用。
///
/// 判据用进程名而不是服务的 `dwServiceType`：实测那个位不可信
/// （独占进程的 MySQL84/Spooler 也置位，RpcSs 反而不置位）。
fn is_shared_host_name(name: &str) -> bool {
    matches!(name, "svchost.exe" | "services.exe" | "conhost.exe")
}

/// 该进程是不是「一个进程承载多个服务」的宿主。
fn is_shared_host(sys: &System, pid: u32) -> bool {
    match sys.process(Pid::from_u32(pid)) {
        Some(p) => is_shared_host_name(&p.name().to_lowercase()),
        // 进程都不在了，交给 sample_tree 那边一并判掉
        None => false,
    }
}

/// 采样根进程及其**整棵进程树**（BFS 含孙进程）的 CPU/内存之和。
/// Node/pm2 的 worker 常是孙进程，只数直接子进程会明显少报。
/// 根进程不在本轮快照里（已退出，或如 macOS 上的容器进程那样压根不在本机进程表中）
/// 时返回 None：报「测不到」比报 0 诚实。
fn sample_tree(
    sys: &System,
    children_map: &HashMap<Pid, Vec<Pid>>,
    root_pid: u32,
) -> Option<(f32, f32)> {
    let root = Pid::from_u32(root_pid);
    sys.process(root)?;
    let mut cpu = 0.0f32;
    let mut mem_bytes = 0u64;
    let mut stack = vec![root];
    let mut seen: std::collections::HashSet<Pid> = std::collections::HashSet::new();
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue; // 防御环/重复
        }
        if let Some(p) = sys.process(pid) {
            // 窗口宿主 / 服务宿主这类共用进程不算进本服务的账
            if !is_shared_host_name(&p.name().to_lowercase()) {
                cpu += p.cpu_usage();
                mem_bytes += p.memory();
            }
        }
        if let Some(kids) = children_map.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }
    Some((cpu, mem_bytes as f32 / (1024.0 * 1024.0)))
}

#[cfg(test)]
mod probe_schedule {
    use super::tick;
    use crate::state::{AppState, Runtime};
    use crate::types::{Desired, DriverKind, HealthCheck, HealthKind, ServiceSpec, State};
    use std::sync::Arc;
    use sysinfo::System;

    fn state_with(spec: ServiceSpec) -> Arc<AppState> {
        let state = Arc::new(AppState::new(
            rusqlite::Connection::open_in_memory().unwrap(),
            std::env::temp_dir(),
        ));
        state
            .runtimes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                "h".to_string(),
                Runtime {
                    state: State::Running,
                    desired: Desired::Running,
                    ..Runtime::new(spec)
                },
            );
        state
    }

    fn streak_and_next(state: &Arc<AppState>) -> (u32, i64) {
        let rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
        let rt = rts.get("h").unwrap();
        (rt.unhealthy_streak, rt.next_health_at)
    }

    /// 健康探测按「间隔(秒)」排期：间隔设 1 小时时，连续两拍只该探一次。
    /// 用 Command 驱动 + 不 spawn 真实进程，是为了避开真实服务是否存在的机器差异；
    /// 探测目标是关着的端口，必然失败，于是「探了几次」直接体现在 unhealthy_streak 上。
    #[test]
    fn probe_honours_interval_secs() {
        let spec = ServiceSpec {
            driver: DriverKind::Command,
            healthcheck: HealthCheck {
                kind: HealthKind::Tcp,
                target: "127.0.0.1:1".to_string(),
                interval_secs: 3600,
                unhealthy_threshold: 3,
                ..HealthCheck::default()
            },
            ..ServiceSpec::default()
        };
        let state = state_with(spec);

        let mut sys = System::new_all();
        tick(&state, &sys);
        let (after_first, scheduled) = streak_and_next(&state);
        assert_eq!(after_first, 1, "第一拍没探：健康检查链路断了");
        assert!(
            scheduled > chrono::Utc::now().timestamp_millis(),
            "探测没排到下个间隔，interval_secs 仍然没生效"
        );

        sys.refresh_processes();
        tick(&state, &sys);
        tick(&state, &sys);
        let (after_more, _) = streak_and_next(&state);
        assert_eq!(
            after_more, after_first,
            "间隔内又探了 {after_more} 次（应为 1 次）"
        );
    }

    /// 间隔填 0 不能变成「每 tick 狂探」，至少隔一秒。
    #[test]
    fn zero_interval_falls_back_to_one_second() {
        let spec = ServiceSpec {
            driver: DriverKind::Command,
            healthcheck: HealthCheck {
                kind: HealthKind::Tcp,
                target: "127.0.0.1:1".to_string(),
                interval_secs: 0,
                unhealthy_threshold: 9,
                ..HealthCheck::default()
            },
            ..ServiceSpec::default()
        };
        let state = state_with(spec);
        let sys = System::new_all();
        // 起点时间必须在 tick 之前取：探测本身要花时间，之后再读 now 会显得
        // 「排到了过去」，那是测试的时钟取错了，不是排期错了
        let t0 = chrono::Utc::now().timestamp_millis();
        tick(&state, &sys);
        let (_streak, scheduled) = streak_and_next(&state);
        let waited = scheduled - t0;
        assert!(
            (1000..=5000).contains(&waited),
            "间隔 0 应排到约 1 秒后，实际排到 tick 起点后 {waited}ms"
        );
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::{build_children_map, is_shared_host, sample_tree, tick};
    use std::sync::Arc;
    use sysinfo::System;

    /// 端到端：系统服务探测回来的宿主 PID 真能采出内存占用。
    /// 这条链路以前只对面板自己 spawn 的进程生效，外部服务因此恒显示 0。
    #[test]
    fn external_service_host_yields_real_metrics() {
        let st = crate::drivers::winservice::status_full("Spooler");
        let Some(pid) = st.pid else {
            eprintln!("Spooler 未运行（部分 Windows 上它是手动启动），跳过");
            return;
        };
        let mut sys = System::new_all();
        sys.refresh_processes();
        let map = build_children_map(&sys);
        assert!(!is_shared_host(&sys, pid), "spoolsv 不是共享宿主，被误判了");
        let (_cpu, mem_mb) = sample_tree(&sys, &map, pid).expect("宿主进程应在进程表里");
        // CPU 只在服务真干活时才非 0，空闲时按 0 算是对的；内存不可能为 0
        assert!(mem_mb > 0.0, "内存采出 {mem_mb}MB，采样链路没生效");
    }

    /// 走两拍真实 tick：外部服务应从「探测回传 PID」一路走到「采样出内存」。
    /// 只测函数不够——以前断掉的正是 Phase C 写 ext_pid、Phase A 读它这一环。
    #[test]
    fn tick_samples_external_service_across_phases() {
        use crate::state::{AppState, Runtime};
        use crate::types::{Desired, DriverKind, ServiceSpec, State};

        let probe = crate::drivers::winservice::status_full("Spooler");
        if probe.running != Some(true) {
            eprintln!("Spooler 未运行，跳过 tick 验证");
            return;
        }
        let spec = ServiceSpec {
            driver: DriverKind::WindowsService,
            service_name: "Spooler".to_string(),
            ..ServiceSpec::default()
        };
        let state = Arc::new(AppState::new(
            rusqlite::Connection::open_in_memory().unwrap(),
            std::env::temp_dir(),
        ));
        let rt = Runtime {
            state: State::Running,
            desired: Desired::Running,
            ..Runtime::new(spec)
        };
        state
            .runtimes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert("sp".into(), rt);

        let mut sys = System::new_all();
        tick(&state, &sys); // 第一拍：探测回传宿主 PID
        sys.refresh_processes();
        tick(&state, &sys); // 第二拍：按 PID 采样

        let rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
        let rt = rts.get("sp").unwrap();
        assert_eq!(rt.ext_pid, probe.pid, "Phase C 没把探测回来的 PID 存下");
        assert!(
            rt.mem_mb.unwrap_or(0.0) > 0.0,
            "两拍之后仍没采到内存：cpu={:?} mem={:?}",
            rt.cpu,
            rt.mem_mb
        );
    }
}
