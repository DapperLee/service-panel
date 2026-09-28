use crate::db;
use crate::manager;
use crate::state::AppState;
use crate::types::{Desired, ServiceInput, ServiceView, State};
use std::sync::Arc;
use tauri_plugin_autostart::ManagerExt;

#[cfg(windows)]
use crate::elevate;

#[tauri::command]
pub fn list_services(state: tauri::State<'_, Arc<AppState>>) -> Result<Vec<ServiceView>, String> {
    // 静态列走 DB（不解密），spec + 动态状态走内存 Runtime（启动时已解密缓存）。
    // 避免每 2.5s 轮询都对全部服务做一次 DPAPI 解密。
    let statics = {
        let conn = state.db.lock().unwrap();
        db::list_static(&conn).map_err(|e| e.to_string())?
    };
    let rts = state.runtimes.lock().unwrap();
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
                cpu: rt.map(|r| r.cpu).unwrap_or(0.0),
                mem_mb: rt.map(|r| r.mem_mb).unwrap_or(0.0),
                last_error: rt.and_then(|r| r.last_error.clone()),
                config_ok: rt.map(|r| r.config_ok).unwrap_or(true),
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
        let conn = state.db.lock().unwrap();
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
        let conn = state.db.lock().unwrap();
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
            let conn = st.db.lock().unwrap();
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
    let conn = state.db.lock().unwrap();
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
            let conn = state.db.lock().unwrap();
            db::rename_group(&conn, &gid, &name).map_err(|e| e.to_string())?;
            gid
        }
        None => {
            let gid = uuid::Uuid::new_v4().to_string();
            let conn = state.db.lock().unwrap();
            db::create_group(&conn, &gid, &name).map_err(|e| e.to_string())?;
            gid
        }
    };
    Ok(gid)
}

#[tauri::command]
pub fn delete_group(state: tauri::State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    let conn = state.db.lock().unwrap();
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
    let conn = state.db.lock().unwrap();
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
        std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!("Start-Process -FilePath '{exe_str}' -Verb RunAs"),
            ])
            .spawn()
            .map_err(|e| format!("启动失败: {e}"))?;
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
