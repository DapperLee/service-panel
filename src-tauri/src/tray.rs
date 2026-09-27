use crate::manager;
use crate::state::AppState;
use crate::types::{DriverKind, ServiceSpec, State};
use std::sync::Arc;
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

pub const TRAY_ID: &str = "main-tray";

fn spec_name(spec: &ServiceSpec) -> String {
    match spec.driver {
        DriverKind::Command => {
            if spec.command.trim().is_empty() {
                "(未配置命令)".to_string()
            } else {
                spec.command.clone()
            }
        }
        DriverKind::WindowsService => spec.service_name.clone(),
        DriverKind::Docker => spec.container.clone(),
    }
}

fn build_menu(app: &AppHandle, state: &Arc<AppState>) -> tauri::Result<Menu<Wry>> {
    let show = MenuItem::with_id(app, "show", "打开面板", true, None::<&str>)?;
    let start_all = MenuItem::with_id(app, "start_all", "▶ 全部启动", true, None::<&str>)?;
    let stop_all = MenuItem::with_id(app, "stop_all", "■ 全部停止", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;

    // 每个服务一项：点击 = 启停切换
    let mut svc_items: Vec<MenuItem<Wry>> = Vec::new();
    let mut svc_names: Vec<(String, String, bool)> = {
        let rts = state.runtimes.lock().unwrap();
        rts.iter()
            .map(|(id, r)| {
                let name = spec_name(&r.spec);
                let running = r.state == State::Running;
                (id.clone(), name, running)
            })
            .collect()
    };
    svc_names.sort_by(|a, b| a.1.cmp(&b.1));
    for (id, name, running) in svc_items_source(&svc_names) {
        let label = format!("{} {}", if running { "●" } else { "○" }, name);
        let item = MenuItem::with_id(app, format!("svc::{}", id), label, true, None::<&str>)?;
        svc_items.push(item);
    }

    let sep2 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;

    let mut items: Vec<&dyn IsMenuItem<Wry>> = Vec::new();
    items.push(&show);
    items.push(&start_all);
    items.push(&stop_all);
    items.push(&sep1);
    for it in &svc_items {
        items.push(it);
    }
    items.push(&sep2);
    items.push(&quit);

    Menu::with_items(app, &items)
}

// 仅为可读性：把三元组转成迭代
fn svc_items_source(v: &[(String, String, bool)]) -> Vec<(String, String, bool)> {
    v.to_vec()
}

pub fn refresh(app: &AppHandle, state: &Arc<AppState>) {
    if let Ok(menu) = build_menu(app, state) {
        if let Some(tray) = app.tray_by_id(TRAY_ID) {
            let _ = tray.set_menu(Some(menu));
        }
    }
}

fn toggle_service(app: &AppHandle, state: &Arc<AppState>, sid: &str) {
    let running = state
        .runtimes
        .lock()
        .unwrap()
        .get(sid)
        .map(|r| r.state == State::Running)
        .unwrap_or(false);
    if running {
        let _ = manager::stop(state, sid);
    } else {
        let _ = manager::start(state, sid);
    }
    refresh(app, state);
}

fn handle_menu(app: &AppHandle, state: &Arc<AppState>, id: &str) {
    match id {
        "show" => {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }
        "start_all" => {
            let ids: Vec<String> = state.runtimes.lock().unwrap().keys().cloned().collect();
            for sid in ids {
                let _ = manager::start(state, &sid);
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            refresh(app, state);
        }
        "stop_all" => {
            let ids: Vec<String> = state.runtimes.lock().unwrap().keys().cloned().collect();
            for sid in ids {
                let _ = manager::stop(state, &sid);
            }
            refresh(app, state);
        }
        "quit" => {
            // 真正退出：绕过 close-to-tray
            app.exit(0);
        }
        s => {
            if let Some(sid) = s.strip_prefix("svc::") {
                toggle_service(app, state, sid);
            }
        }
    }
}

pub fn install(app: &AppHandle, state: Arc<AppState>) -> tauri::Result<()> {
    let menu = build_menu(app, &state)?;
    // 构建期已注入 bundle 图标，default_window_icon 必然存在
    let icon = app
        .default_window_icon()
        .cloned()
        .expect("缺少应用图标（应由 generate_context 注入）");

    let app_handle = app.clone();
    TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .icon(icon)
        .tooltip("本地服务管理面板")
        .on_menu_event(move |_app, event| {
            handle_menu(&app_handle, &state, event.id().as_ref());
        })
        .on_tray_icon_event(|tray, event| {
            // 左键点击：切换主窗口显示/隐藏
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if let Some(w) = tray.app_handle().get_webview_window("main") {
                    if w.is_visible().unwrap_or(true) {
                        let _ = w.hide();
                    } else {
                        let _ = w.show();
                        let _ = w.unminimize();
                        let _ = w.set_focus();
                    }
                }
            }
        })
        .build(app)?;
    Ok(())
}
