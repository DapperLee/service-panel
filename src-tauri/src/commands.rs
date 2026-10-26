use crate::db;
use crate::manager;
use crate::state::AppState;
use crate::types::{Desired, ServiceInput, ServiceView, State, TaskInput};
use std::sync::Arc;
use tauri_plugin_autostart::ManagerExt;

#[cfg(windows)]
use crate::elevate;

#[tauri::command]
pub fn list_services(state: tauri::State<'_, Arc<AppState>>) -> Result<Vec<ServiceView>, String> {
    // 静态列走 DB（不解密），spec + 动态状态走内存 Runtime（启动时已解密缓存）。
    // 避免每 2.5s 轮询都对全部服务做一次 DPAPI 解密。
    let statics = {
        let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
        db::list_static(&conn).map_err(|e| e.to_string())?
    };
    let rts = state.runtimes.lock().unwrap_or_else(|e| e.into_inner());
    let views = statics
        .into_iter()
        .map(|(id, name, group_id, sort_order, enabled)| {
            let rt = rts.get(&id);
            ServiceView {
                id,
                name,
                group_id,
                sort_order,
                enabled,
                spec: rt.map(|r| r.spec.clone()).unwrap_or_default(),
                state: rt.map(|r| r.state).unwrap_or(State::Stopped),
                desired: rt.map(|r| r.desired).unwrap_or(Desired::Stopped),
                pid: rt.and_then(|r| r.observed_pid()),
                cpu: rt.and_then(|r| r.cpu),
                mem_mb: rt.and_then(|r| r.mem_mb),
                last_error: rt.and_then(|r| r.last_error.clone()),
                config_ok: rt.map(|r| r.config_ok).unwrap_or(true),
                chosen_port: rt.and_then(|r| r.chosen_port),
            }
        })
        .collect();
    Ok(views)
}

#[tauri::command]
pub fn add_service(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    input: ServiceInput,
) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    {
        let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
        db::insert_service(
            &conn,
            &id,
            &input.name,
            input.group_id.as_deref(),
            input.sort_order,
            input.enabled,
            &input.spec,
        )
        .map_err(|e| e.to_string())?;
    }
    manager::ensure_runtime(state.inner(), &id, input.spec, true);
    crate::tray::refresh(&app, state.inner());
    Ok(id)
}

#[tauri::command]
pub fn update_service(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    input: ServiceInput,
) -> Result<(), String> {
    let id = input.id.clone().ok_or("缺少 id")?;
    {
        let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
        db::update_service(
            &conn,
            &id,
            &input.name,
            input.group_id.as_deref(),
            input.sort_order,
            input.enabled,
            &input.spec,
        )
        .map_err(|e| e.to_string())?;
    }
    manager::ensure_runtime(state.inner(), &id, input.spec, true);
    crate::tray::refresh(&app, state.inner());
    Ok(())
}

#[tauri::command]
pub async fn delete_service(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    let st = state.inner().clone();
    let app2 = app.clone();
    let id2 = id.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let _ = manager::stop(&st, &id2);
        {
            let conn = st.db.lock().unwrap_or_else(|e| e.into_inner());
            db::delete_service(&conn, &id2).map_err(|e| e.to_string())?;
        }
        manager::remove(&st, &id2);
        crate::tray::refresh(&app2, &st);
        Ok(())
    })
    .await
    .map_err(|e| format!("任务调度失败: {e}"))??;
    Ok(())
}

#[tauri::command]
pub async fn start_service(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager::start(&st, &id))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}

#[tauri::command]
pub async fn stop_service(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager::stop(&st, &id))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}

#[tauri::command]
pub async fn restart_service(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager::restart(&st, &id))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}

