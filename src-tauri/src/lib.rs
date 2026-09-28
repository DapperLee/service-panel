mod commands;
mod db;
mod drivers;
#[cfg(windows)]
mod elevate;
mod export;
mod group;
mod health;
mod logs;
mod logsrc;
mod manager;
mod secrets;
mod state;
mod supervisor;
mod tray;
mod types;
#[cfg(windows)]
mod winjob;

use state::{AppState, StateEvent};
use std::sync::Arc;
use tauri::{Emitter, Manager};

/// 是否由开机自启触发（autostart 插件会往启动参数里追加 --autostart）
fn launched_by_autostart() -> bool {
    std::env::args().any(|a| a == "--autostart")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 单实例：开机自启 + 用户手动再开时，只唤起已有窗口，不再起第二个面板
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        // 开机自启：注册到系统启动项，附带 --autostart 参数
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
        // 原生文件对话框（导出/导入配置用）
        .plugin(tauri_plugin_dialog::init())
        // 关闭按钮 -> 隐藏到托盘，而不是退出
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            let app_handle = app.handle().clone();

            // 数据目录：%APPDATA%/com.local.servicepanel
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir).ok();
            let db_dir = data_dir.join("data");
            let log_root = data_dir.join("logs");

            let conn = db::open(&db_dir).map_err(|e| format!("数据库初始化失败: {e}"))?;
            let state = Arc::new(AppState::new(conn, log_root));

            // 日志批量缓冲：emitter 只入队（队列有界，满了只丢 UI 流并计数、磁盘仍完整），
            // flusher 线程每 80ms 聚合成数组一次性 emit，避免话痨服务每秒上千次 IPC
            // 以及单批无上限撑爆 webview 消息。
            let log_buf: Arc<std::sync::Mutex<logs::LogQueue>> =
                Arc::new(std::sync::Mutex::new(logs::LogQueue::new(2000)));
            {
                let buf = log_buf.clone();
                let emitter: logs::LogEmitter = Arc::new(move |ev| {
                    let _ = buf.lock().unwrap().push(ev);
                });
                *state.emitter.lock().unwrap() = Some(emitter);
            }
            {
                let handle = app_handle.clone();
                let buf = log_buf.clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(80));
                    let batch = {
                        let mut q = buf.lock().unwrap();
                        if q.is_empty() {
                            continue;
                        }
                        q.take_batch()
                    };
                    let _ = handle.emit("log://batch", &batch);
                });
            }

            // 状态变化 channel：监督/管理线程发事件 -> 消费者线程 emit 给前端 + 落库 + 刷新托盘
            let (tx, rx) = std::sync::mpsc::channel::<StateEvent>();
            *state.state_tx.lock().unwrap() = Some(tx);
            {
                let handle = app_handle.clone();
                let db = state.db.clone();
                let tray_state = state.clone();
                let mut count = 0u32;
                std::thread::spawn(move || {
                    for ev in rx {
                        {
                            let conn = db.lock().unwrap();
                            db::record_event(
                                &conn,
                                &ev.id,
                                ev.from.map(|s| format!("{:?}", s).to_lowercase()).as_deref(),
                                &format!("{:?}", ev.state).to_lowercase(),
                                ev.error.as_deref(),
                                ev.exit_code,
                            );
                        }
                        // 每 500 条事件顺带做一次过期清理，防 flapping 撑爆
                        count += 1;
                        if count % 500 == 0 {
                            let conn = db.lock().unwrap();
                            db::prune_events(&conn, 7, 5000);
                        }
                        let payload = serde_json::json!({
                            "id": ev.id,
                            "from": ev.from.map(|s| format!("{:?}", s).to_lowercase()),
                            "state": ev.state,
                            "pid": ev.pid,
                            "error": ev.error,
                        });
                        let _ = handle.emit("state://change", &payload);
                        // 状态变化同步刷新托盘菜单的服务启停点
                        tray::refresh(&handle, &tray_state);
                    }
                });
            }

            // 把 DB 里的服务载入内存注册表
            let views = {
                let conn = state.db.lock().unwrap();
                db::list_services(&conn).unwrap_or_default()
            };
            for v in views {
                manager::ensure_runtime(&state, &v.id, v.spec, v.config_ok);
            }

            // 启动清理：过期事件 + 超保留天数的日志目录
            {
                let conn = state.db.lock().unwrap();
                db::prune_events(&conn, 7, 5000);
            }
            logs::cleanup_old_logs(&state.log_root, 7);

            // 系统托盘常驻
            if let Err(e) = tray::install(&app_handle, state.clone()) {
                eprintln!("托盘初始化失败: {e}");
            }

            // 监督线程（含启动接管与自启）
            supervisor::spawn_supervisor(state.clone());

            // 开机自启启动时：不弹主窗口，直接躲在托盘后面
            if launched_by_autostart() {
                if let Some(w) = app_handle.get_webview_window("main") {
                    let _ = w.hide();
                }
            }

            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_services,
            commands::add_service,
            commands::update_service,
            commands::delete_service,
            commands::start_service,
            commands::stop_service,
            commands::restart_service,
            commands::start_all,
            commands::stop_all,
            commands::recent_events,
            commands::list_groups,
            commands::save_group,
            commands::delete_group,
            commands::group_order,
            commands::start_group,
            commands::stop_group,
            commands::get_autostart,
            commands::set_autostart,
            commands::quit_app,
            commands::is_admin,
            commands::restart_as_admin,
            commands::export_config,
            commands::import_config,
        ])
        .run(tauri::generate_context!())
        .expect("运行 服务面板 失败");
}