#[tauri::command]
pub fn start_all(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<(), String> {
    // 按全局拓扑定序，后台逐个「启动 + 就绪等待」，进度经事件回报
    crate::group::start_all_seq(&app, state.inner().clone())
}

#[tauri::command]
pub fn stop_all(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<(), String> {
    crate::group::stop_all_seq(&app, state.inner().clone())
}

// ———— 分组与依赖拓扑 ————

#[tauri::command]
pub fn list_groups(state: tauri::State<'_, Arc<AppState>>) -> Result<Vec<db::GroupRow>, String> {
    let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
    db::list_groups(&conn).map_err(|e| e.to_string())
}

/// 新建或重命名分组；返回分组 id
#[tauri::command]
pub fn save_group(
    state: tauri::State<'_, Arc<AppState>>,
    id: Option<String>,
    name: String,
) -> Result<String, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("分组名不能为空".to_string());
    }
    let gid = match id {
        Some(gid) => {
            let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
            db::rename_group(&conn, &gid, &name).map_err(|e| e.to_string())?;
            gid
        }
        None => {
            let gid = uuid::Uuid::new_v4().to_string();
            let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
            db::create_group(&conn, &gid, &name).map_err(|e| e.to_string())?;
            gid
        }
    };
    Ok(gid)
}

#[tauri::command]
pub fn delete_group(state: tauri::State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
    db::delete_group(&conn, &id).map_err(|e| e.to_string())
}

/// 返回某组拓扑顺序的服务 id 列表；有环则返回错误
#[tauri::command]
pub fn group_order(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<Vec<String>, String> {
    crate::group::ordered_members(state.inner(), &id)
        .map(|(order, _)| order)
        .map_err(|cycle| crate::group::cycle_err(&cycle))
}

#[tauri::command]
pub fn start_group(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    crate::group::start_group(&app, state.inner().clone(), id)
}

#[tauri::command]
pub fn stop_group(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    crate::group::stop_group(&app, state.inner().clone(), id)
}

#[tauri::command]
pub fn recent_events(
    state: tauri::State<'_, Arc<AppState>>,
    limit: i64,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
    let rows = db::recent_events(&conn, limit.max(1)).map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|(service_id, ts, from, to, reason, exit_code)| {
            serde_json::json!({
                "serviceId": service_id,
                "ts": ts,
                "from": from,
                "to": to,
                "reason": reason,
                "exitCode": exit_code,
            })
        })
        .collect())
}

/// 查询面板是否已注册开机自启
#[tauri::command]
pub fn get_autostart(app: tauri::AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

/// 开启/关闭面板开机自启
#[tauri::command]
pub fn set_autostart(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    let m = app.autolaunch();
    let r = if enabled { m.enable() } else { m.disable() };
    r.map_err(|e| e.to_string())
}

/// 从前端彻底退出应用（绕过 close-to-tray）
#[tauri::command]
pub fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

// ============================ 任务 ============================

#[tauri::command]
pub fn list_tasks(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<crate::types::TaskView>, String> {
    let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
    db::list_tasks(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_task(
    state: tauri::State<'_, Arc<AppState>>,
    input: TaskInput,
) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
    db::insert_task(
        &conn,
        &id,
        &input.name,
        input.group.as_deref(),
        &input.config,
    )
    .map_err(|e| e.to_string())?;
    Ok(id)
}

#[tauri::command]
pub fn update_task(state: tauri::State<'_, Arc<AppState>>, input: TaskInput) -> Result<(), String> {
    let id = input.id.clone().ok_or("缺少 id")?;
    let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
    db::update_task(
        &conn,
        &id,
        &input.name,
        input.group.as_deref(),
        &input.config,
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn delete_task(state: tauri::State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
    db::delete_task(&conn, &id).map_err(|e| e.to_string())
}

/// 排队运行一个任务。返回 run_id（也作为前端日志流的 key）。
#[tauri::command]
pub fn run_task(
    state: tauri::State<'_, Arc<AppState>>,
    task_id: String,
    variant_index: Option<usize>,
    extra_args: String,
) -> Result<String, String> {
    let task = {
        let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
        db::list_tasks(&conn)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|t| t.id == task_id)
    }
    .ok_or("任务不存在")?;
    if !task.config_ok {
        return Err("任务配置解析失败，无法运行".to_string());
    }
    let variant_label = match variant_index {
        Some(i) => task.config.variants.get(i).map(|v| v.label.clone()),
        None => None,
    };
    let parsed_extra: Vec<String> = extra_args
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    let run_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();
    let run = crate::types::TaskRun {
        id: run_id.clone(),
        task_id: task_id.clone(),
        task_name: task.name.clone(),
        variant: variant_label,
        variant_index,
        extra_args: extra_args.trim().to_string(),
        status: crate::types::TaskRunStatus::Queued,
        exit_code: None,
        started_at: now,
        finished_at: None,
        duration_ms: None,
        log_path: String::new(),
    };
    {
        let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
        db::insert_task_run(&conn, &run).map_err(|e| e.to_string())?;
        // 记住该任务上次选的变体，供「运行本组」沿用
        db::set_task_last_variant(&conn, &task_id, variant_index);
    }
    let req = crate::types::TaskRunRequest {
        run_id: run_id.clone(),
        name: task.name.clone(),
        config: task.config,
        variant_index,
        extra_args: parsed_extra,
    };
    {
        let tx = state.task_tx.lock().unwrap_or_else(|e| e.into_inner());
        match tx.as_ref() {
            Some(tx) => tx.send(req).map_err(|_| "任务队列不可用".to_string())?,
            None => return Err("任务队列未就绪".to_string()),
        }
    }
    Ok(run_id)
}

/// 取消任务运行：运行中 -> 置取消标志（调度器 kill 进程树）；排队中 -> 标记为已取消，
/// 调度器 pop 到它会跳过。
#[tauri::command]
pub fn cancel_task_run(
    state: tauri::State<'_, Arc<AppState>>,
    run_id: String,
) -> Result<(), String> {
    let flag = state
        .task_cancel_flags
        .lock()
        .unwrap()
        .get(run_id.as_str())
        .cloned();
    if let Some(f) = flag {
        f.store(true, std::sync::atomic::Ordering::SeqCst);
        return Ok(());
    }
    // 不在运行集合里：若仍是 queued，则取消排队
    let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
    match db::get_run_status(&conn, &run_id) {
        Some(crate::types::TaskRunStatus::Queued) => {
            db::finish_task_run(
                &conn,
                &run_id,
                crate::types::TaskRunStatus::Cancelled,
                None,
                chrono::Utc::now().timestamp(),
                0,
            );
            Ok(())
        }
        _ => Err("该运行已结束，无法取消".to_string()),
    }
}

/// 任务最大并发数（1=串行）
#[tauri::command]
pub fn get_task_max_parallel(state: tauri::State<'_, Arc<AppState>>) -> usize {
    state
        .task_max_parallel
        .load(std::sync::atomic::Ordering::SeqCst)
}

#[tauri::command]
pub fn set_task_max_parallel(
    state: tauri::State<'_, Arc<AppState>>,
    n: usize,
) -> Result<(), String> {
    let v = n.clamp(1, 16);
    state
        .task_max_parallel
        .store(v, std::sync::atomic::Ordering::SeqCst);
    let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
    db::set_setting(&conn, "task_max_parallel", &v.to_string());
    Ok(())
}

/// 读取某次运行的日志文件内容（回看历史运行；本次会话实时跑过的走内存流即可）
#[tauri::command]
pub fn read_task_log(
    state: tauri::State<'_, Arc<AppState>>,
    run_id: String,
) -> Result<Vec<crate::types::LogLine>, String> {
    let dir = {
        let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT log_path FROM task_runs WHERE id=?1",
            [&run_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .map_err(|_| "运行记录不存在".to_string())?
    };
    let dir = dir.ok_or("该运行没有日志文件".to_string())?;
    let dir = std::path::Path::new(&dir);
    // 取该运行目录下最新的 .log 文件
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("读日志目录失败: {e}"))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map(|n| n.contains(".log"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    let latest = files.last().ok_or("无日志文件".to_string())?;
    let content = std::fs::read_to_string(latest).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for line in content.lines() {
        // 落盘格式： "{ts} [{stream}] {text}"
        let sp = match line.find(' ') {
            Some(i) => i,
            None => continue,
        };
        let ts: i64 = line[..sp].parse().unwrap_or(0);
        let rest = &line[sp + 1..];
        // 落盘行可能因进程被强杀而在中途截断，切片一律用 get 防越界 panic
        let (stream, text) = match (rest.strip_prefix('['), rest.find(']')) {
            (Some(_), Some(end)) if end >= 1 => {
                let stream = rest.get(1..end).unwrap_or("stdout");
                // "]" 之后应有一个空格再接正文
                let text = rest.get(end + 2..).unwrap_or("").to_string();
                (stream, text)
            }
            _ => ("stdout", rest.to_string()),
        };
        out.push(crate::types::LogLine {
            ts,
            stream: stream.to_string(),
            text,
        });
    }
    Ok(out)
}

#[tauri::command]
pub fn list_task_runs(
    state: tauri::State<'_, Arc<AppState>>,
    limit: i64,
) -> Result<Vec<crate::types::TaskRun>, String> {
    let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
    db::list_task_runs(&conn, limit.max(1)).map_err(|e| e.to_string())
}

/// 在资源管理器打开目录
#[tauri::command]
pub fn open_in_explorer(path: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(&path).spawn();
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&path).spawn();
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(&path).spawn();
        Ok(())
    }
    #[cfg(not(any(windows, target_os = "macos", unix)))]
    {
        let _ = path;
        Err("不支持的平台".to_string())
    }
}

/// 在该目录打开系统终端
#[tauri::command]
pub fn open_in_terminal(path: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        // 优先 Windows Terminal，回退 cmd
        let wt = std::process::Command::new("wt").args(["-d", &path]).spawn();
        if wt.is_ok() {
            return Ok(());
        }
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "cmd", "/K", &format!("cd /d {}", path)])
            .spawn();
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .args(["-a", "Terminal", &path])
            .spawn();
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("x-terminal-emulator")
            .current_dir(&path)
            .spawn();
        Ok(())
    }
    #[cfg(not(any(windows, target_os = "macos", unix)))]
    {
        let _ = path;
        Err("不支持的平台".to_string())
    }
}

// ============================ 书签 ============================

#[tauri::command]
pub fn list_bookmarks(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<crate::types::Bookmark>, String> {
    let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
    db::list_bookmarks(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_bookmark(
    state: tauri::State<'_, Arc<AppState>>,
    name: String,
    path: String,
) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
    db::insert_bookmark(&conn, &id, &name, &path).map_err(|e| e.to_string())?;
    Ok(id)
}

#[tauri::command]
pub fn delete_bookmark(state: tauri::State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
    db::delete_bookmark(&conn, &id).map_err(|e| e.to_string())
}

/// 是否以管理员运行（非 Windows 恒返回 true，不影响功能）
#[tauri::command]
pub fn is_admin() -> bool {
    #[cfg(windows)]
    {
        elevate::is_elevated()
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// 以管理员权限重启面板（弹 UAC）。成功后退出当前实例。
#[tauri::command]
pub fn restart_as_admin(app: tauri::AppHandle) -> Result<(), String> {
    #[cfg(windows)]
    {
        // 开发版（debug）exe 加载的是 vite devUrl，裸启会因 dev server 不在而白屏/连接拒绝。
        // 提权请改用「管理员终端里跑 npm run tauri dev」，不要走这个按钮。
        if cfg!(debug_assertions) {
            return Err(
                "开发模式不支持一键提权重启：请以管理员身份打开终端，再运行 npm run tauri dev。"
                    .to_string(),
            );
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe_str = exe.to_string_lossy().replace('\'', "''"); // PowerShell 单引号转义
        let mut cmd = std::process::Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-Command",
            &format!("Start-Process -FilePath '{exe_str}' -Verb RunAs"),
        ]);
        crate::drivers::command::hide_window(&mut cmd);
        cmd.spawn().map_err(|e| format!("启动失败: {e}"))?;
        app.exit(0);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        Err("仅 Windows 支持提权重启".to_string())
    }
}

/// 导出配置到指定路径（明文 JSON）。返回导出的服务数量。
#[tauri::command]
pub fn export_config(
    state: tauri::State<'_, Arc<AppState>>,
    path: String,
) -> Result<usize, String> {
    crate::export::export_to_file(state.inner(), &path)
}

/// 从指定路径导入配置。replace=true 覆盖（先停所有服务再重建），false 合并。
#[tauri::command]
pub async fn import_config(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    path: String,
    replace: bool,
) -> Result<crate::export::ImportOutcome, String> {
    let st = state.inner().clone();
    let app2 = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        crate::export::import_from_file(&st, &path, replace)
    })
    .await
    .map_err(|e| format!("任务调度失败: {e}"))??;
    crate::tray::refresh(&app2, state.inner());
    Ok(outcome)
}

/// 端口冲突体检：服务声明的 TCP/HTTP 健康端口若被**别的**进程占着，列出来。
/// 判定考虑了启动器场景（npm 之类 fork 出真正监听的子进程），不会误报。
#[tauri::command]
pub async fn port_audit(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<crate::netdiag::PortAudit>, String> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || crate::netdiag::audit(&st))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))
}
